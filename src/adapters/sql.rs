use crate::adapters::ToolResult;
use crate::config::model::{SqlDriver, ToolConfig};
use crate::template::render::{merge_vars, render_template};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use sqlx::Column;
use sqlx::Row;
use sqlx::TypeInfo;
use std::collections::HashMap;
use std::time::Duration;

pub async fn execute(
    tool: &ToolConfig,
    input: Value,
    config_vars: &HashMap<String, String>,
) -> Result<ToolResult> {
    let driver = tool.driver.as_ref().context("sql tool missing 'driver'")?;
    let dsn_template = tool.dsn.as_ref().context("sql tool missing 'dsn'")?;
    let query_template = tool.query.as_ref().context("sql tool missing 'query'")?;

    let vars = merge_vars(&input, config_vars);
    let dsn = render_template(dsn_template, &vars)?;
    let timeout = Duration::from_millis(tool.timeout_ms);
    let read_only = tool
        .annotations
        .as_ref()
        .and_then(|a| a.read_only)
        .unwrap_or(false);

    let result = tokio::time::timeout(
        timeout,
        run_query(driver, &dsn, query_template, &vars, read_only),
    )
    .await
    .map_err(|_| crate::errors::McpifyError::Timeout(tool.timeout_ms))?
    .with_context(|| format!("sql tool '{}': query failed", tool.name))?;

    Ok(result)
}

async fn run_query(
    driver: &SqlDriver,
    dsn: &str,
    query_template: &str,
    vars: &HashMap<String, Value>,
    read_only: bool,
) -> Result<ToolResult> {
    match driver {
        SqlDriver::Sqlite => run_sqlite(dsn, query_template, vars, read_only).await,
        SqlDriver::Postgres => run_postgres(dsn, query_template, vars, read_only).await,
    }
}

/// Turn a `query` template into a parameterized statement + ordered bind values.
/// `{{var}}` becomes a driver placeholder (`$n` / `?`) bound as a value; a
/// non-scalar value is rejected. `{{raw:var}}` is interpolated as text but only
/// after the value passes a strict SQL-identifier allowlist — so neither role
/// permits injection.
fn build_bound_query(
    query: &str,
    vars: &HashMap<String, Value>,
    driver: &SqlDriver,
) -> Result<(String, Vec<Value>)> {
    let mut out = String::with_capacity(query.len());
    let mut values: Vec<Value> = Vec::new();
    let mut chars = query.chars().peekable();

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

            if let Some(raw_key) = key.strip_prefix("raw:") {
                let raw_key = raw_key.trim();
                let value = vars
                    .get(raw_key)
                    .ok_or_else(|| anyhow!("missing template variable: {raw_key}"))?;
                let ident = match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                if !is_valid_identifier(&ident) {
                    bail!(
                        "raw substitution {{{{raw:{raw_key}}}}} = '{ident}' is not a valid SQL identifier"
                    );
                }
                out.push_str(&ident);
            } else {
                let value = vars
                    .get(key)
                    .ok_or_else(|| anyhow!("missing template variable: {key}"))?;
                if value.is_array() || value.is_object() {
                    bail!("cannot bind non-scalar value for {{{{{key}}}}}");
                }
                values.push(value.clone());
                match driver {
                    SqlDriver::Postgres => out.push_str(&format!("${}", values.len())),
                    SqlDriver::Sqlite => out.push('?'),
                }
            }
        } else {
            out.push(ch);
        }
    }

    Ok((out, values))
}

fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

