use crate::adapters::ToolResult;
use crate::config::model::ToolConfig;
use crate::template::render::{merge_vars, render_template};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio::process::Command;

pub async fn execute(
    tool: &ToolConfig,
    input: Value,
    config_vars: &HashMap<String, String>,
) -> Result<ToolResult> {
    let command = tool
        .command
        .as_ref()
        .context("exec tool missing 'command'")?;

    let vars = merge_vars(&input, config_vars);

    // Render each arg through template engine
    let mut rendered_args = Vec::with_capacity(tool.args.len());
    for arg in &tool.args {
        rendered_args.push(render_template(arg, &vars)?);
    }

    let mut cmd = Command::new(command);
    cmd.args(&rendered_args);

    // Set cwd if specified
    if let Some(cwd) = &tool.cwd {
        cmd.current_dir(cwd);
    }

    // Set env vars
    for (k, v) in &tool.env {
        cmd.env(k, v);
    }

    // Capture output
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let timeout = Duration::from_millis(tool.timeout_ms);

    let output = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| crate::errors::McpifyError::Timeout(tool.timeout_ms))?
        .with_context(|| format!("exec tool '{}': running {command}", tool.name))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code();
    let is_error = !output.status.success();

    Ok(ToolResult {
        stdout,
        stderr,
        exit_code,
        is_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{ToolConfig, ToolType};
    use serde_json::json;
    use std::collections::HashMap;

    fn make_exec_tool(command: &str, args: Vec<&str>, timeout_ms: u64) -> ToolConfig {
        ToolConfig {
            name: "test".to_string(),
            tool_type: ToolType::Exec,
            description: String::new(),
            command: Some(command.to_string()),
            args: args.into_iter().map(String::from).collect(),
            cwd: None,
            env: HashMap::new(),
            method: None,
            url: None,
            headers: HashMap::new(),
            body: None,
            driver: None,
            dsn: None,
            query: None,
            timeout_ms,
            depends_on: vec![],
            enabled: true,
            input: None,
            retry: None,
            annotations: None,
        }
    }

    fn empty_vars() -> HashMap<String, String> {
        HashMap::new()
    }

    #[tokio::test]
    async fn test_exec_echo() {
        let tool = make_exec_tool("echo", vec!["hello"], 5000);
        let result = execute(&tool, json!({}), &empty_vars()).await.unwrap();
        assert!(!result.is_error);
        assert_eq!(result.stdout.trim(), "hello");
    }

    #[tokio::test]
    async fn test_exec_with_template() {
        let tool = make_exec_tool("echo", vec!["{{msg}}"], 5000);
        let result = execute(&tool, json!({"msg": "world"}), &empty_vars())
            .await
            .unwrap();
        assert_eq!(result.stdout.trim(), "world");
    }

    #[tokio::test]
    async fn test_exec_with_config_vars() {
        let tool = make_exec_tool("echo", vec!["{{greeting}}"], 5000);
        let mut cv = HashMap::new();
        cv.insert("greeting".to_string(), "hola".to_string());
        let result = execute(&tool, json!({}), &cv).await.unwrap();
        assert_eq!(result.stdout.trim(), "hola");
    }

    #[tokio::test]
    async fn test_exec_timeout() {
        let tool = make_exec_tool("sleep", vec!["10"], 100);
        let result = execute(&tool, json!({}), &empty_vars()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
    }

    #[tokio::test]
    async fn test_exec_nonzero_exit() {
        let tool = make_exec_tool("false", vec![], 5000);
        let result = execute(&tool, json!({}), &empty_vars()).await.unwrap();
        assert!(result.is_error);
    }
}
