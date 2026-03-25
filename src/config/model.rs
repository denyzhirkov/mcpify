use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpifyConfig {
    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub supervisor: SupervisorConfig,

    #[serde(default)]
    pub services: Vec<ServiceConfig>,

    #[serde(default)]
    pub tools: Vec<ToolConfig>,

    #[serde(default)]
    pub vars: HashMap<String, String>,

    #[serde(default)]
    pub resources: Vec<ResourceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_server_name")]
    pub name: String,

    #[serde(default = "default_transport")]
    pub transport: String,

    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: default_server_name(),
            transport: default_transport(),
            log_level: default_log_level(),
        }
    }
}

fn default_server_name() -> String {
    "mcpify".to_string()
}

fn default_transport() -> String {
    "stdio".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorConfig {
    #[serde(default = "default_restart_policy")]
    pub restart_policy: RestartPolicy,

    #[serde(default = "default_healthcheck_interval")]
    pub healthcheck_interval_ms: u64,

    #[serde(default = "default_graceful_shutdown_timeout")]
    pub graceful_shutdown_timeout_ms: u64,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            restart_policy: default_restart_policy(),
            healthcheck_interval_ms: default_healthcheck_interval(),
            graceful_shutdown_timeout_ms: default_graceful_shutdown_timeout(),
        }
    }
}

fn default_restart_policy() -> RestartPolicy {
    RestartPolicy::OnFailure
}

fn default_healthcheck_interval() -> u64 {
    3000
}

fn default_graceful_shutdown_timeout() -> u64 {
    5000
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    OnFailure,
    Always,
    Never,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub name: String,
    pub command: String,

    #[serde(default)]
    pub args: Vec<String>,

    #[serde(default = "default_cwd")]
    pub cwd: String,

    #[serde(default)]
    pub env: HashMap<String, String>,

    #[serde(default = "default_true")]
    pub autostart: bool,

    #[serde(default = "default_restart_policy")]
    pub restart: RestartPolicy,

    #[serde(default)]
    pub healthcheck: Option<HealthcheckConfig>,
}