async fn run_sqlite(
    dsn: &str,
    query_template: &str,
    vars: &HashMap<String, Value>,
    read_only: bool,
) -> Result<ToolResult> {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    let (sql, values) = build_bound_query(query_template, vars, &SqlDriver::Sqlite)?;

    // read_only annotation → open the DB read-only so writes are rejected by SQLite itself.
    let options = dsn
        .parse::<SqliteConnectOptions>()
        .context("parsing sqlite dsn")?
        .read_only(read_only);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .context("connecting to sqlite")?;

    let is_select = sql.trim_start().to_uppercase().starts_with("SELECT");

    let mut q = sqlx::query(&sql);
    for v in values {
        q = match v {
            Value::String(s) => q.bind(s),
            Value::Bool(b) => q.bind(b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    q.bind(i)
                } else if let Some(u) = n.as_u64() {
                    q.bind(u as i64)
                } else {
                    q.bind(n.as_f64().unwrap_or_default())
                }
            }
            Value::Null => q.bind(Option::<String>::None),
            other => return Err(anyhow!("cannot bind non-scalar value: {other}")),
        };
    }

    if is_select {
        let rows = q.fetch_all(&pool).await.context("executing sqlite query")?;

        let json_rows: Vec<Value> = rows.iter().map(sqlite_row_to_json).collect();
        let stdout = serde_json::to_string_pretty(&json_rows)?;
        pool.close().await;
        Ok(ToolResult {
            stdout,
            stderr: String::new(),
            exit_code: Some(0),
            is_error: false,
            structured: Some(Value::Array(json_rows)),
        })
    } else {
        let result = q
            .execute(&pool)
            .await
            .context("executing sqlite statement")?;

        let affected = result.rows_affected();
        let structured = json!({ "rows_affected": affected });
        pool.close().await;
        Ok(ToolResult {
            stdout: structured.to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            is_error: false,
            structured: Some(structured),
        })
    }
}

async fn run_postgres(
    dsn: &str,
    query_template: &str,
    vars: &HashMap<String, Value>,
    read_only: bool,
) -> Result<ToolResult> {
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    let (sql, values) = build_bound_query(query_template, vars, &SqlDriver::Postgres)?;

    // read_only annotation → force a read-only session so writes are rejected by Postgres.
    let mut options = dsn
        .parse::<PgConnectOptions>()
        .context("parsing postgres dsn")?;
    if read_only {
        options = options.options([("default_transaction_read_only", "on")]);
    }
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .context("connecting to postgres")?;

    let is_select = sql.trim_start().to_uppercase().starts_with("SELECT");

    let mut q = sqlx::query(&sql);
    for v in values {
        q = match v {
            Value::String(s) => q.bind(s),
            Value::Bool(b) => q.bind(b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    q.bind(i)
                } else if let Some(u) = n.as_u64() {
                    q.bind(u as i64)
                } else {
                    q.bind(n.as_f64().unwrap_or_default())
                }
            }
            Value::Null => q.bind(Option::<String>::None),
            other => return Err(anyhow!("cannot bind non-scalar value: {other}")),
        };
    }

    if is_select {
        let rows = q
            .fetch_all(&pool)
            .await
            .context("executing postgres query")?;

        let json_rows: Vec<Value> = rows.iter().map(pg_row_to_json).collect();
        let stdout = serde_json::to_string_pretty(&json_rows)?;
        pool.close().await;
        Ok(ToolResult {
            stdout,
            stderr: String::new(),
            exit_code: Some(0),
            is_error: false,
            structured: Some(Value::Array(json_rows)),
        })
    } else {
        let result = q
            .execute(&pool)
            .await
            .context("executing postgres statement")?;

        let affected = result.rows_affected();
        let structured = json!({ "rows_affected": affected });
        pool.close().await;
        Ok(ToolResult {
            stdout: structured.to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            is_error: false,
            structured: Some(structured),
        })
    }
}

fn sqlite_row_to_json(row: &sqlx::sqlite::SqliteRow) -> Value {
    let mut map = serde_json::Map::new();
    for col in row.columns() {
        let name = col.name();
        let value: Value = match col.type_info().name() {
            "INTEGER" => row
                .try_get::<i64, _>(name)
                .map(|v| json!(v))
                .unwrap_or(Value::Null),
            "REAL" => row
                .try_get::<f64, _>(name)
                .map(|v| json!(v))
                .unwrap_or(Value::Null),
            "BOOLEAN" => row
                .try_get::<bool, _>(name)
                .map(|v| json!(v))
                .unwrap_or(Value::Null),
            "NULL" => Value::Null,
            _ => row
                .try_get::<String, _>(name)
                .map(|v| json!(v))
                .unwrap_or(Value::Null),
        };
        map.insert(name.to_string(), value);
    }
    Value::Object(map)
}

