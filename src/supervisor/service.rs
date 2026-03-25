use crate::config::model::ServiceConfig;
use std::time::Instant;
use tokio::process::Child;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Starting,
    Online,
    Degraded,
    Stopped,
    Failed,
}

impl std::fmt::Display for ServiceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(f, "starting"),
            Self::Online => write!(f, "online"),
            Self::Degraded => write!(f, "degraded"),
            Self::Stopped => write!(f, "stopped"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

pub struct ManagedService {
    pub config: ServiceConfig,
    pub state: ServiceState,
    pub handle: Option<Child>,
    pub pid: Option<u32>,
    pub started_at: Option<Instant>,
    pub last_health_check: Option<Instant>,
    pub restart_count: u32,
}

impl ManagedService {
    pub fn new(config: ServiceConfig) -> Self {
        Self {
            config,
            state: ServiceState::Stopped,
            handle: None,
            pid: None,
            started_at: None,
            last_health_check: None,
            restart_count: 0,
        }
    }

    pub fn is_alive(&mut self) -> bool {
        if let Some(handle) = &mut self.handle {
            match handle.try_wait() {
                Ok(None) => true,
                Ok(Some(_)) => false,
                Err(_) => false,
            }
        } else {
            false
        }
    }

    #[allow(dead_code)]
    pub fn name(&self) -> &str {
        &self.config.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{ServiceConfig, RestartPolicy};
    use std::collections::HashMap;

    fn test_service_config(name: &str) -> ServiceConfig {
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

    #[test]
    fn test_initial_state() {
        let svc = ManagedService::new(test_service_config("test"));
        assert_eq!(svc.state, ServiceState::Stopped);
        assert!(svc.handle.is_none());
        assert!(svc.pid.is_none());
        assert_eq!(svc.restart_count, 0);
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", ServiceState::Online), "online");
        assert_eq!(format!("{}", ServiceState::Failed), "failed");
    }
}
