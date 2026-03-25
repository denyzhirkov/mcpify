use crate::adapters::ToolResult;
use crate::config::model::{SqlDriver, ToolConfig};
use crate::template::render::{merge_vars, render_template};
use anyhow::{Context, Result};
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
    let query = render_template(query_template, &vars)?;
    let timeout = Duration::from_millis(tool.timeout_ms);

    let result = tokio::time::timeout(timeout, run_query(driver, &dsn, &query))
        .await
        .map_err(|_| crate::errors::McpifyError::Timeout(tool.timeout_ms))?
        .with_context(|| format!("sql tool '{}': query failed", tool.name))?;

    Ok(result)
}

async fn run_query(driver: &SqlDriver, dsn: &str, query: &str) -> Result<ToolResult> {
    match driver {
        SqlDriver::Sqlite => run_sqlite(dsn, query).await,
        SqlDriver::Postgres => run_postgres(dsn, query).await,
    }
}

async fn run_sqlite(dsn: &str, query: &str) -> Result<ToolResult> {
    use sqlx::sqlite::SqlitePoolOptions;

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(dsn)
        .await
        .context("connecting to sqlite")?;

    let is_select = query.trim_start().to_uppercase().starts_with("SELECT");

    if is_select {
        let rows = sqlx::query(query)
            .fetch_all(&pool)
            .await
            .context("executing sqlite query")?;

        let json_rows: Vec<Value> = rows.iter().map(sqlite_row_to_json).collect();
        let stdout = serde_json::to_string_pretty(&json_rows)?;
        pool.close().await;
        Ok(ToolResult {
            stdout,
            stderr: String::new(),
            exit_code: Some(0),
            is_error: false,
        })
    } else {
        let result = sqlx::query(query)
            .execute(&pool)
            .await
            .context("executing sqlite statement")?;

        let affected = result.rows_affected();
        pool.close().await;
        Ok(ToolResult {
            stdout: json!({"rows_affected": affected}).to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            is_error: false,
        })
    }
}

async fn run_postgres(dsn: &str, query: &str) -> Result<ToolResult> {
    use sqlx::postgres::PgPoolOptions;

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(dsn)
        .await
        .context("connecting to postgres")?;

    let is_select = query.trim_start().to_uppercase().starts_with("SELECT");

    if is_select {
        let rows = sqlx::query(query)
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
        })
    } else {
        let result = sqlx::query(query)
            .execute(&pool)
            .await
            .context("executing postgres statement")?;

        let affected = result.rows_affected();
        pool.close().await;
        Ok(ToolResult {
            stdout: json!({"rows_affected": affected}).to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            is_error: false,
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

        let tool_insert = make_sql_tool(
            SqlDriver::Sqlite,
            &dsn,
            "INSERT INTO t VALUES (1, '{{name}}')",
        );
        execute(&tool_insert, json!({"name": "bob"}), &cv)
            .await
            .unwrap();

        let tool_select = make_sql_tool(
            SqlDriver::Sqlite,
            &dsn,
            "SELECT * FROM t WHERE name = '{{name}}'",
        );
        let result = execute(&tool_select, json!({"name": "bob"}), &cv)
            .await
            .unwrap();
        let rows: Vec<Value> = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "bob");
    }
}
