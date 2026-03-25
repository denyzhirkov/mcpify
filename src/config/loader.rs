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

pub fn load_config(path: Option<&Path>) -> Result<McpifyConfig> {
    let config_path = match path {
        Some(p) => p.to_path_buf(),
        None => find_config_file()?,
    };
    let content = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading {config_path:?}"))?;
    let config: McpifyConfig =
        serde_yaml::from_str(&content).with_context(|| format!("parsing {config_path:?}"))?;
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
}
