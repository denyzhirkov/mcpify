use crate::adapters::ToolResult;
use crate::config::model::{AuthConfig, HttpMethod, ToolConfig};
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

    // Pre-render headers, query, auth and body (they don't change between retries)
    let rendered_headers = render_headers(&tool.headers, &vars)?;
    let rendered_query = render_query(&tool.query_params, &vars)?;
    let rendered_auth = render_auth(tool.auth.as_ref(), &vars)?;
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

        if !rendered_query.is_empty() {
            request = request.query(&rendered_query);
        }

        request = match &rendered_auth {
            Some(AppliedAuth::Bearer(token)) => request.bearer_auth(token),
            Some(AppliedAuth::Basic { username, password }) => {
                request.basic_auth(username, Some(password))
            }
            Some(AppliedAuth::ApiKey { header, value }) => request.header(header, value),
            None => request,
        };

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
                        structured: None,
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
                    structured: None,
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

fn render_query(
    query: &HashMap<String, String>,
    vars: &HashMap<String, Value>,
) -> Result<Vec<(String, String)>> {
    let mut result = Vec::with_capacity(query.len());
    for (k, v) in query {
        result.push((k.clone(), render_template(v, vars)?));
    }
    Ok(result)
}

/// Auth with its `{{var}}` fields resolved, ready to apply to the request.
enum AppliedAuth {
    Bearer(String),
    Basic { username: String, password: String },
    ApiKey { header: String, value: String },
}

fn render_auth(
    auth: Option<&AuthConfig>,
    vars: &HashMap<String, Value>,
) -> Result<Option<AppliedAuth>> {
    let applied = match auth {
        None => None,
        Some(AuthConfig::Bearer { token }) => {
            Some(AppliedAuth::Bearer(render_template(token, vars)?))
        }
        Some(AuthConfig::Basic { username, password }) => Some(AppliedAuth::Basic {
            username: render_template(username, vars)?,
            password: render_template(password, vars)?,
        }),
        Some(AuthConfig::ApiKey { header, value }) => Some(AppliedAuth::ApiKey {
            header: render_template(header, vars)?,
            value: render_template(value, vars)?,
        }),
    };
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect()
    }

    #[test]
    fn test_render_query_substitutes() {
        let mut q = HashMap::new();
        q.insert("page".to_string(), "{{page}}".to_string());
        q.insert("limit".to_string(), "10".to_string());
        let mut out = render_query(&q, &vars(&[("page", "3")])).unwrap();
        out.sort();
        assert_eq!(
            out,
            vec![
                ("limit".to_string(), "10".to_string()),
                ("page".to_string(), "3".to_string())
            ]
        );
    }

    #[test]
    fn test_render_auth_bearer() {
        let auth = AuthConfig::Bearer {
            token: "{{tok}}".to_string(),
        };
        match render_auth(Some(&auth), &vars(&[("tok", "abc123")])).unwrap() {
            Some(AppliedAuth::Bearer(t)) => assert_eq!(t, "abc123"),
            _ => panic!("expected bearer"),
        }
    }

    #[test]
    fn test_render_auth_basic_and_apikey() {
        let basic = AuthConfig::Basic {
            username: "u".to_string(),
            password: "{{pw}}".to_string(),
        };
        match render_auth(Some(&basic), &vars(&[("pw", "s3cret")])).unwrap() {
            Some(AppliedAuth::Basic { username, password }) => {
                assert_eq!(username, "u");
                assert_eq!(password, "s3cret");
            }
            _ => panic!("expected basic"),
        }

        let apikey = AuthConfig::ApiKey {
            header: "X-API-Key".to_string(),
            value: "{{k}}".to_string(),
        };
        match render_auth(Some(&apikey), &vars(&[("k", "keyval")])).unwrap() {
            Some(AppliedAuth::ApiKey { header, value }) => {
                assert_eq!(header, "X-API-Key");
                assert_eq!(value, "keyval");
            }
            _ => panic!("expected api-key"),
        }
    }
}