fn pg_row_to_json(row: &sqlx::postgres::PgRow) -> Value {
    let mut map = serde_json::Map::new();
    for col in row.columns() {
        let name = col.name();
        let value: Value = match col.type_info().name() {
            "INT2" | "INT4" | "INT8" => row
                .try_get::<i64, _>(name)
                .map(|v| json!(v))
                .unwrap_or(Value::Null),
            "FLOAT4" | "FLOAT8" => row
                .try_get::<f64, _>(name)
                .map(|v| json!(v))
                .unwrap_or(Value::Null),
            "BOOL" => row
                .try_get::<bool, _>(name)
                .map(|v| json!(v))
                .unwrap_or(Value::Null),
            _ => row
                .try_get::<String, _>(name)
                .map(|v| json!(v))
                .unwrap_or(Value::Null),
        };
        map.insert(name.to_string(), value);
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{SqlDriver, ToolConfig, ToolType};

    fn make_sql_tool(driver: SqlDriver, dsn: &str, query: &str) -> ToolConfig {
        ToolConfig {
            name: "test_sql".to_string(),
            tool_type: ToolType::Sql,
            description: String::new(),
            command: None,
            args: vec![],
            cwd: None,
            env: HashMap::new(),
            method: None,
            url: None,
            headers: HashMap::new(),
            body: None,
            driver: Some(driver),
            dsn: Some(dsn.to_string()),
            query: Some(query.to_string()),
            timeout_ms: 5000,
            depends_on: vec![],
            enabled: true,
            input: None,
            retry: None,
            annotations: None,
            output: None,
        }
    }

    #[tokio::test]
    async fn test_sql_sqlite_select() {
        let cv = HashMap::new();

        // Create table first
        let tool_create = make_sql_tool(
            SqlDriver::Sqlite,
            "sqlite::memory:",
            "CREATE TABLE t (id INTEGER, name TEXT)",
        );
        let r = execute(&tool_create, json!({}), &cv).await.unwrap();
        assert!(!r.is_error);

        // Use a file-based temp db so the table persists across connections
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let dsn = format!("sqlite:{}", tmp.path().display());

        let tool_create = make_sql_tool(
            SqlDriver::Sqlite,
            &dsn,
            "CREATE TABLE t (id INTEGER, name TEXT)",
        );
        execute(&tool_create, json!({}), &cv).await.unwrap();

        let tool_insert =
            make_sql_tool(SqlDriver::Sqlite, &dsn, "INSERT INTO t VALUES (1, 'alice')");
        execute(&tool_insert, json!({}), &cv).await.unwrap();

        let tool_select = make_sql_tool(SqlDriver::Sqlite, &dsn, "SELECT * FROM t");
        let result = execute(&tool_select, json!({}), &cv).await.unwrap();
        assert!(!result.is_error);
        let rows: Vec<Value> = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "alice");

        // sql produces structuredContent natively, without any output.parse.
        let structured = result.structured.as_ref().unwrap();
        assert!(structured.is_array());
        assert_eq!(structured[0]["name"], "alice");
    }

    #[tokio::test]
    async fn test_sql_with_template_vars() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let dsn = format!("sqlite:{}", tmp.path().display());
        let cv = HashMap::new();

        let tool_create = make_sql_tool(
            SqlDriver::Sqlite,
            &dsn,
            "CREATE TABLE t (id INTEGER, name TEXT)",
        );
        execute(&tool_create, json!({}), &cv).await.unwrap();

        // Bound placeholders are NOT quoted — values are bound, not interpolated.
        let tool_insert = make_sql_tool(
            SqlDriver::Sqlite,
            &dsn,
            "INSERT INTO t VALUES (1, {{name}})",
        );
        execute(&tool_insert, json!({"name": "bob"}), &cv)
            .await
            .unwrap();

        let tool_select = make_sql_tool(
            SqlDriver::Sqlite,
            &dsn,
            "SELECT * FROM t WHERE name = {{name}}",
        );
        let result = execute(&tool_select, json!({"name": "bob"}), &cv)
            .await
            .unwrap();
        let rows: Vec<Value> = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "bob");
    }

    #[tokio::test]
    async fn test_sql_injection_is_bound_not_executed() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let dsn = format!("sqlite:{}", tmp.path().display());
        let cv = HashMap::new();

        let create = make_sql_tool(
            SqlDriver::Sqlite,
            &dsn,
            "CREATE TABLE t (id INTEGER, name TEXT)",
        );
        execute(&create, json!({}), &cv).await.unwrap();

        // Classic injection payload passed as a value.
        let payload = "'; DROP TABLE t; --";
        let insert = make_sql_tool(
            SqlDriver::Sqlite,
            &dsn,
            "INSERT INTO t VALUES (1, {{name}})",
        );
        execute(&insert, json!({ "name": payload }), &cv)
            .await
            .unwrap();

        // Table survives, and the payload was stored verbatim as data.
        let select = make_sql_tool(SqlDriver::Sqlite, &dsn, "SELECT name FROM t");
        let result = execute(&select, json!({}), &cv).await.unwrap();
        let rows: Vec<Value> = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], payload);
    }

    #[tokio::test]
    async fn test_sql_raw_identifier_substitution() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let dsn = format!("sqlite:{}", tmp.path().display());
        let cv = HashMap::new();

        let create = make_sql_tool(SqlDriver::Sqlite, &dsn, "CREATE TABLE users (id INTEGER)");
        execute(&create, json!({}), &cv).await.unwrap();

        let select = make_sql_tool(SqlDriver::Sqlite, &dsn, "SELECT * FROM {{raw:table}}");
        let result = execute(&select, json!({ "table": "users" }), &cv)
            .await
            .unwrap();
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn test_sql_read_only_blocks_write() {
        use crate::config::model::ToolAnnotations;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let dsn = format!("sqlite:{}", tmp.path().display());
        let cv = HashMap::new();

        // Set up the table with a normal (read-write) tool.
        let create = make_sql_tool(SqlDriver::Sqlite, &dsn, "CREATE TABLE t (id INTEGER)");
        execute(&create, json!({}), &cv).await.unwrap();

        let read_only = || {
            Some(ToolAnnotations {
                read_only: Some(true),
                ..Default::default()
            })
        };

        // read_only tool: SELECT works.
        let mut ro_select = make_sql_tool(SqlDriver::Sqlite, &dsn, "SELECT * FROM t");
        ro_select.annotations = read_only();
        assert!(execute(&ro_select, json!({}), &cv).await.is_ok());

        // read_only tool: write rejected by SQLite itself.
        let mut ro_insert = make_sql_tool(SqlDriver::Sqlite, &dsn, "INSERT INTO t VALUES (1)");
        ro_insert.annotations = read_only();
        assert!(execute(&ro_insert, json!({}), &cv).await.is_err());

        // Without the annotation, writes still work.
        let rw_insert = make_sql_tool(SqlDriver::Sqlite, &dsn, "INSERT INTO t VALUES (2)");
        assert!(execute(&rw_insert, json!({}), &cv).await.is_ok());
    }

    #[test]
    fn test_build_bound_query_placeholders_and_values() {
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), json!("bob"));
        vars.insert("age".to_string(), json!(42));

        let (sql, values) = build_bound_query(
            "SELECT * FROM t WHERE name = {{name}} AND age > {{age}}",
            &vars,
            &SqlDriver::Postgres,
        )
        .unwrap();
        assert_eq!(sql, "SELECT * FROM t WHERE name = $1 AND age > $2");
        assert_eq!(values, vec![json!("bob"), json!(42)]);

        let (sql, _) = build_bound_query("SELECT {{name}}", &vars, &SqlDriver::Sqlite).unwrap();
        assert_eq!(sql, "SELECT ?");
    }

    #[test]
    fn test_build_bound_query_raw_rejects_non_identifier() {
        let mut vars = HashMap::new();
        vars.insert("table".to_string(), json!("users; DROP TABLE users"));
        let err = build_bound_query("SELECT * FROM {{raw:table}}", &vars, &SqlDriver::Sqlite)
            .unwrap_err();
        assert!(err.to_string().contains("not a valid SQL identifier"));
    }

    #[test]
    fn test_build_bound_query_rejects_non_scalar() {
        let mut vars = HashMap::new();
        vars.insert("ids".to_string(), json!([1, 2, 3]));
        let err = build_bound_query(
            "SELECT * FROM t WHERE id IN {{ids}}",
            &vars,
            &SqlDriver::Sqlite,
        )
        .unwrap_err();
        assert!(err.to_string().contains("non-scalar"));
    }
}
