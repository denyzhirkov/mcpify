use crate::config::model::{McpifyConfig, ToolType};
use anyhow::Result;
use std::collections::HashSet;

#[derive(Debug)]
pub struct ValidationWarning {
    pub message: String,
}

pub fn validate(config: &McpifyConfig) -> Result<Vec<ValidationWarning>> {
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    // Check tool name uniqueness
    let mut tool_names = HashSet::new();
    for tool in &config.tools {
        if !tool_names.insert(&tool.name) {
            errors.push(format!("duplicate tool name: {}", tool.name));
        }
    }

    // Check service name uniqueness
    let mut service_names = HashSet::new();
    for svc in &config.services {
        if !service_names.insert(&svc.name) {
            errors.push(format!("duplicate service name: {}", svc.name));
        }
    }

    // Validate each tool
    for tool in &config.tools {
        if tool.name.is_empty() {
            errors.push("tool has empty name".to_string());
        }

        if tool.timeout_ms == 0 {
            errors.push(format!("tool '{}': timeout_ms must be > 0", tool.name));
        }

        match tool.tool_type {
            ToolType::Exec => {
                if tool.command.is_none() {
                    errors.push(format!("exec tool '{}': missing 'command'", tool.name));
                }
            }
            ToolType::Http => {
                if tool.url.is_none() {
                    errors.push(format!("http tool '{}': missing 'url'", tool.name));
                }
                if tool.method.is_none() {
                    errors.push(format!("http tool '{}': missing 'method'", tool.name));
                }
            }
        }

        // Check depends_on references existing services
        for dep in &tool.depends_on {
            if !service_names.contains(dep) {
                errors.push(format!(
                    "tool '{}': depends_on '{}' — service not found",
                    tool.name, dep
                ));
            }
        }
    }

    // Validate services
    for svc in &config.services {
        if svc.name.is_empty() {
            errors.push("service has empty name".to_string());
        }
        if svc.command.is_empty() {
            errors.push(format!("service '{}': missing 'command'", svc.name));
        }
        if let Some(hc) = &svc.healthcheck
            && hc.check_type == crate::config::model::HealthcheckType::Http
            && hc.url.is_none()
        {
            errors.push(format!(
                "service '{}': http healthcheck requires 'url'",
                svc.name
            ));
        }
    }

    // Warn about tools with no input schema
    for tool in &config.tools {
        if tool.input.is_none() {
            warnings.push(ValidationWarning {
                message: format!("tool '{}': no input schema defined", tool.name),
            });
        }
    }

    if errors.is_empty() {
        Ok(warnings)
    } else {
        anyhow::bail!("config validation failed:\n  - {}", errors.join("\n  - "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::McpifyConfig;

    fn parse(yaml: &str) -> McpifyConfig {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn test_valid_config() {
        let config = parse(
            r#"
tools:
  - name: hello
    type: exec
    command: echo
    timeout_ms: 5000
"#,
        );
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn test_duplicate_tool_names() {
        let config = parse(
            r#"
tools:
  - name: hello
    type: exec
    command: echo
  - name: hello
    type: exec
    command: ls
"#,
        );
        let err = validate(&config).unwrap_err();
        assert!(err.to_string().contains("duplicate tool name"));
    }

    #[test]
    fn test_exec_missing_command() {
        let config = parse(
            r#"
tools:
  - name: broken
    type: exec
"#,
        );
        let err = validate(&config).unwrap_err();
        assert!(err.to_string().contains("missing 'command'"));
    }

    #[test]
    fn test_http_missing_url() {
        let config = parse(
            r#"
tools:
  - name: broken
    type: http
    method: GET
"#,
        );
        let err = validate(&config).unwrap_err();
        assert!(err.to_string().contains("missing 'url'"));
    }

    #[test]
    fn test_depends_on_unknown_service() {
        let config = parse(
            r#"
tools:
  - name: api_call
    type: http
    method: GET
    url: http://localhost/test
    depends_on: ["nonexistent"]
"#,
        );
        let err = validate(&config).unwrap_err();
        assert!(err.to_string().contains("service not found"));
    }
}
