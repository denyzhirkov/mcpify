use crate::adapters::{self, ToolResult};
use crate::config::model::{ToolConfig, ToolType};
use crate::runtime::app_state::AppState;
use crate::template::render::render_template;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// Run a `pipeline` tool: execute its steps in order, each referencing an
/// existing tool. Later steps read earlier outputs via `{{steps.<id>.<field>}}`.
/// Fail-fast; the pipeline result is the last step's output.
pub async fn execute(
    state: &Arc<AppState>,
    config: &ToolConfig,
    input: Value,
) -> Result<ToolResult> {
    if config.steps.is_empty() {
        bail!("pipeline '{}' has no steps", config.name);
    }

    let config_vars = state.vars.read().await.clone();
    let mut steps_out = Map::new();
    let mut last: Option<ToolResult> = None;

    for step in &config.steps {
        let step_tool = {
            let registry = state.registry.read().await;
            registry.get(&step.tool).map(|e| e.config.clone())
        };
        let step_tool = step_tool.ok_or_else(|| {
            anyhow!(
                "pipeline '{}': step references unknown tool '{}'",
                config.name,
                step.tool
            )
        })?;
        if step_tool.tool_type == ToolType::Pipeline {
            bail!(
                "pipeline '{}': step '{}' references another pipeline (nesting is not supported)",
                config.name,
                step.tool
            );
        }

        // Render context: pipeline input + config vars + prior step outputs.
        let mut ctx: HashMap<String, Value> = HashMap::new();
        for (k, v) in &config_vars {
            ctx.insert(k.clone(), Value::String(v.clone()));
        }
        if let Value::Object(m) = &input {
            for (k, v) in m {
                ctx.insert(k.clone(), v.clone());
            }
        }
        ctx.insert("steps".to_string(), Value::Object(steps_out.clone()));

        // Empty input mapping → pass the pipeline input through unchanged.
        let step_input = if step.input.is_empty() {
            input.clone()
        } else {
            let mut map = Map::new();
            for (k, tpl) in &step.input {
                let rendered = render_template(tpl, &ctx).with_context(|| {
                    format!(
                        "pipeline '{}': rendering step '{}' input '{}'",
                        config.name, step.tool, k
                    )
                })?;
                map.insert(k.clone(), Value::String(rendered));
            }
            Value::Object(map)
        };

        let result = run_leaf(state, &step_tool, step_input, &config_vars)
            .await
            .with_context(|| format!("pipeline '{}': step '{}' failed", config.name, step.tool))?;
        if result.is_error {
            bail!(
                "pipeline '{}': step '{}' returned an error: {}",
                config.name,
                step.tool,
                result.stderr
            );
        }

        let id = step.id.clone().unwrap_or_else(|| step.tool.clone());
        steps_out.insert(id, step_output_value(&result));
        last = Some(result);
    }

    last.ok_or_else(|| anyhow!("pipeline '{}' produced no result", config.name))
}

/// The value a step exposes to later steps: its structured content, else its
/// stdout parsed as JSON, else the raw stdout string.
fn step_output_value(result: &ToolResult) -> Value {
    if let Some(structured) = &result.structured {
        return structured.clone();
    }
    serde_json::from_str::<Value>(&result.stdout)
        .unwrap_or_else(|_| Value::String(result.stdout.clone()))
}

async fn run_leaf(
    state: &Arc<AppState>,
    tool: &ToolConfig,
    input: Value,
    config_vars: &HashMap<String, String>,
) -> Result<ToolResult> {
    match tool.tool_type {
        ToolType::Exec => adapters::exec::execute(tool, input, config_vars).await,
        ToolType::Http => {
            adapters::http::execute(tool, input, &state.http_client, config_vars).await
        }
        ToolType::Sql => adapters::sql::execute(tool, input, config_vars).await,
        ToolType::Pipeline => bail!("nested pipeline execution is not supported"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::McpifyConfig;
    use crate::runtime::registry::ToolRegistry;
    use crate::supervisor::manager::SupervisorManager;
    use serde_json::json;

    fn state_from(yaml: &str) -> Arc<AppState> {
        let config: McpifyConfig = serde_yaml::from_str(yaml).unwrap();
        let registry = ToolRegistry::from_config(&config);
        let supervisor = SupervisorManager::from_config(&config);
        Arc::new(AppState::new(config, registry, supervisor))
    }

    #[tokio::test]
    async fn test_pipeline_chains_step_output() {
        let state = state_from(
            r#"
tools:
  - name: emit
    type: exec
    command: printf
    args: ['{"name":"bob"}']
  - name: greet
    type: exec
    command: echo
    args: ["hello {{name}}"]
  - name: flow
    type: pipeline
    steps:
      - id: u
        tool: emit
      - tool: greet
        input:
          name: "{{steps.u.name}}"
"#,
        );
        let config = state
            .registry
            .read()
            .await
            .get("flow")
            .unwrap()
            .config
            .clone();
        let result = execute(&state, &config, json!({})).await.unwrap();
        assert!(!result.is_error);
        assert_eq!(result.stdout.trim(), "hello bob");
    }
}
