use crate::config::model::{McpifyConfig, RestartPolicy};
use crate::supervisor::child::{ChildProcess, ChildState};
use crate::supervisor::health::{self, HealthResult};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::process::Command;

const MAX_RESTARTS: u32 = 3;

pub struct SupervisorManager {
    children: HashMap<String, ChildProcess>,
    http_client: reqwest::Client,
    graceful_timeout: Duration,
}

impl SupervisorManager {
    pub fn from_config(config: &McpifyConfig) -> Self {
        let mut children = HashMap::new();
        for child_config in &config.children {
            children.insert(
                child_config.name.clone(),
                ChildProcess::new(child_config.clone()),
            );
        }
        Self {
            children,
            http_client: reqwest::Client::new(),
            graceful_timeout: Duration::from_millis(config.supervisor.graceful_shutdown_timeout_ms),
        }
    }

    pub async fn start_all(&mut self) -> Result<()> {
        let names: Vec<String> = self
            .children
            .iter()
            .filter(|(_, c)| c.config.autostart)
            .map(|(name, _)| name.clone())
            .collect();

        for name in names {
            if let Err(e) = self.start_child(&name).await {
                tracing::error!(child = %name, error = %e, "failed to start child");
            }
        }
        Ok(())
    }

    pub async fn start_child(&mut self, name: &str) -> Result<()> {
        let child = self
            .children
            .get_mut(name)
            .with_context(|| format!("child not found: {name}"))?;

        let mut cmd = Command::new(&child.config.command);
        cmd.args(&child.config.args);
        cmd.current_dir(&child.config.cwd);

        for (k, v) in &child.config.env {
            cmd.env(k, v);
        }

        // Don't inherit our stdio — children are background services
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::piped());

        let process = cmd.spawn().with_context(|| {
            format!("spawning child '{}': {} {}", name, child.config.command, child.config.args.join(" "))
        })?;

        let pid = process.id();
        child.pid = pid;
        child.handle = Some(process);
        child.state = ChildState::Starting;
        child.started_at = Some(Instant::now());
        child.restart_count = 0;

