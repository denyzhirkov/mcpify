//! End-to-end tests for cached/polling tools.
//!
//! Proof of caching uses an exec tool that prints its shell PID (`echo $$`):
//! every real execution spawns a fresh `sh`, so a changed value means the action
//! ran, and an unchanged value means the cached result was served.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Elicit, TestClient, build_state, call, connect};
use serde_json::json;

const NONCE_CACHED: &str = r#"
tools:
  - name: nonce
    type: exec
    command: sh
    args: ["-c", "echo $$"]
    cache:
      refresh_interval_ms: 60000
"#;

fn body(result: &rmcp::model::CallToolResult) -> String {
    serde_json::to_string(&result.content).unwrap()
}

fn meta_cached(result: &rmcp::model::CallToolResult) -> bool {
    result
        .meta
        .as_ref()
        .and_then(|m| m.0.get("cached"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

#[tokio::test]
async fn cached_tool_serves_stored_result_then_force_refresh() {
    let state = build_state(NONCE_CACHED);
    let (_server, client) = connect(&state, TestClient::new(Elicit::Accept)).await;

    // Cold call fills the cache synchronously and is flagged as cached.
    let first = call(&client, "nonce", json!({})).await;
    assert_ne!(first.is_error, Some(true));
    assert!(meta_cached(&first));
    let cached_value = body(&first);

    // A second plain call returns the SAME stored value — the action did not run
    // again (a fresh `sh` would print a different PID).
    let second = call(&client, "nonce", json!({})).await;
    assert_eq!(body(&second), cached_value, "second call should be cached");

    // `refresh: true` bypasses the cache and runs the action fresh.
    let forced = call(&client, "nonce", json!({ "refresh": true })).await;
    assert_ne!(forced.is_error, Some(true));
    assert!(meta_cached(&forced));
    assert_ne!(
        body(&forced),
        cached_value,
        "force-refresh should re-execute the action"
    );

    // ...and the refreshed value is now what plain calls serve.
    let after = call(&client, "nonce", json!({})).await;
    assert_eq!(body(&after), body(&forced));
}

const NONCE_FAST: &str = r#"
tools:
  - name: nonce
    type: exec
    command: sh
    args: ["-c", "echo $$"]
    cache:
      refresh_interval_ms: 100
"#;

#[tokio::test]
async fn cache_poller_refreshes_in_background() {
    let state = build_state(NONCE_FAST);

    // Spawn the pollers as cmd_serve / reload would.
    {
        let cfg = state.current_config.read().await;
        state.cache_pollers.lock().await.reconcile(&state, &cfg);
    }

    // The eager fetch populates the cache without any call.
    let first = wait_for_cache(&state, "nonce").await;

    // After a few intervals the background poller has re-run the action, so the
    // stored value has changed (fresh `sh` PID each time).
    tokio::time::sleep(Duration::from_millis(400)).await;
    let later = state
        .cache
        .read()
        .await
        .get("nonce")
        .map(|e| e.result.stdout.clone())
        .expect("cache still populated");

    assert_ne!(first, later, "background poller should refresh the value");

    state.cache_pollers.lock().await.stop_all();
}

async fn wait_for_cache(state: &Arc<mcpify::runtime::app_state::AppState>, name: &str) -> String {
    for _ in 0..100 {
        if let Some(entry) = state.cache.read().await.get(name) {
            return entry.result.stdout.clone();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("cache never populated for '{name}'");
}
