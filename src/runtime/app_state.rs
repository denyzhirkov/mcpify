use crate::config::model::McpifyConfig;
use crate::runtime::registry::ToolRegistry;
use crate::supervisor::manager::SupervisorManager;
use rmcp::service::{Peer, RoleServer};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::sync::RwLock;

pub struct AppState {
    pub registry: Arc<RwLock<ToolRegistry>>,
    pub supervisor: Arc<RwLock<SupervisorManager>>,
    pub current_config: Arc<RwLock<McpifyConfig>>,
    pub http_client: reqwest::Client,
    pub generation: AtomicU64,
    pub vars: Arc<RwLock<HashMap<String, String>>>,
    /// Connected MCP peer, set once the stdio server is serving; lets the reload
    /// path push `notifications/tools/list_changed`.
    pub peer: Arc<RwLock<Option<Peer<RoleServer>>>>,
}

impl AppState {
    pub fn new(
        config: McpifyConfig,
        registry: ToolRegistry,
        supervisor: SupervisorManager,
    ) -> Self {
        let vars = config.vars.clone();
        Self {
            registry: Arc::new(RwLock::new(registry)),
            supervisor: Arc::new(RwLock::new(supervisor)),
            current_config: Arc::new(RwLock::new(config)),
            http_client: reqwest::Client::new(),
            generation: AtomicU64::new(0),
            vars: Arc::new(RwLock::new(vars)),
            peer: Arc::new(RwLock::new(None)),
        }
    }
}
