use crate::config::model::{McpifyConfig, RestartPolicy};
use crate::supervisor::health::{self, HealthResult};
use crate::supervisor::service::{ManagedService, ServiceState};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::process::Command;

const MAX_RESTARTS: u32 = 3;

pub struct SupervisorManager {
    services: HashMap<String, ManagedService>,
    http_client: reqwest::Client,
    graceful_timeout: Duration,
}

impl SupervisorManager {
    pub fn from_config(config: &McpifyConfig) -> Self {
        let mut services = HashMap::new();
        for svc_config in &config.services {
            services.insert(
                svc_config.name.clone(),
                ManagedService::new(svc_config.clone()),
            );
        }
        Self {
            services,
            http_client: reqwest::Client::new(),
            graceful_timeout: Duration::from_millis(config.supervisor.graceful_shutdown_timeout_ms),
        }
    }

    pub async fn start_all(&mut self) -> Result<()> {
        let names: Vec<String> = self
            .services
            .iter()
            .filter(|(_, s)| s.config.autostart)
            .map(|(name, _)| name.clone())
            .collect();

        for name in names {
            if let Err(e) = self.start_service(&name).await {
                tracing::error!(service = %name, error = %e, "failed to start service");
            }
        }
        Ok(())
    }

    pub async fn start_service(&mut self, name: &str) -> Result<()> {
        let svc = self
            .services
            .get_mut(name)
            .with_context(|| format!("service not found: {name}"))?;

        let mut cmd = Command::new(&svc.config.command);
        cmd.args(&svc.config.args);
        cmd.current_dir(&svc.config.cwd);

        for (k, v) in &svc.config.env {
            cmd.env(k, v);
        }

        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::piped());

        let process = cmd.spawn().with_context(|| {
            format!(
                "spawning service '{}': {} {}",
                name,
                svc.config.command,
                svc.config.args.join(" ")
            )
        })?;

        let pid = process.id();
        svc.pid = pid;
        svc.handle = Some(process);
        svc.state = ServiceState::Starting;
        svc.started_at = Some(Instant::now());
        svc.restart_count = 0;

