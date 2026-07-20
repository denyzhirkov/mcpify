pub mod exec;
pub mod http;
pub mod sql;

use serde_json::Value;

/// Result of executing a tool.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub is_error: bool,
    /// Structured payload for MCP `structuredContent`, when the adapter can
    /// produce one natively (sql rows) or `output.parse: json` yields it.
    pub structured: Option<Value>,
}
