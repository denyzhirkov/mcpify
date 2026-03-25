use crate::config::model::{McpifyConfig, ToolConfig};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum ToolAvailability {
    Enabled,
    Disabled,
    BlockedByDependency(String),
}

#[derive(Debug, Clone)]
pub struct ToolEntry {
    pub config: ToolConfig,
    pub availability: ToolAvailability,
}

#[derive(Debug)]
pub struct ToolRegistry {
    tools: HashMap<String, ToolEntry>,
}

impl ToolRegistry {
    pub fn from_config(config: &McpifyConfig) -> Self {
        let mut tools = HashMap::new();
        for tool in &config.tools {
            let availability = if tool.enabled {
                ToolAvailability::Enabled
            } else {
                ToolAvailability::Disabled
            };
            tools.insert(
                tool.name.clone(),
                ToolEntry {
                    config: tool.clone(),
                    availability,
                },
            );
        }
        Self { tools }
    }

    pub fn get(&self, name: &str) -> Option<&ToolEntry> {
        self.tools.get(name)
    }

    pub fn list(&self) -> Vec<&ToolEntry> {
        let mut entries: Vec<_> = self.tools.values().collect();
        entries.sort_by(|a, b| a.config.name.cmp(&b.config.name));
        entries
    }

    #[allow(dead_code)]
    pub fn add(&mut self, tool: ToolConfig) {
        let availability = if tool.enabled {
            ToolAvailability::Enabled
        } else {
            ToolAvailability::Disabled
        };
        self.tools.insert(
            tool.name.clone(),
            ToolEntry {
                config: tool,
                availability,
            },
        );
    }

    #[allow(dead_code)]
    pub fn remove(&mut self, name: &str) -> Option<ToolEntry> {
        self.tools.remove(name)
    }

    #[allow(dead_code)]
    pub fn set_availability(&mut self, name: &str, availability: ToolAvailability) {
        if let Some(entry) = self.tools.get_mut(name) {
            entry.availability = availability;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::McpifyConfig;

    fn test_config() -> McpifyConfig {
        serde_yaml::from_str(
            r#"
tools:
  - name: alpha
    type: exec
    command: echo
  - name: beta
    type: exec
    command: ls
    enabled: false
"#,
        )
        .unwrap()
    }

    #[test]
    fn test_from_config() {
        let registry = ToolRegistry::from_config(&test_config());
        assert_eq!(registry.list().len(), 2);
        assert_eq!(
            registry.get("alpha").unwrap().availability,
            ToolAvailability::Enabled
        );
        assert_eq!(
            registry.get("beta").unwrap().availability,
            ToolAvailability::Disabled
        );
    }

    #[test]
    fn test_lookup() {
        let registry = ToolRegistry::from_config(&test_config());
        assert!(registry.get("alpha").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_remove() {
        let mut registry = ToolRegistry::from_config(&test_config());
        assert!(registry.remove("alpha").is_some());
        assert!(registry.get("alpha").is_none());
        assert_eq!(registry.list().len(), 1);
    }
}
