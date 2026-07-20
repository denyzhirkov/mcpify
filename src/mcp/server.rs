use crate::adapters;
use crate::adapters::ToolResult;
use crate::config::model::{OutputParse, ResourceType, ToolConfig, ToolType};
use crate::runtime::app_state::AppState;
use crate::runtime::registry::ToolAvailability;
use anyhow::Result;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, RawResource, ReadResourceRequestParams, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{ElicitationError, Peer, RoleServer};
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
                .enable_tool_list_changed()
                .enable_resources()
                .build()
        } else {
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build()
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

            // Advertise the declared output schema, if any.
            if let Some(output) = &config.output
                && let Some(Value::Object(map)) = &output.schema
            {
                tool = tool.with_raw_output_schema(Arc::new(map.clone()));
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
        context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
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

        // Destructive gate: elicit confirmation (or fall back to confirm:true),
        // then strip confirm so it never leaks into the rendered command/query.
        let input = match destructive_gate(&context.peer, &config, input).await {
            Ok(input) => input,
            Err(msg) => return Ok(CallToolResult::error(vec![Content::text(msg)])),
        };

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
                let structured = resolve_structured_content(&config, &tool_result);

                let mut content = vec![Content::text(tool_result.stdout)];
                if !tool_result.stderr.is_empty() {
                    content.push(Content::text(format!("[stderr] {}", tool_result.stderr)));
                }

                let mut call_result = if tool_result.is_error {
                    CallToolResult::error(content)
                } else {
                    CallToolResult::success(content)
                };
                call_result.structured_content = structured;
                Ok(call_result)
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

/// `InputSchema` serializes directly as a JSON Schema object, so this is just
/// `to_value` with an empty-object fallback for tools that declare no input.
fn build_input_schema(config: &ToolConfig) -> Value {
    let empty = || json!({ "type": "object", "properties": {} });
    match &config.input {
        Some(schema) => serde_json::to_value(schema).unwrap_or_else(|_| empty()),
        None => empty(),
    }
}

/// Elicited confirmation for a destructive tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct Confirm {
    confirm: bool,
}
rmcp::elicit_safe!(Confirm);

fn strip_confirm(mut input: Value) -> Value {
    if let Value::Object(map) = &mut input {
        map.remove("confirm");
    }
    input
}

/// Fallback gate for clients without elicitation: require an explicit
/// `confirm: true` argument, then strip it so it never reaches the renderer.
/// A guardrail, not authorization.
fn confirm_arg_gate(config: &ToolConfig, input: Value) -> std::result::Result<Value, String> {
    if input.get("confirm") == Some(&Value::Bool(true)) {
        Ok(strip_confirm(input))
    } else {
        Err(format!(
            "tool '{}' is destructive; pass \"confirm\": true to proceed",
            config.name
        ))
    }
}

/// Enforce the `destructive` annotation. Prefer MCP elicitation (real
/// human-in-the-loop); if the client can't elicit, fall back to the
/// `confirm: true` argument gate. Fail-closed on decline/cancel/error.
async fn destructive_gate(
    peer: &Peer<RoleServer>,
    config: &ToolConfig,
    input: Value,
) -> std::result::Result<Value, String> {
    let destructive = config
        .annotations
        .as_ref()
        .and_then(|a| a.destructive)
        .unwrap_or(false);
    if !destructive {
        return Ok(input);
    }

    let prompt = format!(
        "Run destructive tool '{}'? Confirm to proceed.",
        config.name
    );
    match peer.elicit::<Confirm>(prompt).await {
        Ok(Some(c)) if c.confirm => Ok(strip_confirm(input)),
        Ok(_) => Err(format!(
            "destructive tool '{}' was not confirmed",
            config.name
        )),
        Err(ElicitationError::CapabilityNotSupported) => confirm_arg_gate(config, input),
        Err(ElicitationError::UserDeclined) | Err(ElicitationError::UserCancelled) => Err(format!(
            "destructive tool '{}' declined by user",
            config.name
        )),
        Err(e) => Err(format!(
            "destructive tool '{}': elicitation failed: {e}",
            config.name
        )),
    }
}

/// Resolve the MCP `structuredContent` for a call: prefer what the adapter
/// produced natively (sql rows); otherwise, if the tool declares
/// `output.parse: json`, parse stdout — degrading to text-only (`None`) when
/// stdout is not valid JSON rather than failing the call.
fn resolve_structured_content(config: &ToolConfig, result: &ToolResult) -> Option<Value> {
    if let Some(structured) = &result.structured {
        return Some(structured.clone());
    }
    match &config.output {
        Some(output) if output.parse == OutputParse::Json => {
            match serde_json::from_str::<Value>(&result.stdout) {
                Ok(value) => Some(value),
                Err(e) => {
                    tracing::warn!(
                        tool = %config.name,
                        error = %e,
                        "output.parse=json but stdout is not valid JSON; returning text only"
                    );
                    None
                }
            }
        }
        _ => None,
    }
}

pub async fn run_stdio_server(state: Arc<AppState>) -> Result<()> {
    let server = McpifyServer::new(state.clone());
    let transport = rmcp::transport::io::stdio();

    tracing::info!("MCP server starting on stdio");
    let handle = server.serve(transport).await?;
    // Publish the peer so the reload path can push tools/list_changed.
    *state.peer.write().await = Some(handle.peer().clone());
    handle.waiting().await?;
    tracing::info!("MCP server stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::ToolResult;
    use crate::config::model::McpifyConfig;

    fn first_tool(yaml: &str) -> ToolConfig {
        let cfg: McpifyConfig = serde_yaml::from_str(yaml).unwrap();
        cfg.tools.into_iter().next().unwrap()
    }

    fn text_result(stdout: &str) -> ToolResult {
        ToolResult {
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            is_error: false,
            structured: None,
        }
    }

    #[test]
    fn test_build_input_schema_rich() {
        let tool = first_tool(
            r#"
tools:
  - name: t
    type: exec
    command: echo
    input:
      type: object
      properties:
        age:
          type: integer
          minimum: 0
          maximum: 120
        role:
          type: string
          enum: [admin, user]
          default: user
        tags:
          type: array
          items: { type: string }
      required: [role]
"#,
        );
        let schema = build_input_schema(&tool);

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["role"]));

        let age = &schema["properties"]["age"];
        assert_eq!(age["type"], "integer");
        assert_eq!(age["minimum"], json!(0));
        assert_eq!(age["maximum"], json!(120));

        let role = &schema["properties"]["role"];
        assert_eq!(role["enum"], json!(["admin", "user"]));
        assert_eq!(role["default"], "user");

        assert_eq!(schema["properties"]["tags"]["items"]["type"], "string");
    }

    #[test]
    fn test_build_input_schema_none_defaults_to_empty_object() {
        let tool = first_tool(
            r#"
tools:
  - name: t
    type: exec
    command: echo
"#,
        );
        let schema = build_input_schema(&tool);
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"].as_object().unwrap().is_empty());
    }

    #[test]
    fn test_structured_native_takes_precedence() {
        let tool = first_tool(
            r#"
tools:
  - name: t
    type: sql
    driver: sqlite
    output:
      parse: json
"#,
        );
        let mut result = text_result("not json at all");
        result.structured = Some(json!([{ "id": 1 }]));
        assert_eq!(
            resolve_structured_content(&tool, &result),
            Some(json!([{ "id": 1 }]))
        );
    }

    #[test]
    fn test_output_parse_json_valid() {
        let tool = first_tool(
            r#"
tools:
  - name: t
    type: http
    method: GET
    url: http://x
    output:
      parse: json
"#,
        );
        let result = text_result(r#"{"ok": true}"#);
        assert_eq!(
            resolve_structured_content(&tool, &result),
            Some(json!({ "ok": true }))
        );
    }

    #[test]
    fn test_output_parse_json_invalid_degrades_to_none() {
        let tool = first_tool(
            r#"
tools:
  - name: t
    type: exec
    command: echo
    output:
      parse: json
"#,
        );
        let result = text_result("plain text");
        assert_eq!(resolve_structured_content(&tool, &result), None);
    }

    #[test]
    fn test_confirm_arg_gate() {
        // Fallback path (client without elicitation): require confirm: true.
        let tool = first_tool(
            r#"
tools:
  - name: rm
    type: exec
    command: rm
    annotations: { destructive: true }
"#,
        );
        assert!(confirm_arg_gate(&tool, json!({})).is_err());
        assert!(confirm_arg_gate(&tool, json!({ "confirm": false })).is_err());

        let out = confirm_arg_gate(&tool, json!({ "confirm": true, "x": 1 })).unwrap();
        assert_eq!(out.get("confirm"), None); // stripped
        assert_eq!(out["x"], json!(1));
    }

    #[test]
    fn test_strip_confirm() {
        let out = strip_confirm(json!({ "confirm": true, "keep": 1 }));
        assert_eq!(out.get("confirm"), None);
        assert_eq!(out["keep"], json!(1));
    }

    #[test]
    fn test_no_output_no_structured() {
        let tool = first_tool(
            r#"
tools:
  - name: t
    type: exec
    command: echo
"#,
        );
        let result = text_result(r#"{"looks": "like json"}"#);
        assert_eq!(resolve_structured_content(&tool, &result), None);
    }
}