        tracing::info!(child = %name, pid = ?pid, "child started");
        Ok(())
    }

    pub async fn stop_child(&mut self, name: &str) -> Result<()> {
        let child = self
            .children
            .get_mut(name)
            .with_context(|| format!("child not found: {name}"))?;

        if let Some(handle) = &mut child.handle {
            // Try graceful shutdown (SIGTERM on Unix) via safe API
            #[cfg(unix)]
            if let Some(pid) = child.pid {
                if let Ok(raw_pid) = i32::try_from(pid) {
                    if raw_pid > 0 {
                        // nix provides a safe wrapper; fallback to libc with validated pid
                        let _ = nix::sys::signal::kill(
                            nix::unistd::Pid::from_raw(raw_pid),
                            nix::sys::signal::Signal::SIGTERM,
                        );
                    }
                }
            }

            // Wait for graceful shutdown with timeout
            let wait_result =
                tokio::time::timeout(self.graceful_timeout, handle.wait()).await;

            match wait_result {
                Ok(Ok(_)) => {
                    tracing::info!(child = %name, "child stopped gracefully");
                }
                _ => {
                    // Force kill via tokio handle (safe, no pid needed)
                    let _ = handle.kill().await;
                    tracing::warn!(child = %name, "child force killed");
                }
            }
        }

        child.handle = None;
        child.pid = None;
        child.state = ChildState::Stopped;
        Ok(())
    }

    pub async fn stop_all(&mut self) -> Result<()> {
        let names: Vec<String> = self.children.keys().cloned().collect();
        for name in names {
            if let Err(e) = self.stop_child(&name).await {
                tracing::error!(child = %name, error = %e, "failed to stop child");
            }
        }
        Ok(())
    }

    pub async fn run_health_checks(&mut self) {
        let names: Vec<String> = self.children.keys().cloned().collect();

        for name in names {
            let child = match self.children.get_mut(&name) {
                Some(c) => c,
                None => continue,
            };

            // Skip stopped children
            if child.state == ChildState::Stopped {
                continue;
            }

            let result = health::check_health(child, &self.http_client).await;
            child.last_health_check = Some(Instant::now());

            let old_state = child.state;
            match result {
                HealthResult::Healthy => {
                    if child.state != ChildState::Online {
                        child.state = ChildState::Online;
                        tracing::info!(child = %name, "child is online");
                    }
                }
                HealthResult::Unhealthy(reason) => {
                    child.state = ChildState::Degraded;
                    if old_state != ChildState::Degraded {
                        tracing::warn!(child = %name, reason = %reason, "child is degraded");
                    }
                }
                HealthResult::ProcessDead => {
                    child.state = ChildState::Failed;
                    child.handle = None;
                    child.pid = None;
                    tracing::error!(child = %name, "child process died");
                }
            }
        }
    }

    /// Attempt to restart failed children based on restart policy.
    pub async fn handle_restarts(&mut self) {
        let names: Vec<String> = self
            .children
            .iter()
            .filter(|(_, c)| c.state == ChildState::Failed)
            .map(|(name, _)| name.clone())
            .collect();

        for name in names {
            let should_restart = {
                let child = match self.children.get(&name) {
                    Some(c) => c,
                    None => continue,
                };

                match child.config.restart {
                    RestartPolicy::Always => child.restart_count < MAX_RESTARTS,
                    RestartPolicy::OnFailure => child.restart_count < MAX_RESTARTS,
                    RestartPolicy::Never => false,
                }
            };

            if should_restart {
                let restart_count = self.children.get(&name).map(|c| c.restart_count).unwrap_or(0);
                tracing::info!(child = %name, attempt = restart_count + 1, max = MAX_RESTARTS, "restarting child");
                match self.start_child(&name).await {
                    Ok(()) => {
                        if let Some(child) = self.children.get_mut(&name) {
                            child.restart_count = restart_count + 1;
                        }
                    }
                    Err(e) => {
                        tracing::error!(child = %name, error = %e, "restart failed");
                    }
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn get_state(&mut self, name: &str) -> Option<ChildState> {
        self.children.get(name).map(|c| c.state)
    }

    #[allow(dead_code)]
    pub fn get_all_statuses(&self) -> Vec<(&str, ChildState, Option<u32>)> {
        let mut statuses: Vec<_> = self
            .children
            .values()
            .map(|c| (c.config.name.as_str(), c.state, c.pid))
            .collect();
        statuses.sort_by(|a, b| a.0.cmp(b.0));
        statuses
    }

    pub fn is_child_online(&self, name: &str) -> bool {
        self.children
            .get(name)
            .is_some_and(|c| c.state == ChildState::Online)
    }

    pub fn children_mut(&mut self) -> &mut HashMap<String, ChildProcess> {
        &mut self.children
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::*;

    fn test_config(children: Vec<ChildConfig>) -> McpifyConfig {
        McpifyConfig {
            server: ServerConfig::default(),
            supervisor: SupervisorConfig::default(),
            children,
            tools: vec![],
        }
    }

    fn sleep_child(name: &str) -> ChildConfig {
        ChildConfig {
            name: name.to_string(),
            command: "sleep".to_string(),
            args: vec!["60".to_string()],
            cwd: ".".to_string(),
            env: HashMap::new(),
            autostart: true,
            restart: RestartPolicy::OnFailure,
            healthcheck: None,
        }
    }

    #[tokio::test]
    async fn test_start_and_stop_child() {
        let config = test_config(vec![sleep_child("worker")]);
        let mut mgr = SupervisorManager::from_config(&config);

        mgr.start_child("worker").await.unwrap();
        assert_eq!(mgr.get_state("worker"), Some(ChildState::Starting));
        assert!(mgr.children.get_mut("worker").unwrap().is_alive());

        mgr.stop_child("worker").await.unwrap();
        assert_eq!(mgr.get_state("worker"), Some(ChildState::Stopped));
    }

    #[tokio::test]
    async fn test_start_all_autostart() {
        let mut child1 = sleep_child("a");
        child1.autostart = true;
        let mut child2 = sleep_child("b");
        child2.autostart = false;

        let config = test_config(vec![child1, child2]);
        let mut mgr = SupervisorManager::from_config(&config);
        mgr.start_all().await.unwrap();

        assert_eq!(mgr.get_state("a"), Some(ChildState::Starting));
        assert_eq!(mgr.get_state("b"), Some(ChildState::Stopped));

        mgr.stop_all().await.unwrap();
    }

    #[tokio::test]
    async fn test_health_check_process_only() {
        let config = test_config(vec![sleep_child("worker")]);
        let mut mgr = SupervisorManager::from_config(&config);

        mgr.start_child("worker").await.unwrap();
        // Small delay to let process start
        tokio::time::sleep(Duration::from_millis(50)).await;

        mgr.run_health_checks().await;
        assert_eq!(mgr.get_state("worker"), Some(ChildState::Online));

        mgr.stop_all().await.unwrap();
    }

    #[tokio::test]
    async fn test_detect_dead_process() {
        let child = ChildConfig {
            name: "fast".to_string(),
            command: "true".to_string(), // exits immediately
            args: vec![],
            cwd: ".".to_string(),
            env: HashMap::new(),
            autostart: false,
            restart: RestartPolicy::Never,
            healthcheck: None,
        };
        let config = test_config(vec![child]);
        let mut mgr = SupervisorManager::from_config(&config);

        mgr.start_child("fast").await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        mgr.run_health_checks().await;
        assert_eq!(mgr.get_state("fast"), Some(ChildState::Failed));
    }
}
