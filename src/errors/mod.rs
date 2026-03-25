use thiserror::Error;

#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum McpifyError {
    #[error("failed to load config: {0}")]
    ConfigLoad(String),

    #[error("config validation error: {0}")]
    ConfigValidation(String),

    #[error("template render error: {0}")]
    TemplateRender(String),

    #[error("exec failed: {0}")]
    ExecFailed(String),

    #[error("http request failed: {0}")]
    HttpFailed(String),

    #[error("timeout after {0}ms")]
    Timeout(u64),

    #[error("tool not found: {0}")]
    ToolNotFound(String),

    #[error("dependency not ready: {0}")]
    DependencyNotReady(String),

    #[error("{0}")]
    Internal(String),
}
