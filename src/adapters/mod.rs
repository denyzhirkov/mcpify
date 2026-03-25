pub mod exec;
pub mod http;
pub mod sql;

/// Result of executing a tool.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub is_error: bool,
}
