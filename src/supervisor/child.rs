use crate::config::model::ChildConfig;
use std::time::Instant;
use tokio::process::Child;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildState {
    Starting,
    Online,
    Degraded,
    Stopped,
    Failed,
}

impl std::fmt::Display for ChildState {
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

pub struct ChildProcess {
    pub config: ChildConfig,
    pub state: ChildState,
    pub handle: Option<Child>,
    pub pid: Option<u32>,
    pub started_at: Option<Instant>,
    pub last_health_check: Option<Instant>,
    pub restart_count: u32,
}

impl ChildProcess {
    pub fn new(config: ChildConfig) -> Self {
        Self {
            config,
            state: ChildState::Stopped,
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
                Ok(None) => true,  // still running
                Ok(Some(_)) => false, // exited
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
    use crate::config::model::{ChildConfig, RestartPolicy};
    use std::collections::HashMap;

    fn test_child_config(name: &str) -> ChildConfig {
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

    #[test]
    fn test_initial_state() {
        let child = ChildProcess::new(test_child_config("test"));
        assert_eq!(child.state, ChildState::Stopped);
        assert!(child.handle.is_none());
        assert!(child.pid.is_none());
        assert_eq!(child.restart_count, 0);
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", ChildState::Online), "online");
        assert_eq!(format!("{}", ChildState::Failed), "failed");
    }
}
