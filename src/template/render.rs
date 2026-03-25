use anyhow::{Result, bail};
use serde_json::Value;
use std::collections::HashMap;

/// Render `{{variable}}` placeholders in a template string.
/// Only simple substitution — no loops, conditions, or expressions.
pub fn render_template(template: &str, vars: &HashMap<String, Value>) -> Result<String> {
    let mut result = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '{' && chars.peek() == Some(&'{') {
            chars.next(); // consume second '{'
            let mut key = String::new();
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == '}' && chars.peek() == Some(&'}') {
                    chars.next(); // consume second '}'
                    closed = true;
                    break;
                }
                key.push(c);
            }
            if !closed {
                bail!("unclosed template placeholder: {{{{{key}");
            }
            let key = key.trim();
            if key.is_empty() {
                bail!("empty template placeholder");
            }
            match vars.get(key) {
                Some(Value::String(s)) => result.push_str(s),
                Some(Value::Number(n)) => result.push_str(&n.to_string()),
                Some(Value::Bool(b)) => result.push_str(&b.to_string()),
                Some(Value::Null) => result.push_str("null"),
                Some(other) => result.push_str(&other.to_string()),
                None => bail!("missing template variable: {key}"),
            }
        } else {
            result.push(ch);
        }
    }

    Ok(result)
}

/// Convert a flat JSON object to a HashMap<String, Value> for template rendering.
pub fn json_to_vars(input: &Value) -> HashMap<String, Value> {
    let mut vars = HashMap::new();
    if let Value::Object(map) = input {
        for (k, v) in map {
            vars.insert(k.clone(), v.clone());
        }
    }
    vars
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
    fn test_simple_substitution() {
        let result = render_template("hello {{name}}", &vars(&[("name", "world")])).unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_multiple_vars() {
        let result = render_template("{{a}} and {{b}}", &vars(&[("a", "X"), ("b", "Y")])).unwrap();
        assert_eq!(result, "X and Y");
    }

    #[test]
    fn test_no_placeholders() {
        let result = render_template("plain text", &HashMap::new()).unwrap();
        assert_eq!(result, "plain text");
    }

    #[test]
    fn test_missing_variable() {
        let result = render_template("{{missing}}", &HashMap::new());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing"));
    }

    #[test]
    fn test_trimmed_key() {
        let result = render_template("{{ name }}", &vars(&[("name", "trimmed")])).unwrap();
        assert_eq!(result, "trimmed");
    }

    #[test]
    fn test_number_value() {
        let mut v = HashMap::new();
        v.insert("port".to_string(), json!(3010));
        let result = render_template("port={{port}}", &v).unwrap();
        assert_eq!(result, "port=3010");
    }

    #[test]
    fn test_unclosed_placeholder() {
        let result = render_template("{{broken", &HashMap::new());
        assert!(result.is_err());
    }

    #[test]
    fn test_json_to_vars_conversion() {
        let input = json!({"id": "123", "name": "test"});
        let v = json_to_vars(&input);
        assert_eq!(v.get("id"), Some(&json!("123")));
        assert_eq!(v.get("name"), Some(&json!("test")));
    }
}
