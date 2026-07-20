use crate::config::model::McpifyConfig;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const CONFIG_CANDIDATES: &[&str] = &["mcpify.yaml", "mcpify.yml", ".mcpify/mcpify.yaml"];

pub fn find_config_file() -> Result<PathBuf> {
    for candidate in CONFIG_CANDIDATES {
        let path = Path::new(candidate);
        if path.exists() {
            return Ok(path.to_path_buf());
        }
    }
    anyhow::bail!(
        "config file not found; tried: {}",
        CONFIG_CANDIDATES.join(", ")
    )
}

/// Resolve source references in var values:
/// - `${env:NAME}` — environment variable, else empty (+ warn)
/// - `${env:NAME:-default}` — environment variable if set & non-empty, else `default`
/// - `${file:path}` — file contents (trailing whitespace trimmed), else empty (+ warn)
pub fn resolve_vars(vars: &mut std::collections::HashMap<String, String>) {
    for value in vars.values_mut() {
        if let Some(inner) = value
            .strip_prefix("${env:")
            .and_then(|s| s.strip_suffix('}'))
        {
            let (name, default) = match inner.split_once(":-") {
                Some((name, default)) => (name, Some(default)),
                None => (inner, None),
            };
            *value = match std::env::var(name) {
                Ok(env_val) if !env_val.is_empty() => env_val,
                _ => match default {
                    Some(d) => d.to_string(),
                    None => {
                        tracing::warn!(var = name, "env var not found, leaving empty");
                        String::new()
                    }
                },
            };
        } else if let Some(path) = value
            .strip_prefix("${file:")
            .and_then(|s| s.strip_suffix('}'))
        {
            *value = match std::fs::read_to_string(path) {
                Ok(contents) => contents.trim_end().to_string(),
                Err(e) => {
                    tracing::warn!(path, error = %e, "file var not readable, leaving empty");
                    String::new()
                }
            };
        }
    }
}

pub fn load_config(path: Option<&Path>) -> Result<McpifyConfig> {
    let config_path = match path {
        Some(p) => p.to_path_buf(),
        None => find_config_file()?,
    };
    let content = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading {config_path:?}"))?;
    let mut config: McpifyConfig =
        serde_yaml::from_str(&content).with_context(|| format!("parsing {config_path:?}"))?;
    resolve_vars(&mut config.vars);
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_load_config_from_path() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(
            f,
            r#"
tools:
  - name: hello
    type: exec
    command: echo
    args: ["world"]
"#
        )
        .unwrap();
        let config = load_config(Some(f.path())).unwrap();
        assert_eq!(config.tools.len(), 1);
        assert_eq!(config.tools[0].name, "hello");
    }

    #[test]
    fn test_load_invalid_yaml() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "{{{{invalid yaml").unwrap();
        assert!(load_config(Some(f.path())).is_err());
    }

    #[test]
    fn test_resolve_vars_env() {
        // SAFETY: test runs single-threaded; no concurrent env access
        unsafe { std::env::set_var("MCPIFY_TEST_VAR", "secret123") };
        let mut vars = std::collections::HashMap::new();
        vars.insert("key".to_string(), "${env:MCPIFY_TEST_VAR}".to_string());
        vars.insert("plain".to_string(), "hello".to_string());
        resolve_vars(&mut vars);
        assert_eq!(vars["key"], "secret123");
        assert_eq!(vars["plain"], "hello");
        unsafe { std::env::remove_var("MCPIFY_TEST_VAR") };
    }

    #[test]
    fn test_resolve_vars_missing_env() {
        let mut vars = std::collections::HashMap::new();
        vars.insert(
            "missing".to_string(),
            "${env:MCPIFY_NONEXISTENT_VAR_XYZ}".to_string(),
        );
        resolve_vars(&mut vars);
        assert_eq!(vars["missing"], "");
    }

    #[test]
    fn test_resolve_vars_env_default() {
        let mut vars = std::collections::HashMap::new();
        vars.insert(
            "unset".to_string(),
            "${env:MCPIFY_NOPE_XYZ:-fallback}".to_string(),
        );
        // SAFETY: test runs single-threaded; no concurrent env access
        unsafe { std::env::set_var("MCPIFY_SET_VAR", "real") };
        vars.insert(
            "set".to_string(),
            "${env:MCPIFY_SET_VAR:-fallback}".to_string(),
        );
        resolve_vars(&mut vars);
        assert_eq!(vars["unset"], "fallback");
        assert_eq!(vars["set"], "real");
        unsafe { std::env::remove_var("MCPIFY_SET_VAR") };
    }

    #[test]
    fn test_resolve_vars_file() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "token-from-file").unwrap();
        let mut vars = std::collections::HashMap::new();
        vars.insert(
            "tok".to_string(),
            format!("${{file:{}}}", f.path().display()),
        );
        resolve_vars(&mut vars);
        assert_eq!(vars["tok"], "token-from-file"); // trailing newline trimmed
    }
}
