use crate::adapters::ToolResult;
use crate::config::model::{HttpMethod, ToolConfig};
use crate::template::render::{merge_vars, render_template};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

pub async fn execute(
    tool: &ToolConfig,
    input: Value,
    client: &reqwest::Client,
    config_vars: &HashMap<String, String>,
) -> Result<ToolResult> {
    let url_template = tool.url.as_ref().context("http tool missing 'url'")?;
    let method = tool.method.as_ref().context("http tool missing 'method'")?;

    let vars = merge_vars(&input, config_vars);
    let url = render_template(url_template, &vars)?;
    let timeout = Duration::from_millis(tool.timeout_ms);

    // Pre-render headers and body (they don't change between retries)
    let rendered_headers = render_headers(&tool.headers, &vars)?;
    let rendered_body = match &tool.body {
        Some(tpl) => Some(render_template(tpl, &vars)?),
        None => None,
    };

    let max_attempts = match &tool.retry {
        Some(retry) => 1 + retry.max_retries,
        None => 1,
    };
    let retry_delay = tool
        .retry
        .as_ref()
        .map(|r| Duration::from_millis(r.retry_delay_ms))
        .unwrap_or_default();

    let mut last_err = None;

    for attempt in 1..=max_attempts {
        let mut request = match method {
            HttpMethod::Get => client.get(&url),
            HttpMethod::Post => client.post(&url),
            HttpMethod::Put => client.put(&url),
            HttpMethod::Patch => client.patch(&url),
            HttpMethod::Delete => client.delete(&url),
        };

        request = request.timeout(timeout);

        for (k, v) in &rendered_headers {
            request = request.header(k, v);
        }

        if let Some(body) = &rendered_body {
            request = request
                .header("content-type", "application/json")
                .body(body.clone());
        }

        match request.send().await {
            Ok(response) => {
                let status = response.status();
                let is_error = !status.is_success();
                let body = response.text().await.unwrap_or_default();

                // Don't retry on successful responses or client errors (4xx)
                if !is_error || status.is_client_error() {
                    return Ok(ToolResult {
                        stdout: body,
                        stderr: if is_error {
                            format!("HTTP {status}")
                        } else {
                            String::new()
                        },
                        exit_code: Some(status.as_u16() as i32),
                        is_error,
                    });
                }

                // Server error (5xx) — retry if allowed
                if attempt < max_attempts {
                    tracing::warn!(
                        tool = %tool.name,
                        attempt,
                        status = %status,
                        "retrying after server error"
                    );
                    tokio::time::sleep(retry_delay).await;
                    last_err = Some(format!("HTTP {status}"));
                    continue;
                }

                return Ok(ToolResult {
                    stdout: body,
                    stderr: format!("HTTP {status}"),
                    exit_code: Some(status.as_u16() as i32),
                    is_error: true,
                });
            }
            Err(e) => {
                if attempt < max_attempts {
                    tracing::warn!(
                        tool = %tool.name,
                        attempt,
                        error = %e,
                        "retrying after request error"
                    );
                    tokio::time::sleep(retry_delay).await;
                    last_err = Some(e.to_string());
                    continue;
                }
                return Err(e).with_context(|| {
                    format!("http tool '{}': request to {url} failed", tool.name)
                });
            }
        }
    }

    anyhow::bail!(
        "http tool '{}': all {} attempts failed: {}",
        tool.name,
        max_attempts,
        last_err.unwrap_or_default()
    )
}

fn render_headers(
    headers: &HashMap<String, String>,
    vars: &HashMap<String, Value>,
) -> Result<Vec<(String, String)>> {
    let mut result = Vec::with_capacity(headers.len());
    for (k, v) in headers {
        result.push((k.clone(), render_template(v, vars)?));
    }
    Ok(result)
}