fn default_cwd() -> String {
    ".".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthcheckConfig {
    #[serde(rename = "type")]
    pub check_type: HealthcheckType,

    #[serde(default)]
    pub url: Option<String>,

    #[serde(default = "default_healthcheck_interval")]
    pub interval_ms: u64,

    #[serde(default = "default_healthcheck_timeout")]
    pub timeout_ms: u64,
}

fn default_healthcheck_timeout() -> u64 {
    1000
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum HealthcheckType {
    Http,
    Process,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConfig {
    pub name: String,

    #[serde(rename = "type")]
    pub tool_type: ToolType,

    #[serde(default)]
    pub description: String,

    // exec fields
    #[serde(default)]
    pub command: Option<String>,

    #[serde(default)]
    pub args: Vec<String>,

    #[serde(default)]
    pub cwd: Option<String>,

    #[serde(default)]
    pub env: HashMap<String, String>,

    // http fields
    #[serde(default)]
    pub method: Option<HttpMethod>,

    #[serde(default)]
    pub url: Option<String>,

    #[serde(default)]
    pub headers: HashMap<String, String>,

    #[serde(default)]
    pub body: Option<String>,

    // sql fields
    #[serde(default)]
    pub driver: Option<SqlDriver>,

    #[serde(default)]
    pub dsn: Option<String>,

    #[serde(default)]
    pub query: Option<String>,

    // common
    #[serde(default = "default_tool_timeout")]
    pub timeout_ms: u64,

    #[serde(default)]
    pub depends_on: Vec<String>,

    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default)]
    pub input: Option<InputSchema>,

    #[serde(default)]
    pub retry: Option<RetryConfig>,

    #[serde(default)]
    pub annotations: Option<ToolAnnotations>,
}

fn default_tool_timeout() -> u64 {
    30000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    #[serde(default = "default_retry_delay")]
    pub retry_delay_ms: u64,
}

fn default_max_retries() -> u32 {
    3
}

fn default_retry_delay() -> u64 {
    1000
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ToolType {
    Exec,
    Http,
    Sql,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum SqlDriver {
    Postgres,
    Sqlite,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolAnnotations {
    #[serde(default)]
    pub destructive: Option<bool>,
    #[serde(default)]
    pub read_only: Option<bool>,
    #[serde(default)]
    pub idempotent: Option<bool>,
    #[serde(default)]
    pub open_world: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceConfig {
    pub name: String,
    pub uri: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub resource_type: ResourceType,

    // For file resources
    #[serde(default)]
    pub path: Option<String>,

    // For exec resources
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,

    #[serde(default)]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceType {
    File,
    Exec,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputSchema {
    #[serde(rename = "type", default = "default_object_type")]
    pub schema_type: String,

    #[serde(default)]
    pub properties: HashMap<String, PropertyDef>,

    #[serde(default)]
    pub required: Vec<String>,
}

fn default_object_type() -> String {
    "object".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyDef {
    #[serde(rename = "type")]
    pub prop_type: String,

    #[serde(default)]
    pub description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_annotations() {
        let yaml = r#"
tools:
  - name: read_file
    type: exec
    command: cat
    args: ["{{path}}"]
    annotations:
      read_only: true
      destructive: false
      idempotent: true
      open_world: false
"#;
        let config: McpifyConfig = serde_yaml::from_str(yaml).unwrap();
        let ann = config.tools[0].annotations.as_ref().unwrap();
        assert_eq!(ann.read_only, Some(true));
        assert_eq!(ann.destructive, Some(false));
        assert_eq!(ann.idempotent, Some(true));
        assert_eq!(ann.open_world, Some(false));
    }

    #[test]
    fn test_deserialize_vars() {
        let yaml = r#"
vars:
  api_key: "${env:API_KEY}"
  base_url: "http://localhost:3000"
tools: []
"#;
        let config: McpifyConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.vars.len(), 2);
        assert_eq!(config.vars["base_url"], "http://localhost:3000");
    }

    #[test]
    fn test_deserialize_sql_tool() {
        let yaml = r#"
tools:
  - name: query_users
    type: sql
    description: Query users table
    driver: sqlite
    dsn: "sqlite::memory:"
    query: "SELECT * FROM users"
    timeout_ms: 5000
"#;
        let config: McpifyConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.tools[0].tool_type, ToolType::Sql);
        assert_eq!(config.tools[0].driver, Some(SqlDriver::Sqlite));
        assert_eq!(config.tools[0].dsn.as_deref(), Some("sqlite::memory:"));
    }

    #[test]
    fn test_deserialize_resources() {
        let yaml = r#"
resources:
  - name: readme
    type: file
    uri: "file:///README.md"
    path: "./README.md"
    mime_type: "text/markdown"
  - name: version
    type: exec
    uri: "mcpify://version"
    command: git
    args: ["describe", "--tags"]
tools: []
"#;
        let config: McpifyConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.resources.len(), 2);
        assert_eq!(config.resources[0].resource_type, ResourceType::File);
        assert_eq!(config.resources[1].resource_type, ResourceType::Exec);
    }

    #[test]
    fn test_deserialize_full_config() {
        let yaml = r#"
server:
  name: my-mcp-runtime
  transport: stdio
  log_level: info

supervisor:
  restart_policy: on-failure
  healthcheck_interval_ms: 3000
  graceful_shutdown_timeout_ms: 5000

services:
  - name: local_api
    command: ./bin/local-api
    args: ["--port", "3010"]
    cwd: .
    env:
      APP_ENV: development
    autostart: true
    restart: on-failure
    healthcheck:
      type: http
      url: http://127.0.0.1:3010/health
      interval_ms: 3000
      timeout_ms: 1000

tools:
  - name: git_status
    type: exec
    description: Show git status
    command: git
    args: ["status", "--short"]
    timeout_ms: 5000
    input:
      type: object
      properties: {}
      required: []

  - name: create_commit
    type: exec
    description: Create git commit
    command: git
    args: ["commit", "-m", "{{message}}"]
    timeout_ms: 10000
    input:
      type: object
      properties:
        message:
          type: string
      required: ["message"]

  - name: get_user
    type: http
    description: Get user by id from local api
    method: GET
    url: http://127.0.0.1:3010/users/{{id}}
    timeout_ms: 5000
    depends_on: ["local_api"]
    input:
      type: object
      properties:
        id:
          type: string
      required: ["id"]
"#;

        let config: McpifyConfig = serde_yaml::from_str(yaml).unwrap();

        assert_eq!(config.server.name, "my-mcp-runtime");
        assert_eq!(config.server.transport, "stdio");
        assert_eq!(config.services.len(), 1);
        assert_eq!(config.services[0].name, "local_api");
        assert_eq!(config.services[0].command, "./bin/local-api");
        assert!(config.services[0].healthcheck.is_some());

        assert_eq!(config.tools.len(), 3);
        assert_eq!(config.tools[0].name, "git_status");
        assert_eq!(config.tools[0].tool_type, ToolType::Exec);
        assert_eq!(config.tools[1].name, "create_commit");
        assert_eq!(config.tools[2].name, "get_user");
        assert_eq!(config.tools[2].tool_type, ToolType::Http);
        assert_eq!(config.tools[2].method, Some(HttpMethod::Get));
        assert_eq!(config.tools[2].depends_on, vec!["local_api"]);
    }

    #[test]
    fn test_deserialize_minimal_config() {
        let yaml = r#"
tools:
  - name: hello
    type: exec
    command: echo
    args: ["hello"]
"#;
        let config: McpifyConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.server.name, "mcpify");
        assert_eq!(config.tools.len(), 1);
        assert!(config.tools[0].enabled);
    }
}