        tracing::info!(service = %name, pid = ?pid, "service started");
        Ok(())
    }

    pub async fn stop_service(&mut self, name: &str) -> Result<()> {
        let svc = self
            .services
            .get_mut(name)
            .with_context(|| format!("service not found: {name}"))?;

        if let Some(handle) = &mut svc.handle {
            #[cfg(unix)]
            if let Some(pid) = svc.pid {
                if let Ok(raw_pid) = i32::try_from(pid) {
                    if raw_pid > 0 {
                        let _ = nix::sys::signal::kill(
                            nix::unistd::Pid::from_raw(raw_pid),
                            nix::sys::signal::Signal::SIGTERM,
                        );
                    }
                }
            }

            let wait_result =
                tokio::time::timeout(self.graceful_timeout, handle.wait()).await;

            match wait_result {
                Ok(Ok(_)) => {
                    tracing::info!(service = %name, "service stopped gracefully");
                }
                _ => {
                    let _ = handle.kill().await;
                    tracing::warn!(service = %name, "service force killed");
                }
            }
        }

        svc.handle = None;
        svc.pid = None;
        svc.state = ServiceState::Stopped;
        Ok(())
    }

    pub async fn stop_all(&mut self) -> Result<()> {
        let names: Vec<String> = self.services.keys().cloned().collect();
        for name in names {
            if let Err(e) = self.stop_service(&name).await {
                tracing::error!(service = %name, error = %e, "failed to stop service");
            }
        }
        Ok(())
    }

    pub async fn run_health_checks(&mut self) {
        let names: Vec<String> = self.services.keys().cloned().collect();

        for name in names {
            let svc = match self.services.get_mut(&name) {
                Some(s) => s,
                None => continue,
            };

            if svc.state == ServiceState::Stopped {
                continue;
            }

            let result = health::check_health(svc, &self.http_client).await;
            svc.last_health_check = Some(Instant::now());

            let old_state = svc.state;
            match result {
                HealthResult::Healthy => {
                    if svc.state != ServiceState::Online {
                        svc.state = ServiceState::Online;
                        tracing::info!(service = %name, "service is online");
                    }
                }
                HealthResult::Unhealthy(reason) => {
                    svc.state = ServiceState::Degraded;
                    if old_state != ServiceState::Degraded {
                        tracing::warn!(service = %name, reason = %reason, "service is degraded");
                    }
                }
                HealthResult::ProcessDead => {
                    svc.state = ServiceState::Failed;
                    svc.handle = None;
                    svc.pid = None;
                    tracing::error!(service = %name, "service process died");
                }
            }
        }
    }

    pub async fn handle_restarts(&mut self) {
        let names: Vec<String> = self
            .services
            .iter()
            .filter(|(_, s)| s.state == ServiceState::Failed)
            .map(|(name, _)| name.clone())
            .collect();

        for name in names {
            let should_restart = {
                let svc = match self.services.get(&name) {
                    Some(s) => s,
                    None => continue,
                };

                match svc.config.restart {
                    RestartPolicy::Always => svc.restart_count < MAX_RESTARTS,
                    RestartPolicy::OnFailure => svc.restart_count < MAX_RESTARTS,
                    RestartPolicy::Never => false,
                }
            };

            if should_restart {
                let restart_count = self.services.get(&name).map(|s| s.restart_count).unwrap_or(0);
                tracing::info!(service = %name, attempt = restart_count + 1, max = MAX_RESTARTS, "restarting service");
                match self.start_service(&name).await {
                    Ok(()) => {
                        if let Some(svc) = self.services.get_mut(&name) {
                            svc.restart_count = restart_count + 1;
                        }
                    }
                    Err(e) => {
                        tracing::error!(service = %name, error = %e, "restart failed");
                    }
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn get_state(&mut self, name: &str) -> Option<ServiceState> {
        self.services.get(name).map(|s| s.state)
    }

    #[allow(dead_code)]
    pub fn get_all_statuses(&self) -> Vec<(&str, ServiceState, Option<u32>)> {
        let mut statuses: Vec<_> = self
            .services
            .values()
            .map(|s| (s.config.name.as_str(), s.state, s.pid))
            .collect();
        statuses.sort_by(|a, b| a.0.cmp(b.0));
        statuses
    }

    pub fn is_service_online(&self, name: &str) -> bool {
        self.services
            .get(name)
            .is_some_and(|s| s.state == ServiceState::Online)
    }

    pub fn services_mut(&mut self) -> &mut HashMap<String, ManagedService> {
        &mut self.services
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::*;

    fn test_config(services: Vec<ServiceConfig>) -> McpifyConfig {
        McpifyConfig {
            server: ServerConfig::default(),
            supervisor: SupervisorConfig::default(),
            services,
            tools: vec![],
        }
    }

    fn sleep_service(name: &str) -> ServiceConfig {
        ServiceConfig {
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
    async fn test_start_and_stop_service() {
        let config = test_config(vec![sleep_service("worker")]);
        let mut mgr = SupervisorManager::from_config(&config);

        mgr.start_service("worker").await.unwrap();
        assert_eq!(mgr.get_state("worker"), Some(ServiceState::Starting));
        assert!(mgr.services.get_mut("worker").unwrap().is_alive());

        mgr.stop_service("worker").await.unwrap();
        assert_eq!(mgr.get_state("worker"), Some(ServiceState::Stopped));
    }

    #[tokio::test]
    async fn test_start_all_autostart() {
        let mut svc1 = sleep_service("a");
        svc1.autostart = true;
        let mut svc2 = sleep_service("b");
        svc2.autostart = false;

        let config = test_config(vec![svc1, svc2]);
        let mut mgr = SupervisorManager::from_config(&config);
        mgr.start_all().await.unwrap();

        assert_eq!(mgr.get_state("a"), Some(ServiceState::Starting));
        assert_eq!(mgr.get_state("b"), Some(ServiceState::Stopped));

        mgr.stop_all().await.unwrap();
    }

    #[tokio::test]
    async fn test_health_check_process_only() {
        let config = test_config(vec![sleep_service("worker")]);
        let mut mgr = SupervisorManager::from_config(&config);

        mgr.start_service("worker").await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        mgr.run_health_checks().await;
        assert_eq!(mgr.get_state("worker"), Some(ServiceState::Online));

        mgr.stop_all().await.unwrap();
    }

    #[tokio::test]
    async fn test_detect_dead_process() {
        let svc = ServiceConfig {
            name: "fast".to_string(),
            command: "true".to_string(),
            args: vec![],
            cwd: ".".to_string(),
            env: HashMap::new(),
            autostart: false,
            restart: RestartPolicy::Never,
            healthcheck: None,
        };
        let config = test_config(vec![svc]);
        let mut mgr = SupervisorManager::from_config(&config);

        mgr.start_service("fast").await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        mgr.run_health_checks().await;
        assert_eq!(mgr.get_state("fast"), Some(ServiceState::Failed));
    }
}
