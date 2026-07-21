use crate::config::model::{McpifyConfig, PropertyDef, ResourceType, ToolType};
use anyhow::Result;
use std::collections::HashSet;

/// JSON Schema keywords accepted in an input property's pass-through `extra`
/// (the first-class fields — type/enum/default/items/properties/required/
/// description — never land here). Anything else is flagged as a likely typo.
const KNOWN_SCHEMA_KEYWORDS: &[&str] = &[
    // annotations / generic
    "title",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
    "const",
    "$ref",
    "$comment",
    "allOf",
    "anyOf",
    "oneOf",
    "not",
    // numeric
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    // string
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "contentEncoding",
    "contentMediaType",
    // array
    "minItems",
    "maxItems",
    "uniqueItems",
    "contains",
    "minContains",
    "maxContains",
    "prefixItems",
    // object
    "additionalProperties",
    "patternProperties",
    "minProperties",
    "maxProperties",
    "propertyNames",
    "dependentRequired",
    "dependentSchemas",
];

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
            ToolType::Sql => {
                if tool.driver.is_none() {
                    errors.push(format!("sql tool '{}': missing 'driver'", tool.name));
                }
                if tool.dsn.is_none() {
                    errors.push(format!("sql tool '{}': missing 'dsn'", tool.name));
                }
                if let Some(query) = &tool.query {
                    check_sql_bind_placeholders(&tool.name, query, &mut errors);
                } else {
                    errors.push(format!("sql tool '{}': missing 'query'", tool.name));
                }
            }
            ToolType::Pipeline => {
                if tool.steps.is_empty() {
                    errors.push(format!("pipeline tool '{}': no steps", tool.name));
                }
                for step in &tool.steps {
                    match config.tools.iter().find(|t| t.name == step.tool) {
                        None => errors.push(format!(
                            "pipeline tool '{}': step references unknown tool '{}'",
                            tool.name, step.tool
                        )),
                        Some(t) if t.tool_type == ToolType::Pipeline => errors.push(format!(
                            "pipeline tool '{}': step '{}' references another pipeline (not supported)",
                            tool.name, step.tool
                        )),
                        Some(_) => {}
                    }
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

        // Validate background cache config.
        if let Some(cache) = &tool.cache {
            if cache.refresh_interval_ms == 0 {
                errors.push(format!(
                    "cached tool '{}': cache.refresh_interval_ms must be > 0",
                    tool.name
                ));
            }
            if tool.tool_type == ToolType::Pipeline {
                errors.push(format!(
                    "cached tool '{}': caching a pipeline is not supported",
                    tool.name
                ));
            }
            let destructive = tool
                .annotations
                .as_ref()
                .and_then(|a| a.destructive)
                .unwrap_or(false);
            if destructive {
                errors.push(format!(
                    "cached tool '{}': a destructive tool cannot be cached (it would run on a timer)",
                    tool.name
                ));
            }
            if cache.refresh_interval_ms > 0 && cache.refresh_interval_ms < 1000 {
                warnings.push(ValidationWarning {
                    message: format!(
                        "cached tool '{}': refresh_interval_ms={} is very small; the action runs unattended on this interval",
                        tool.name, cache.refresh_interval_ms
                    ),
                });
            }
            if tool
                .input
                .as_ref()
                .is_some_and(|s| !s.properties.is_empty())
            {
                warnings.push(ValidationWarning {
                    message: format!(
                        "cached tool '{}': input properties are ignored — cached tools are parameterless (the background fetch uses config vars + empty input)",
                        tool.name
                    ),
                });
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

    // Validate resources
    for res in &config.resources {
        if res.uri.is_empty() {
            errors.push(format!("resource '{}': empty URI", res.name));
        }
        match res.resource_type {
            ResourceType::File => {
                if res.path.is_none() {
                    errors.push(format!("file resource '{}': missing 'path'", res.name));
                }
            }
            ResourceType::Exec => {
                if res.command.is_none() {
                    errors.push(format!("exec resource '{}': missing 'command'", res.name));
                }
            }
        }
    }

    // Warn about unresolved env var references in vars
    for (name, value) in &config.vars {
        if value.starts_with("${env:") && value.ends_with('}') {
            // This means resolve_vars didn't replace it (shouldn't happen),
            // but warn if the value is empty (env var was missing)
            warnings.push(ValidationWarning {
                message: format!("var '{}': env reference was not resolved", name),
            });
        }
        if value.is_empty() {
            warnings.push(ValidationWarning {
                message: format!("var '{}': resolved to empty string", name),
            });
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

    // Lint input schemas for unknown (likely mistyped) JSON Schema keywords.
    for tool in &config.tools {
        if let Some(schema) = &tool.input {
            for (name, def) in &schema.properties {
                lint_property(&tool.name, name, def, &mut warnings);
            }
        }
    }

    if errors.is_empty() {
        Ok(warnings)
    } else {
        anyhow::bail!("config validation failed:\n  - {}", errors.join("\n  - "))
    }
}

/// Recursively flag `extra` keys that aren't recognized JSON Schema keywords —
/// the pass-through `#[serde(flatten)]` accepts anything, so a typo like
/// `mimimum` would otherwise reach the client as a silent no-op.
fn lint_property(tool: &str, path: &str, def: &PropertyDef, warnings: &mut Vec<ValidationWarning>) {
    for key in def.extra.keys() {
        if !KNOWN_SCHEMA_KEYWORDS.contains(&key.as_str()) {
            warnings.push(ValidationWarning {
                message: format!(
                    "tool '{tool}': input property '{path}': unknown JSON Schema keyword '{key}'"
                ),
            });
        }
    }
    if let Some(items) = &def.items {
        lint_property(tool, &format!("{path}[]"), items, warnings);
    }
    for (name, nested) in &def.properties {
        lint_property(tool, &format!("{path}.{name}"), nested, warnings);
    }
}

/// A bind placeholder (`{{var}}`, not `{{raw:...}}`) wrapped in a quote is a
/// leftover from the old string-interpolation style — under bind parameters it
/// would emit a literal `'?'`. Flag it with a migration hint.
fn check_sql_bind_placeholders(tool_name: &str, query: &str, errors: &mut Vec<String>) {
    let bytes = query.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let start = i + 2;
            let Some(rel) = query[start..].find("}}") else {
                break; // unclosed — runtime reports it
            };
            let key = query[start..start + rel].trim();
            let is_raw = key.starts_with("raw:");
            let prev = query[..i].chars().last();
            if !is_raw && matches!(prev, Some('\'') | Some('"')) {
                errors.push(format!(
                    "sql tool '{tool_name}': bind placeholder {{{{{key}}}}} is wrapped in quotes — remove the quotes, values are bound automatically"
                ));
            }
            i = start + rel + 2;
        } else {
            i += 1;
        }
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

    #[test]
    fn test_sql_missing_driver() {
        let config = parse(
            r#"
tools:
  - name: broken
    type: sql
    dsn: "sqlite::memory:"
    query: "SELECT 1"
"#,
        );
        let err = validate(&config).unwrap_err();
        assert!(err.to_string().contains("missing 'driver'"));
    }

    #[test]
    fn test_sql_missing_dsn() {
        let config = parse(
            r#"
tools:
  - name: broken
    type: sql
    driver: sqlite
    query: "SELECT 1"
"#,
        );
        let err = validate(&config).unwrap_err();
        assert!(err.to_string().contains("missing 'dsn'"));
    }

    #[test]
    fn test_lint_unknown_schema_keyword() {
        let config = parse(
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
          mimimum: 0
"#,
        );
        let warnings = validate(&config).unwrap(); // typo is a warning, not an error
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("mimimum") && w.message.contains("age"))
        );
    }

    #[test]
    fn test_lint_known_keyword_and_nested_ok() {
        let config = parse(
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
        tags:
          type: array
          items:
            type: string
            minLength: 1
"#,
        );
        let warnings = validate(&config).unwrap();
        assert!(
            !warnings
                .iter()
                .any(|w| w.message.contains("unknown JSON Schema keyword"))
        );
    }

    #[test]
    fn test_lint_nested_typo_caught() {
        let config = parse(
            r#"
tools:
  - name: t
    type: exec
    command: echo
    input:
      type: object
      properties:
        tags:
          type: array
          items:
            type: string
            minLenght: 1
"#,
        );
        let warnings = validate(&config).unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("minLenght") && w.message.contains("tags[]"))
        );
    }

    #[test]
    fn test_pipeline_no_steps() {
        let config = parse(
            r#"
tools:
  - name: p
    type: pipeline
"#,
        );
        let err = validate(&config).unwrap_err();
        assert!(err.to_string().contains("no steps"));
    }

    #[test]
    fn test_pipeline_unknown_step_tool() {
        let config = parse(
            r#"
tools:
  - name: p
    type: pipeline
    steps:
      - tool: does_not_exist
"#,
        );
        let err = validate(&config).unwrap_err();
        assert!(err.to_string().contains("unknown tool"));
    }

    #[test]
    fn test_pipeline_valid() {
        let config = parse(
            r#"
tools:
  - name: a
    type: exec
    command: echo
  - name: p
    type: pipeline
    steps:
      - tool: a
"#,
        );
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn test_resource_file_missing_path() {
        let config = parse(
            r#"
resources:
  - name: readme
    type: file
    uri: "file:///README.md"
tools: []
"#,
        );
        let err = validate(&config).unwrap_err();
        assert!(err.to_string().contains("missing 'path'"));
    }

    #[test]
    fn test_sql_quoted_bind_placeholder_rejected() {
        let config = parse(
            r#"
tools:
  - name: q
    type: sql
    driver: sqlite
    dsn: "sqlite::memory:"
    query: "SELECT * FROM t WHERE name = '{{name}}'"
"#,
        );
        let err = validate(&config).unwrap_err();
        assert!(err.to_string().contains("wrapped in quotes"));
    }

    #[test]
    fn test_sql_unquoted_bind_and_raw_ok() {
        let config = parse(
            r#"
tools:
  - name: q
    type: sql
    driver: sqlite
    dsn: "sqlite::memory:"
    query: "SELECT * FROM {{raw:table}} WHERE name = {{name}}"
"#,
        );
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn test_cache_on_destructive_rejected() {
        let config = parse(
            r#"
tools:
  - name: t
    type: exec
    command: echo
    cache:
      refresh_interval_ms: 5000
    annotations:
      destructive: true
"#,
        );
        let err = validate(&config).unwrap_err();
        assert!(
            err.to_string()
                .contains("destructive tool cannot be cached")
        );
    }

    #[test]
    fn test_cache_on_pipeline_rejected() {
        let config = parse(
            r#"
tools:
  - name: a
    type: exec
    command: echo
  - name: p
    type: pipeline
    steps:
      - tool: a
    cache:
      refresh_interval_ms: 5000
"#,
        );
        let err = validate(&config).unwrap_err();
        assert!(
            err.to_string()
                .contains("caching a pipeline is not supported")
        );
    }

    #[test]
    fn test_cache_zero_interval_rejected() {
        let config = parse(
            r#"
tools:
  - name: t
    type: exec
    command: echo
    cache:
      refresh_interval_ms: 0
"#,
        );
        let err = validate(&config).unwrap_err();
        assert!(err.to_string().contains("refresh_interval_ms must be > 0"));
    }

    #[test]
    fn test_cache_small_interval_and_input_props_warn() {
        let config = parse(
            r#"
tools:
  - name: t
    type: exec
    command: echo
    cache:
      refresh_interval_ms: 100
    input:
      type: object
      properties:
        q:
          type: string
"#,
        );
        let warnings = validate(&config).unwrap();
        assert!(warnings.iter().any(|w| w.message.contains("very small")));
        assert!(warnings.iter().any(|w| w.message.contains("parameterless")));
    }

    #[test]
    fn test_cache_valid() {
        let config = parse(
            r#"
tools:
  - name: t
    type: http
    method: GET
    url: "http://localhost/x"
    cache:
      refresh_interval_ms: 30000
"#,
        );
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn test_resource_exec_missing_command() {
        let config = parse(
            r#"
resources:
  - name: version
    type: exec
    uri: "mcpify://version"
tools: []
"#,
        );
        let err = validate(&config).unwrap_err();
        assert!(err.to_string().contains("missing 'command'"));
    }
}
