//! Shared harness for the `tests/` integration round-trips: a real rmcp client
//! wired to `McpifyServer` over an in-memory `tokio::io::duplex`. Included via
//! `mod common;` in each test binary — not every binary uses every helper, so
//! dead_code is allowed here rather than per-item.
#![allow(dead_code)]

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mcpify::config::model::McpifyConfig;
use mcpify::mcp::server::McpifyServer;
use mcpify::runtime::app_state::AppState;
use mcpify::runtime::registry::ToolRegistry;
use mcpify::supervisor::manager::SupervisorManager;

use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientInfo, CreateElicitationRequestParams,
    CreateElicitationResult, ElicitationAction, ElicitationCapability,
};
use rmcp::service::{NotificationContext, RoleClient, RoleServer, RunningService};
use rmcp::{ClientHandler, ServiceExt};
use serde_json::{Value, json};

#[derive(Clone, Copy)]
pub enum Elicit {
    Accept,
    Decline,
    /// Do not advertise the elicitation capability at all, forcing the server's
    /// `confirm: true` argument fallback.
    Unsupported,
}

#[derive(Clone)]
pub struct TestClient {
    elicit: Elicit,
    pub list_changed: Arc<AtomicUsize>,
}

impl TestClient {
    pub fn new(elicit: Elicit) -> Self {
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

pub type Server = RunningService<RoleServer, McpifyServer>;
pub type Client = RunningService<RoleClient, TestClient>;

pub fn build_state(yaml: &str) -> Arc<AppState> {
    let config: McpifyConfig = serde_yaml::from_str(yaml).expect("valid test config");
    let registry = ToolRegistry::from_config(&config);
    let supervisor = SupervisorManager::from_config(&config);
    Arc::new(AppState::new(config, registry, supervisor))
}

/// Wire a client and server together over an in-memory duplex, completing the
/// initialize handshake, and publish the server peer into `state.peer` exactly
/// as `run_stdio_server` does — so the reload path can push notifications.
pub async fn connect(state: &Arc<AppState>, client: TestClient) -> (Server, Client) {
    let (server_io, client_io) = tokio::io::duplex(16 * 1024);
    let server_fut = McpifyServer::new(Arc::clone(state)).serve(server_io);
    let client_fut = client.serve(client_io);
    let (server, client) = tokio::join!(server_fut, client_fut);
    let server = server.expect("server handshake");
    let client = client.expect("client handshake");
    *state.peer.write().await = Some(server.peer().clone());
    (server, client)
}

pub async fn call(client: &Client, name: &'static str, arguments: Value) -> CallToolResult {
    let mut params = CallToolRequestParams::new(name);
    if let Value::Object(map) = arguments {
        params = params.with_arguments(map);
    }
    client.call_tool(params).await.expect("call_tool transport")
}

/// Best-effort text dump of a call result for substring assertions.
pub fn result_text(result: &CallToolResult) -> String {
    serde_json::to_string(&result.content).unwrap_or_default()
}

pub async fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}
