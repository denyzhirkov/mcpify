use crate::adapters;
use crate::config::model::ToolType;
use crate::runtime::app_state::AppState;
use crate::runtime::registry::ToolAvailability;
use anyhow::Result;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo, Tool,
};
use rmcp::{ServerHandler, service::ServiceExt};
use serde_json::{Map, Value, json};
use std::sync::Arc;

pub struct McpifyServer {
    state: Arc<AppState>,
}

impl McpifyServer {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

impl ServerHandler for McpifyServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("mcpify — config-driven MCP tool runtime")
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> std::result::Result<ListToolsResult, rmcp::ErrorData> {
        let registry = self.state.registry.read().await;
        let mut tools = Vec::new();

        for entry in registry.list() {
            if entry.availability != ToolAvailability::Enabled {
                continue;
            }
            let config = &entry.config;
            let schema = build_input_schema(config);

            let input_schema: serde_json::Map<String, Value> = match schema {
                Value::Object(map) => map,
                _ => serde_json::Map::new(),
            };

            tools.push(Tool::new(
                config.name.clone(),
                config.description.clone(),
                Arc::new(input_schema),
            ));
        }

        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> std::result::Result<CallToolResult, rmcp::ErrorData> {
        let tool_name: &str = &request.name;
        let input = match request.arguments {
            Some(args) => Value::Object(args),
            None => Value::Object(Map::new()),
        };

        // Take registry lock, extract what we need, drop before supervisor lock
        let (config, depends_on) = {
            let registry = self.state.registry.read().await;
            let entry = match registry.get(tool_name) {
                Some(e) => e,
                None => {
                    return Ok(CallToolResult::error(vec![Content::text(format!(
                        "tool not found: {tool_name}"
                    ))]));
                }
            };

            if entry.availability != ToolAvailability::Enabled {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "tool not available: {:?}",
                    entry.availability
                ))]));
            }

            (entry.config.clone(), entry.config.depends_on.clone())
        };
        // registry lock dropped here

        // Check depends_on with a single read lock on supervisor
        if !depends_on.is_empty() {
            let supervisor = self.state.supervisor.read().await;
            for dep in &depends_on {
                if !supervisor.is_child_online(dep) {
                    return Ok(CallToolResult::error(vec![Content::text(format!(
                        "dependency '{dep}' is not online"
                    ))]));
                }
            }
        }

        let result = match config.tool_type {
            ToolType::Exec => adapters::exec::execute(&config, input).await,
            ToolType::Http => {
                adapters::http::execute(&config, input, &self.state.http_client).await
            }
        };

        match result {
            Ok(tool_result) => {
                let mut content = vec![Content::text(tool_result.stdout)];
                if !tool_result.stderr.is_empty() {
                    content.push(Content::text(format!("[stderr] {}", tool_result.stderr)));
                }
                if tool_result.is_error {
                    Ok(CallToolResult::error(content))
                } else {
                    Ok(CallToolResult::success(content))
                }
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "error: {e}"
            ))])),
        }
    }
}

fn build_input_schema(config: &crate::config::model::ToolConfig) -> Value {
    match &config.input {
        Some(schema) => {
            let mut props = serde_json::Map::new();
            for (name, def) in &schema.properties {
                let mut prop = serde_json::Map::new();
                prop.insert("type".to_string(), json!(def.prop_type));
                if let Some(desc) = &def.description {
                    prop.insert("description".to_string(), json!(desc));
                }
                props.insert(name.clone(), Value::Object(prop));
            }
            json!({
                "type": "object",
                "properties": props,
                "required": schema.required,
            })
        }
        None => {
            json!({
                "type": "object",
                "properties": {},
            })
        }
    }
}

pub async fn run_stdio_server(state: Arc<AppState>) -> Result<()> {
    let server = McpifyServer::new(state);
    let transport = rmcp::transport::io::stdio();

    tracing::info!("MCP server starting on stdio");
    let handle = server.serve(transport).await?;
    handle.waiting().await?;
    tracing::info!("MCP server stopped");
    Ok(())
}
