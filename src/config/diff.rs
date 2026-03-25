use crate::config::model::McpifyConfig;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug, Default)]
pub struct ConfigDiff {
    pub added_tools: Vec<String>,
    pub removed_tools: Vec<String>,
    pub changed_tools: Vec<String>,

    pub added_children: Vec<String>,
    pub removed_children: Vec<String>,
    pub changed_children: Vec<String>,
}

impl ConfigDiff {
    pub fn is_empty(&self) -> bool {
        self.added_tools.is_empty()
            && self.removed_tools.is_empty()
            && self.changed_tools.is_empty()
            && self.added_children.is_empty()
            && self.removed_children.is_empty()
            && self.changed_children.is_empty()
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
        if !self.added_children.is_empty() {
            parts.push(format!("+children: {}", self.added_children.join(", ")));
        }
        if !self.removed_children.is_empty() {
            parts.push(format!("-children: {}", self.removed_children.join(", ")));
        }
        if !self.changed_children.is_empty() {
            parts.push(format!("~children: {}", self.changed_children.join(", ")));
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
    let old_children: HashSet<&str> = old.children.iter().map(|c| c.name.as_str()).collect();
    let new_children: HashSet<&str> = new.children.iter().map(|c| c.name.as_str()).collect();

    for name in new_children.difference(&old_children) {
        diff.added_children.push(name.to_string());
    }
    for name in old_children.difference(&new_children) {
        diff.removed_children.push(name.to_string());
    }
    for name in old_children.intersection(&new_children) {
        let old_child = old.children.iter().find(|c| c.name == *name).unwrap();
        let new_child = new.children.iter().find(|c| c.name == *name).unwrap();
        let old_val = serde_json::to_value(old_child).unwrap_or(Value::Null);
        let new_val = serde_json::to_value(new_child).unwrap_or(Value::Null);
        if old_val != new_val {
            diff.changed_children.push(name.to_string());
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
    fn test_children_diff() {
        let old = parse(
            r#"
children:
  - name: svc1
    command: ./svc1
"#,
        );
        let new = parse(
            r#"
children:
  - name: svc2
    command: ./svc2
"#,
        );
        let diff = diff_configs(&old, &new);
        assert_eq!(diff.added_children, vec!["svc2"]);
        assert_eq!(diff.removed_children, vec!["svc1"]);
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
