use crate::config::diff::diff_configs;
use crate::config::{load_config, validate};
use crate::runtime::app_state::AppState;
use crate::runtime::registry::ToolRegistry;
use crate::supervisor::service::ManagedService;
use anyhow::{Context, Result};
use notify_debouncer_mini::{DebounceEventResult, new_debouncer};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub async fn apply_reload(state: &Arc<AppState>, config_path: &PathBuf) -> Result<()> {
    tracing::info!("reload: reading config from {:?}", config_path);

    let new_config = load_config(Some(config_path)).context("reload: failed to load new config")?;
    validate(&new_config).context("reload: new config is invalid — keeping current state")?;

    let diff = {
        let old_config = state.current_config.read().await;
        diff_configs(&old_config, &new_config)
    };

    if diff.is_empty() {
        tracing::info!("reload: no changes detected");
        return Ok(());
    }

    tracing::info!("reload: {diff}");

    let supervisor_result = {
        let mut supervisor = state.supervisor.write().await;
        let mut started_services: Vec<String> = Vec::new();

        // Start new services
        for name in &diff.added_services {
            if let Some(svc_cfg) = new_config.services.iter().find(|s| s.name == *name) {
                supervisor
                    .services_mut()
                    .insert(name.clone(), ManagedService::new(svc_cfg.clone()));
                if let Err(e) = supervisor.start_service(name).await {
                    tracing::error!(service = %name, error = %e, "reload: failed to start new service");
                    for started in &started_services {
                        let _ = supervisor.stop_service(started).await;
                        supervisor.services_mut().remove(started);
                    }
                    anyhow::bail!("reload aborted: failed to start service '{name}': {e}");
                }
                started_services.push(name.clone());
            }
        }

        // Brief wait + health check for new services
        if !diff.added_services.is_empty() {
            drop(supervisor);
            tokio::time::sleep(Duration::from_millis(500)).await;
            supervisor = state.supervisor.write().await;
            supervisor.run_health_checks().await;
        }

        // Restart changed services
        for name in &diff.changed_services {
            tracing::info!(service = %name, "reload: restarting changed service");
            let _ = supervisor.stop_service(name).await;
            supervisor.services_mut().remove(name);

            if let Some(svc_cfg) = new_config.services.iter().find(|s| s.name == *name) {
                supervisor
                    .services_mut()
                    .insert(name.clone(), ManagedService::new(svc_cfg.clone()));
                if let Err(e) = supervisor.start_service(name).await {
                    tracing::error!(service = %name, error = %e, "reload: restart failed — service left stopped");
                }
            }
        }

        // Stop removed services
        for name in &diff.removed_services {
            tracing::info!(service = %name, "reload: stopping removed service");
            let _ = supervisor.stop_service(name).await;
            supervisor.services_mut().remove(name);
        }

        Ok::<(), anyhow::Error>(())
    };

    supervisor_result?;

    {
        let new_registry = ToolRegistry::from_config(&new_config);
        let mut registry = state.registry.write().await;
        *registry = new_registry;
        tracing::info!("reload: registry updated ({} tools)", registry.list().len());
    }

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
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
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

    let mut debouncer =
        new_debouncer(Duration::from_millis(500), std_tx).expect("failed to create file watcher");

    debouncer
        .watcher()
        .watch(&watch_path, notify::RecursiveMode::NonRecursive)
        .expect("failed to watch config file");

    tracing::info!(path = ?config_path, "file watcher enabled");

    std::thread::spawn(move || {
        let _debouncer = debouncer;
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
