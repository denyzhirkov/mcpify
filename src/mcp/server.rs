use crate::adapters;
use crate::config::model::{ResourceType, ToolType};
use crate::runtime::app_state::AppState;
use crate::runtime::registry::ToolAvailability;
use anyhow::Result;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, RawResource, ReadResourceRequestParams, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerInfo, Tool,
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
        let config = self.state.current_config.try_read();
        let has_resources = config
            .as_ref()
            .map(|c| !c.resources.is_empty())
            .unwrap_or(false);

        let capabilities = if has_resources {
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build()
        } else {
            ServerCapabilities::builder().enable_tools().build()
        };
        ServerInfo::new(capabilities).with_instructions("mcpify — config-driven MCP tool runtime")
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

            let mut tool = Tool::new(
                config.name.clone(),
                config.description.clone(),
                Arc::new(input_schema),
            );

            // Map config annotations to rmcp ToolAnnotations
            if let Some(ann) = &config.annotations {
                let mut rmcp_ann = rmcp::model::ToolAnnotations::new();
                if let Some(v) = ann.read_only {
                    rmcp_ann = rmcp_ann.read_only(v);
                }
                if let Some(v) = ann.destructive {
                    rmcp_ann = rmcp_ann.destructive(v);
                }
                if let Some(v) = ann.idempotent {
                    rmcp_ann = rmcp_ann.idempotent(v);
                }
                if let Some(v) = ann.open_world {
                    rmcp_ann = rmcp_ann.open_world(v);
                }
                tool = tool.with_annotations(rmcp_ann);
            }

            tools.push(tool);
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
                if !supervisor.is_service_online(dep) {
                    return Ok(CallToolResult::error(vec![Content::text(format!(
                        "dependency '{dep}' is not online"
                    ))]));
                }
            }
        }

        let vars = self.state.vars.read().await;

        let result = match config.tool_type {
            ToolType::Exec => adapters::exec::execute(&config, input, &vars).await,
            ToolType::Http => {
                adapters::http::execute(&config, input, &self.state.http_client, &vars).await
            }
            ToolType::Sql => adapters::sql::execute(&config, input, &vars).await,
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

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> std::result::Result<ListResourcesResult, rmcp::ErrorData> {
        let config = self.state.current_config.read().await;
        let resources: Vec<Resource> = config
            .resources
            .iter()
            .map(|r| {
                let mut raw = RawResource::new(r.uri.clone(), r.name.clone());
                if let Some(desc) = &r.description {
                    raw = raw.with_description(desc.clone());
                }
                if let Some(mt) = &r.mime_type {
                    raw = raw.with_mime_type(mt.clone());
                }
                Resource {
                    raw,
                    annotations: None,
                }
            })
            .collect();

        Ok(ListResourcesResult {
            resources,
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> std::result::Result<ReadResourceResult, rmcp::ErrorData> {
        let config = self.state.current_config.read().await;
        let resource = config
            .resources
            .iter()
            .find(|r| r.uri == request.uri)
            .ok_or_else(|| {
                rmcp::ErrorData::resource_not_found(
                    format!("resource not found: {}", request.uri),
                    None,
                )
            })?;

        let (text, mime) = match resource.resource_type {
            ResourceType::File => {
                let path = resource.path.as_deref().unwrap_or("");
                let content = std::fs::read_to_string(path).map_err(|e| {
                    rmcp::ErrorData::internal_error(
                        format!("failed to read file {path}: {e}"),
                        None,
                    )
                })?;
                (content, resource.mime_type.clone())
            }
            ResourceType::Exec => {
                let cmd = resource.command.as_deref().unwrap_or("");
                let output = std::process::Command::new(cmd)
                    .args(&resource.args)
                    .output()
                    .map_err(|e| {
                        rmcp::ErrorData::internal_error(format!("failed to exec {cmd}: {e}"), None)
                    })?;
                let text = String::from_utf8_lossy(&output.stdout).to_string();
                (text, resource.mime_type.clone())
            }
        };

        let mut contents = ResourceContents::text(text, request.uri);
        if let Some(mt) = mime {
            contents = contents.with_mime_type(mt);
        }

        Ok(ReadResourceResult::new(vec![contents]))
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
