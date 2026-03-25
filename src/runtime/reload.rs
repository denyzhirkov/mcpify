use crate::config::diff::diff_configs;
use crate::config::{load_config, validate};
use crate::runtime::app_state::AppState;
use crate::runtime::registry::ToolRegistry;
use crate::supervisor::child::ChildProcess;
use anyhow::{Context, Result};
use notify_debouncer_mini::{DebounceEventResult, new_debouncer};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub async fn apply_reload(state: &Arc<AppState>, config_path: &PathBuf) -> Result<()> {
    tracing::info!("reload: reading config from {:?}", config_path);

    // 1. Load and validate new config (before taking any locks)
    let new_config =
        load_config(Some(config_path)).context("reload: failed to load new config")?;
    validate(&new_config).context("reload: new config is invalid — keeping current state")?;

    // 2. Compute diff (short-lived read lock)
    let diff = {
        let old_config = state.current_config.read().await;
        diff_configs(&old_config, &new_config)
    };

    if diff.is_empty() {
        tracing::info!("reload: no changes detected");
        return Ok(());
    }

    tracing::info!("reload: {diff}");

    // 3-7. Apply all supervisor changes under a single write lock
    // This prevents partial state between steps.
    let supervisor_result = {
        let mut supervisor = state.supervisor.write().await;

        // Track what we successfully started so we can rollback on failure
        let mut started_children: Vec<String> = Vec::new();

        // Step 3: Start new children
        for name in &diff.added_children {
            if let Some(child_cfg) = new_config.children.iter().find(|c| c.name == *name) {
                supervisor
                    .children_mut()
                    .insert(name.clone(), ChildProcess::new(child_cfg.clone()));
                if let Err(e) = supervisor.start_child(name).await {
                    tracing::error!(child = %name, error = %e, "reload: failed to start new child");
                    // Rollback: stop children we already started in this reload
                    for started in &started_children {
                        let _ = supervisor.stop_child(started).await;
                        supervisor.children_mut().remove(started);
                    }
                    anyhow::bail!("reload aborted: failed to start child '{name}': {e}");
                }
                started_children.push(name.clone());
            }
        }

        // Step 4: Brief wait + health check for new children
        if !diff.added_children.is_empty() {
            // Release lock briefly to allow health check HTTP requests
            drop(supervisor);
            tokio::time::sleep(Duration::from_millis(500)).await;
            supervisor = state.supervisor.write().await;
            supervisor.run_health_checks().await;
        }

        // Step 5: Restart changed children (stop old, start new)
        for name in &diff.changed_children {
            tracing::info!(child = %name, "reload: restarting changed child");
            let _ = supervisor.stop_child(name).await;
            supervisor.children_mut().remove(name);

            if let Some(child_cfg) = new_config.children.iter().find(|c| c.name == *name) {
                supervisor
                    .children_mut()
                    .insert(name.clone(), ChildProcess::new(child_cfg.clone()));
                if let Err(e) = supervisor.start_child(name).await {
                    tracing::error!(child = %name, error = %e, "reload: restart failed — child left stopped");
                }
            }
        }

        // Step 6: Stop removed children
        for name in &diff.removed_children {
            tracing::info!(child = %name, "reload: stopping removed child");
            let _ = supervisor.stop_child(name).await;
            supervisor.children_mut().remove(name);
        }

        Ok::<(), anyhow::Error>(())
    };

    // If supervisor changes failed, don't update registry or config
    supervisor_result?;

    // Step 7: Update tool registry
    {
        let new_registry = ToolRegistry::from_config(&new_config);
        let mut registry = state.registry.write().await;
        *registry = new_registry;
        tracing::info!("reload: registry updated ({} tools)", registry.list().len());
    }

    // Step 8: Update stored config and generation (only on full success)
    {
        let mut config = state.current_config.write().await;
        *config = new_config;
    }
    let generation = state.generation.fetch_add(1, Ordering::Relaxed) + 1;
    tracing::info!(generation, "reload: complete");

    Ok(())
}

#[cfg(unix)]
pub fn spawn_signal_handler(state: Arc<AppState>, config_path: PathBuf) {
    tokio::spawn(async move {
        let mut signal =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
                .expect("failed to register SIGHUP handler");
        loop {
            signal.recv().await;
            tracing::info!("received SIGHUP, triggering reload");
            if let Err(e) = apply_reload(&state, &config_path).await {
                tracing::error!(error = %e, "reload failed");
            }
        }
    });
}

pub fn spawn_file_watcher(state: Arc<AppState>, config_path: PathBuf) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

    let sync_tx = tx.clone();
    let watch_path = config_path.clone();
    let (std_tx, std_rx) = std::sync::mpsc::channel::<DebounceEventResult>();

    let mut debouncer = new_debouncer(Duration::from_millis(500), std_tx)
        .expect("failed to create file watcher");

    debouncer
        .watcher()
        .watch(&watch_path, notify::RecursiveMode::NonRecursive)
        .expect("failed to watch config file");

    tracing::info!(path = ?config_path, "file watcher enabled");

    // Bridge thread: std mpsc → tokio mpsc
    std::thread::spawn(move || {
        let _debouncer = debouncer; // keep alive
        while let Ok(event) = std_rx.recv() {
            match event {
                Ok(_events) => {
                    let _ = sync_tx.blocking_send(());
                }
                Err(errs) => {
                    eprintln!("file watcher errors: {errs:?}");
                }
            }
        }
    });

    tokio::spawn(async move {
        while rx.recv().await.is_some() {
            tracing::info!("config file changed, triggering reload");
            if let Err(e) = apply_reload(&state, &config_path).await {
                tracing::error!(error = %e, "file-watch reload failed");
            }
        }
    });
}
