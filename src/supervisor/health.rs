use crate::config::model::HealthcheckType;
use crate::supervisor::service::ManagedService;
use std::time::Duration;

#[derive(Debug)]
pub enum HealthResult {
    Healthy,
    Unhealthy(String),
    ProcessDead,
}

pub async fn check_health(svc: &mut ManagedService, client: &reqwest::Client) -> HealthResult {
    if !svc.is_alive() {
        return HealthResult::ProcessDead;
    }

    let hc = match &svc.config.healthcheck {
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
        HealthcheckType::Process => HealthResult::Healthy,
    }
}
