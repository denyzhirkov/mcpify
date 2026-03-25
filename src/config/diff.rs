use crate::config::model::McpifyConfig;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug, Default)]
pub struct ConfigDiff {
    pub added_tools: Vec<String>,
    pub removed_tools: Vec<String>,
    pub changed_tools: Vec<String>,

    pub added_services: Vec<String>,
    pub removed_services: Vec<String>,
    pub changed_services: Vec<String>,
}

impl ConfigDiff {
    pub fn is_empty(&self) -> bool {
        self.added_tools.is_empty()
            && self.removed_tools.is_empty()
            && self.changed_tools.is_empty()
            && self.added_services.is_empty()
            && self.removed_services.is_empty()
            && self.changed_services.is_empty()
    }
}

impl std::fmt::Display for ConfigDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return write!(f, "no changes");
        }
        let mut parts = Vec::new();
        if !self.added_tools.is_empty() {
            parts.push(format!("+tools: {}", self.added_tools.join(", ")));
        }
        if !self.removed_tools.is_empty() {
            parts.push(format!("-tools: {}", self.removed_tools.join(", ")));
        }
        if !self.changed_tools.is_empty() {
            parts.push(format!("~tools: {}", self.changed_tools.join(", ")));
        }
        if !self.added_services.is_empty() {
            parts.push(format!("+services: {}", self.added_services.join(", ")));
        }
        if !self.removed_services.is_empty() {
            parts.push(format!("-services: {}", self.removed_services.join(", ")));
        }
        if !self.changed_services.is_empty() {
            parts.push(format!("~services: {}", self.changed_services.join(", ")));
        }
        write!(f, "{}", parts.join("; "))
    }
}

pub fn diff_configs(old: &McpifyConfig, new: &McpifyConfig) -> ConfigDiff {
    let mut diff = ConfigDiff::default();

    // Tools diff
    let old_tools: HashSet<&str> = old.tools.iter().map(|t| t.name.as_str()).collect();
    let new_tools: HashSet<&str> = new.tools.iter().map(|t| t.name.as_str()).collect();

    for name in new_tools.difference(&old_tools) {
        diff.added_tools.push(name.to_string());
    }
    for name in old_tools.difference(&new_tools) {
        diff.removed_tools.push(name.to_string());
    }
    for name in old_tools.intersection(&new_tools) {
        let old_tool = old.tools.iter().find(|t| t.name == *name).unwrap();
        let new_tool = new.tools.iter().find(|t| t.name == *name).unwrap();
        let old_val = serde_json::to_value(old_tool).unwrap_or(Value::Null);
        let new_val = serde_json::to_value(new_tool).unwrap_or(Value::Null);
        if old_val != new_val {
            diff.changed_tools.push(name.to_string());
        }
    }

    // Children diff
    let old_services: HashSet<&str> = old.services.iter().map(|c| c.name.as_str()).collect();
    let new_services: HashSet<&str> = new.services.iter().map(|c| c.name.as_str()).collect();

    for name in new_services.difference(&old_services) {
        diff.added_services.push(name.to_string());
    }
    for name in old_services.difference(&new_services) {
        diff.removed_services.push(name.to_string());
    }
    for name in old_services.intersection(&new_services) {
        let old_svc = old.services.iter().find(|c| c.name == *name).unwrap();
        let new_svc = new.services.iter().find(|c| c.name == *name).unwrap();
        let old_val = serde_json::to_value(old_svc).unwrap_or(Value::Null);
        let new_val = serde_json::to_value(new_svc).unwrap_or(Value::Null);
        if old_val != new_val {
            diff.changed_services.push(name.to_string());
        }
    }

    diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::McpifyConfig;

    fn parse(yaml: &str) -> McpifyConfig {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn test_no_changes() {
        let config = parse(
            r#"
tools:
  - name: a
    type: exec
    command: echo
"#,
        );
        let diff = diff_configs(&config, &config);
        assert!(diff.is_empty());
    }

    #[test]
    fn test_added_tool() {
        let old = parse(
            r#"
tools:
  - name: a
    type: exec
    command: echo
"#,
        );
        let new = parse(
            r#"
tools:
  - name: a
    type: exec
    command: echo
  - name: b
    type: exec
    command: ls
"#,
        );
        let diff = diff_configs(&old, &new);
        assert_eq!(diff.added_tools, vec!["b"]);
        assert!(diff.removed_tools.is_empty());
        assert!(diff.changed_tools.is_empty());
    }

    #[test]
    fn test_removed_tool() {
        let old = parse(
            r#"
tools:
  - name: a
    type: exec
    command: echo
  - name: b
    type: exec
    command: ls
"#,
        );
        let new = parse(
            r#"
tools:
  - name: a
    type: exec
    command: echo
"#,
        );
        let diff = diff_configs(&old, &new);
        assert!(diff.added_tools.is_empty());
        assert_eq!(diff.removed_tools, vec!["b"]);
    }

    #[test]
    fn test_changed_tool() {
        let old = parse(
            r#"
tools:
  - name: a
    type: exec
    command: echo
    timeout_ms: 5000
"#,
        );
        let new = parse(
            r#"
tools:
  - name: a
    type: exec
    command: echo
    timeout_ms: 10000
"#,
        );
        let diff = diff_configs(&old, &new);
        assert!(diff.added_tools.is_empty());
        assert!(diff.removed_tools.is_empty());
        assert_eq!(diff.changed_tools, vec!["a"]);
    }

    #[test]
    fn test_services_diff() {
        let old = parse(
            r#"
services:
  - name: svc1
    command: ./svc1
"#,
        );
        let new = parse(
            r#"
services:
  - name: svc2
    command: ./svc2
"#,
        );
        let diff = diff_configs(&old, &new);
        assert_eq!(diff.added_services, vec!["svc2"]);
        assert_eq!(diff.removed_services, vec!["svc1"]);
    }

    #[test]
    fn test_display() {
        let old = parse("tools: []");
        let new = parse(
            r#"
tools:
  - name: x
    type: exec
    command: echo
"#,
        );
        let diff = diff_configs(&old, &new);
        let s = format!("{diff}");
        assert!(s.contains("+tools: x"));
    }
}
