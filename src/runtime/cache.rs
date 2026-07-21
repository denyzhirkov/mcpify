//! Background caching for tools with a `cache` block: a per-tool poller runs the
//! action on `refresh_interval_ms` (the health-loop pattern) and stores the last
//! result, so `call_tool` serves it instantly. Cached tools are parameterless —
//! the fetch uses config vars + empty input.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};
use tokio::task::JoinHandle;

use crate::adapters::ToolResult;
use crate::config::model::{McpifyConfig, ToolConfig, ToolType};
use crate::runtime::app_state::AppState;

/// The last stored result of a cached tool plus freshness bookkeeping.
#[derive(Clone)]
pub struct CachedEntry {
    pub result: ToolResult,
    pub fetched_at: SystemTime,
    pub stale: bool,
    pub last_error: Option<String>,
}

impl CachedEntry {
    fn fresh(result: ToolResult) -> Self {
        Self {
            result,
            fetched_at: SystemTime::now(),
            stale: false,
            last_error: None,
        }
    }

    /// Freshness metadata for the MCP result `_meta` — lets the agent judge how
    /// old the served value is (times are UTC unix milliseconds).
    pub fn freshness_meta(&self) -> Map<String, Value> {
        let age_ms = self
            .fetched_at
            .elapsed()
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let fetched_at_unix_ms = self
            .fetched_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut m = Map::new();
        m.insert("cached".into(), json!(true));
        m.insert("fetched_at_unix_ms".into(), json!(fetched_at_unix_ms));
        m.insert("age_ms".into(), json!(age_ms));
        m.insert("stale".into(), json!(self.stale));
        m.insert("last_error".into(), json!(self.last_error));
        m
    }
}

/// Dispatch a tool's action to its adapter. Shared by the normal call path, the
/// cache poller, cold fills, and forced refreshes.
pub async fn execute_tool_action(
    state: &Arc<AppState>,
    config: &ToolConfig,
    input: Value,
) -> anyhow::Result<ToolResult> {
    match config.tool_type {
        ToolType::Exec => {
            let vars = state.vars.read().await;
            crate::adapters::exec::execute(config, input, &vars).await
        }
        ToolType::Http => {
            let vars = state.vars.read().await;
            crate::adapters::http::execute(config, input, &state.http_client, &vars).await
        }
        ToolType::Sql => {
            let vars = state.vars.read().await;
            crate::adapters::sql::execute(config, input, &vars).await
        }
        ToolType::Pipeline => crate::runtime::pipeline::execute(state, config, input).await,
    }
}

/// Fetch now (empty input), store as the fresh entry, and return it. Used by the
/// call path for a cold cache or a forced `refresh: true`.
pub async fn fill(
    state: &Arc<AppState>,
    tool_name: &str,
    config: &ToolConfig,
) -> anyhow::Result<CachedEntry> {
    let result = execute_tool_action(state, config, json!({})).await?;
    let entry = CachedEntry::fresh(result);
    state
        .cache
        .write()
        .await
        .insert(tool_name.to_string(), entry.clone());
    Ok(entry)
}

/// Run a cached tool's action once with empty input and store the result.
/// Success replaces the entry; failure keeps the last-good value and marks it
/// stale with the error (or leaves the cache cold if there was nothing yet).
pub async fn refresh_once(state: &Arc<AppState>, tool_name: &str) {
    let config = {
        let registry = state.registry.read().await;
        match registry.get(tool_name) {
            Some(entry) => entry.config.clone(),
            None => return,
        }
    };

    match execute_tool_action(state, &config, json!({})).await {
        Ok(result) => {
            state
                .cache
                .write()
                .await
                .insert(tool_name.to_string(), CachedEntry::fresh(result));
        }
        Err(e) => {
            tracing::warn!(tool = %tool_name, error = %e, "cache refresh failed; serving last-good");
            if let Some(entry) = state.cache.write().await.get_mut(tool_name) {
                entry.stale = true;
                entry.last_error = Some(e.to_string());
            }
        }
    }
}

struct PollerHandle {
    interval_ms: u64,
    handle: JoinHandle<()>,
}

/// Owns the background poller task per cached tool. Reconciled against config on
/// startup and reload so pollers track the current tool set.
#[derive(Default)]
pub struct CachePollers {
    tasks: HashMap<String, PollerHandle>,
}

impl CachePollers {
    /// Spawn/replace/stop pollers so they exactly match the tools declaring a
    /// `cache` block in `config`. A changed interval restarts the poller.
    pub fn reconcile(&mut self, state: &Arc<AppState>, config: &McpifyConfig) {
        let desired: HashMap<&str, u64> = config
            .tools
            .iter()
            .filter_map(|t| {
                t.cache
                    .as_ref()
                    .map(|c| (t.name.as_str(), c.refresh_interval_ms))
            })
            .collect();

        // Stop pollers whose tool lost its cache block or changed interval.
        let stale: Vec<String> = self
            .tasks
            .iter()
            .filter(|(name, h)| desired.get(name.as_str()) != Some(&h.interval_ms))
            .map(|(name, _)| name.clone())
            .collect();
        for name in stale {
            if let Some(h) = self.tasks.remove(&name) {
                h.handle.abort();
            }
        }

        // Start pollers for newly-cached (or interval-changed) tools.
        for (name, interval_ms) in desired {
            if !self.tasks.contains_key(name) {
                let handle = tokio::spawn(poll_loop(
                    Arc::clone(state),
                    name.to_string(),
                    Duration::from_millis(interval_ms),
                ));
                self.tasks.insert(
                    name.to_string(),
                    PollerHandle {
                        interval_ms,
                        handle,
                    },
                );
            }
        }
    }

    pub fn stop_all(&mut self) {
        for (_, h) in self.tasks.drain() {
            h.handle.abort();
        }
    }
}

/// Eager fetch, then refresh on the interval, until the task is aborted.
async fn poll_loop(state: Arc<AppState>, tool_name: String, interval: Duration) {
    loop {
        refresh_once(&state, &tool_name).await;
        tokio::time::sleep(interval).await;
    }
}
