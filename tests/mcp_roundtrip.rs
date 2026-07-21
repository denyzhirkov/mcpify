//! End-to-end MCP round-trip tests.
//!
//! These drive a real rmcp client against the real `McpifyServer` over an
//! in-memory duplex transport — the only way to exercise paths that need a live
//! peer: elicitation (the destructive gate), `tools/list_changed` emission on
//! reload, and pipeline chaining through `call_tool`. Colocated unit tests can't
//! reach these because there is no `Peer`/`RequestContext` outside a serve loop.
//!
//! No network or database here by design; exec tools use `printf`/`echo`, which
//! exist on both macOS (dev) and Linux (CI).

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mcpify::config::model::McpifyConfig;
use mcpify::mcp::server::McpifyServer;
use mcpify::runtime::app_state::AppState;
use mcpify::runtime::registry::ToolRegistry;
use mcpify::runtime::reload::apply_reload;
use mcpify::supervisor::manager::SupervisorManager;

use rmcp::model::{
    CallToolRequestParams, ClientInfo, CreateElicitationRequestParams, CreateElicitationResult,
    ElicitationAction, ElicitationCapability,
};
use rmcp::service::{NotificationContext, RoleClient, RoleServer, RunningService};
use rmcp::{ClientHandler, ServiceExt};
use serde_json::{Value, json};

// --- test client ------------------------------------------------------------

#[derive(Clone, Copy)]
enum Elicit {
    Accept,
    Decline,
    /// Do not advertise the elicitation capability at all, forcing the server's
    /// `confirm: true` argument fallback.
    Unsupported,
}

#[derive(Clone)]
struct TestClient {
    elicit: Elicit,
    list_changed: Arc<AtomicUsize>,
}

impl TestClient {
    fn new(elicit: Elicit) -> Self {
        Self {
            elicit,
            list_changed: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ClientHandler for TestClient {
    fn create_elicitation(
        &self,
        _request: CreateElicitationRequestParams,
        _context: rmcp::service::RequestContext<RoleClient>,
    ) -> impl Future<Output = Result<CreateElicitationResult, rmcp::ErrorData>> + Send + '_ {
        let elicit = self.elicit;
        async move {
            Ok(match elicit {
                Elicit::Accept => CreateElicitationResult {
                    action: ElicitationAction::Accept,
                    content: Some(json!({ "confirm": true })),
                },
                // Unsupported never reaches here (no capability advertised), but
                // decline is the safe answer if it somehow does.
                Elicit::Decline | Elicit::Unsupported => {
                    CreateElicitationResult::new(ElicitationAction::Decline)
                }
            })
        }
    }

    fn on_tool_list_changed(
        &self,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        let counter = Arc::clone(&self.list_changed);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn get_info(&self) -> ClientInfo {
        let mut info = ClientInfo::default();
        if !matches!(self.elicit, Elicit::Unsupported) {
            info.capabilities.elicitation = Some(ElicitationCapability::default());
        }
        info
    }
}

// --- harness ----------------------------------------------------------------

type Server = RunningService<RoleServer, McpifyServer>;
type Client = RunningService<RoleClient, TestClient>;

fn build_state(yaml: &str) -> Arc<AppState> {
    let config: McpifyConfig = serde_yaml::from_str(yaml).expect("valid test config");
    let registry = ToolRegistry::from_config(&config);
    let supervisor = SupervisorManager::from_config(&config);
    Arc::new(AppState::new(config, registry, supervisor))
}

/// Wire a client and server together over an in-memory duplex, completing the
/// initialize handshake, and publish the server peer into `state.peer` exactly
/// as `run_stdio_server` does — so the reload path can push notifications.
async fn connect(state: &Arc<AppState>, client: TestClient) -> (Server, Client) {
    let (server_io, client_io) = tokio::io::duplex(16 * 1024);
    let server_fut = McpifyServer::new(Arc::clone(state)).serve(server_io);
    let client_fut = client.serve(client_io);
    let (server, client) = tokio::join!(server_fut, client_fut);
    let server = server.expect("server handshake");
    let client = client.expect("client handshake");
    *state.peer.write().await = Some(server.peer().clone());
    (server, client)
}

async fn call(
    client: &Client,
    name: &'static str,
    arguments: Value,
) -> rmcp::model::CallToolResult {
    let mut params = CallToolRequestParams::new(name);
    if let Value::Object(map) = arguments {
        params = params.with_arguments(map);
    }
    client.call_tool(params).await.expect("call_tool transport")
}

/// Best-effort text dump of a call result for substring assertions.
fn result_text(result: &rmcp::model::CallToolResult) -> String {
    serde_json::to_string(&result.content).unwrap_or_default()
}

// --- tests ------------------------------------------------------------------

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

async fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}
