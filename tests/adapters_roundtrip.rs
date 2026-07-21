//! End-to-end round-trips for the http and sql adapters through `call_tool`.
//!
//! The delta over the adapters' colocated unit tests is the full path: the
//! request actually crosses a socket (http) or a real DB (sql), the response
//! comes back through the MCP `CallToolResult`, and `structured_content` is
//! populated — natively for sql, via `output.parse: json` for http.
//!
//! No external services: http talks to a hand-rolled local TCP echo stub (no
//! server dependency), sql to a `tempfile` sqlite database.

mod common;

use common::{Elicit, TestClient, build_state, call, connect};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// --- local HTTP echo stub ---------------------------------------------------

/// Bind a throwaway HTTP/1.1 server on a random loopback port that reflects each
/// request (method, target, auth headers, body) back as a JSON body. Returns the
/// base URL. The accept loop is aborted when the test's runtime is dropped.
async fn spawn_http_echo() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = handle_conn(&mut sock).await;
            });
        }
    });
    format!("http://{addr}")
}

async fn handle_conn(sock: &mut tokio::net::TcpStream) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
    };

    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut req = lines.next().unwrap_or("").split_whitespace();
    let method = req.next().unwrap_or("").to_string();
    let target = req.next().unwrap_or("").to_string();

    let mut content_length = 0usize;
    let mut authorization = String::new();
    let mut api_key = String::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let (key, val) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
            match key.as_str() {
                "content-length" => content_length = val.parse().unwrap_or(0),
                "authorization" => authorization = val,
                "x-api-key" => api_key = val,
                _ => {}
            }
        }
    }

    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }

    let payload = json!({
        "method": method,
        "target": target,
        "authorization": authorization,
        "x_api_key": api_key,
        "body": String::from_utf8_lossy(&body),
    })
    .to_string();

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    );
    sock.write_all(response.as_bytes()).await?;
    sock.flush().await
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The tool's response body is JSON (via `output.parse: json`), so it lands in
/// `structured_content` — read the echoed request back from there.
fn echoed(result: &rmcp::model::CallToolResult) -> Value {
    result
        .structured_content
        .clone()
        .expect("http tool with output.parse: json sets structured_content")
}

// --- http round-trips -------------------------------------------------------

#[tokio::test]
async fn http_get_sends_query_and_bearer_auth() {
    let base = spawn_http_echo().await;
    let yaml = format!(
        r#"
tools:
  - name: search
    type: http
    method: GET
    url: "{base}/items"
    query_params:
      q: "{{{{term}}}}"
    auth:
      type: bearer
      token: "s3cret-token"
    output:
      parse: json
"#
    );
    let state = build_state(&yaml);
    let (_server, client) = connect(&state, TestClient::new(Elicit::Accept)).await;

    let result = call(&client, "search", json!({ "term": "rust" })).await;
    assert_ne!(result.is_error, Some(true));

    let req = echoed(&result);
    assert_eq!(req["method"], "GET");
    assert!(
        req["target"].as_str().unwrap().contains("q=rust"),
        "query param not transmitted: {}",
        req["target"]
    );
    assert_eq!(req["authorization"], "Bearer s3cret-token");
}

#[tokio::test]
async fn http_post_sends_body_and_api_key() {
    let base = spawn_http_echo().await;
    let yaml = format!(
        r#"
tools:
  - name: create
    type: http
    method: POST
    url: "{base}/items"
    auth:
      type: api-key
      header: X-API-Key
      value: "key-123"
    body: '{{"name":"{{{{name}}}}"}}'
    output:
      parse: json
"#
    );
    let state = build_state(&yaml);
    let (_server, client) = connect(&state, TestClient::new(Elicit::Accept)).await;

    let result = call(&client, "create", json!({ "name": "widget" })).await;
    assert_ne!(result.is_error, Some(true));

    let req = echoed(&result);
    assert_eq!(req["method"], "POST");
    assert_eq!(req["x_api_key"], "key-123");
    assert_eq!(req["body"], r#"{"name":"widget"}"#);
}

// --- sql round-trips --------------------------------------------------------

fn sqlite_yaml(dsn: &str) -> String {
    format!(
        r#"
tools:
  - name: setup
    type: sql
    driver: sqlite
    dsn: "{dsn}"
    query: "CREATE TABLE users (id INTEGER, name TEXT)"
  - name: add
    type: sql
    driver: sqlite
    dsn: "{dsn}"
    query: "INSERT INTO users VALUES ({{{{id}}}}, {{{{name}}}})"
  - name: list
    type: sql
    driver: sqlite
    dsn: "{dsn}"
    query: "SELECT * FROM users ORDER BY id"
  - name: list_ro
    type: sql
    driver: sqlite
    dsn: "{dsn}"
    query: "SELECT * FROM users"
    annotations:
      read_only: true
  - name: add_ro
    type: sql
    driver: sqlite
    dsn: "{dsn}"
    query: "INSERT INTO users VALUES (99, 'mallory')"
    annotations:
      read_only: true
"#
    )
}

#[tokio::test]
async fn sql_select_returns_rows_and_structured_content() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let dsn = format!("sqlite:{}", tmp.path().display());
    let state = build_state(&sqlite_yaml(&dsn));
    let (_server, client) = connect(&state, TestClient::new(Elicit::Accept)).await;

    assert_ne!(call(&client, "setup", json!({})).await.is_error, Some(true));
    call(&client, "add", json!({ "id": 1, "name": "alice" })).await;
    call(&client, "add", json!({ "id": 2, "name": "bob" })).await;

    let result = call(&client, "list", json!({})).await;
    assert_ne!(result.is_error, Some(true));

    // sql fills structured_content natively (no output.parse needed).
    let rows = result
        .structured_content
        .clone()
        .expect("sql sets structured_content natively");
    let rows = rows.as_array().expect("rows array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], "alice");
    assert_eq!(rows[1]["name"], "bob");
}

#[tokio::test]
async fn sql_read_only_annotation_blocks_write() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let dsn = format!("sqlite:{}", tmp.path().display());
    let state = build_state(&sqlite_yaml(&dsn));
    let (_server, client) = connect(&state, TestClient::new(Elicit::Accept)).await;

    call(&client, "setup", json!({})).await;

    // A read-only SELECT works.
    assert_ne!(
        call(&client, "list_ro", json!({})).await.is_error,
        Some(true)
    );

    // A write through a read-only tool is rejected by SQLite itself.
    let denied = call(&client, "add_ro", json!({})).await;
    assert_eq!(denied.is_error, Some(true));
}
