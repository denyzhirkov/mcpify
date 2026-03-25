use crate::config::model::HealthcheckType;
use crate::supervisor::child::ChildProcess;
use std::time::Duration;

#[derive(Debug)]
pub enum HealthResult {
    Healthy,
    Unhealthy(String),
    ProcessDead,
}

pub async fn check_health(child: &mut ChildProcess, client: &reqwest::Client) -> HealthResult {
    // First check if process is alive
    if !child.is_alive() {
        return HealthResult::ProcessDead;
    }

    // If no healthcheck configured, process alive = healthy
    let hc = match &child.config.healthcheck {
        Some(hc) => hc,
        None => return HealthResult::Healthy,
    };

    match hc.check_type {
        HealthcheckType::Http => {
            let url = match &hc.url {
                Some(u) => u,
                None => return HealthResult::Unhealthy("http healthcheck missing url".to_string()),
            };
            let timeout = Duration::from_millis(hc.timeout_ms);
            match client.get(url).timeout(timeout).send().await {
                Ok(resp) if resp.status().is_success() => HealthResult::Healthy,
                Ok(resp) => {
                    HealthResult::Unhealthy(format!("healthcheck returned {}", resp.status()))
                }
                Err(e) => HealthResult::Unhealthy(format!("healthcheck failed: {e}")),
            }
        }
        HealthcheckType::Process => {
            // Process-only: alive check already passed above
            HealthResult::Healthy
        }
    }
}
