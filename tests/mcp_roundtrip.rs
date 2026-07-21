//! End-to-end MCP round-trip tests for the core protocol paths.
//!
//! These drive a real rmcp client against the real `McpifyServer` over an
//! in-memory duplex transport — the only way to exercise paths that need a live
//! peer: elicitation (the destructive gate), `tools/list_changed` emission on
//! reload, and pipeline chaining through `call_tool`. Colocated unit tests can't
//! reach these because there is no `Peer`/`RequestContext` outside a serve loop.
//!
//! exec tools use `printf`/`echo`, which exist on both macOS (dev) and Linux (CI).

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{Elicit, TestClient, build_state, call, connect, result_text, wait_until};
use mcpify::runtime::reload::apply_reload;
use serde_json::json;

const BASIC: &str = r#"
tools:
  - name: greet
    type: exec
    command: echo
    args: ["hello {{name}}"]
"#;

#[tokio::test]
async fn lists_and_calls_exec_tool() {
    let state = build_state(BASIC);
    let (_server, client) = connect(&state, TestClient::new(Elicit::Accept)).await;

    let tools = client.list_all_tools().await.expect("list_tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "greet");

    let result = call(&client, "greet", json!({ "name": "world" })).await;
    assert_ne!(result.is_error, Some(true));
    assert!(result_text(&result).contains("hello world"));
}

const DESTRUCTIVE: &str = r#"
tools:
  - name: nuke
    type: exec
    command: echo
    args: ["boom"]
    annotations:
      destructive: true
"#;

#[tokio::test]
async fn destructive_gate_elicit_accept_runs() {
    let state = build_state(DESTRUCTIVE);
    let (_server, client) = connect(&state, TestClient::new(Elicit::Accept)).await;

    let result = call(&client, "nuke", json!({})).await;
    assert_ne!(result.is_error, Some(true));
    assert!(result_text(&result).contains("boom"));
}

#[tokio::test]
async fn destructive_gate_elicit_decline_blocks() {
    let state = build_state(DESTRUCTIVE);
    let (_server, client) = connect(&state, TestClient::new(Elicit::Decline)).await;

    let result = call(&client, "nuke", json!({})).await;
    assert_eq!(result.is_error, Some(true));
    assert!(result_text(&result).contains("declined"));
}

#[tokio::test]
async fn destructive_gate_confirm_arg_fallback() {
    let state = build_state(DESTRUCTIVE);
    let (_server, client) = connect(&state, TestClient::new(Elicit::Unsupported)).await;

    // No capability -> server requires an explicit confirm arg.
    let denied = call(&client, "nuke", json!({})).await;
    assert_eq!(denied.is_error, Some(true));
    assert!(result_text(&denied).contains("confirm"));

    // With confirm:true it proceeds (and confirm is stripped before exec).
    let allowed = call(&client, "nuke", json!({ "confirm": true })).await;
    assert_ne!(allowed.is_error, Some(true));
    assert!(result_text(&allowed).contains("boom"));
}

const PIPELINE: &str = r#"
tools:
  - name: emit
    type: exec
    command: printf
    args: ['{"name":"bob"}']
  - name: greet
    type: exec
    command: echo
    args: ["hello {{name}}"]
  - name: flow
    type: pipeline
    description: emit a name, then greet it
    steps:
      - id: u
        tool: emit
      - tool: greet
        input:
          name: "{{steps.u.name}}"
"#;

#[tokio::test]
async fn pipeline_chains_step_output() {
    let state = build_state(PIPELINE);
    let (_server, client) = connect(&state, TestClient::new(Elicit::Accept)).await;

    let result = call(&client, "flow", json!({})).await;
    assert_ne!(result.is_error, Some(true));
    assert!(result_text(&result).contains("hello bob"));
}

#[tokio::test]
async fn reload_emits_tool_list_changed() {
    use std::io::Write;

    let state = build_state(BASIC);
    let client_handler = TestClient::new(Elicit::Accept);
    let counter = Arc::clone(&client_handler.list_changed);
    let (_server, client) = connect(&state, client_handler).await;

    assert_eq!(client.list_all_tools().await.unwrap().len(), 1);

    // Write an updated config (adds a tool) and drive the real reload path.
    let mut file = tempfile::NamedTempFile::new().unwrap();
    write!(
        file,
        r#"
tools:
  - name: greet
    type: exec
    command: echo
    args: ["hello {{name}}"]
  - name: farewell
    type: exec
    command: echo
    args: ["bye {{name}}"]
"#
    )
    .unwrap();
    let path = file.path().to_path_buf();

    apply_reload(&state, &path).await.expect("reload");

    // The notification is fire-and-forget; poll until the client records it.
    let notified = wait_until(Duration::from_secs(2), || {
        counter.load(Ordering::SeqCst) >= 1
    })
    .await;
    assert!(notified, "client never received tools/list_changed");

    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 2);
    assert!(tools.iter().any(|t| t.name == "farewell"));
}
