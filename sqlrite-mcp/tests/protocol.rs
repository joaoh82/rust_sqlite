//! End-to-end protocol tests.
//!
//! Spawn the binary as a subprocess, write JSON-RPC requests to its
//! stdin, parse responses from stdout. Covers the full lifecycle +
//! every tool's success path + the major error shapes.
//!
//! No async runtime, no test framework beyond `#[test]` and
//! `assert_eq!` — keeps consistent with the rest of the workspace.
//!
//! Run with:
//!     cargo test -p sqlrite-mcp
//!
//! These tests rely on the workspace's debug build of the binary,
//! which `cargo test` produces automatically as a build dep.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use serde_json::{Value, json};

/// Path to the `sqlrite-mcp` binary built by the test harness.
/// Cargo sets `CARGO_BIN_EXE_<name>` for any binary in the same
/// crate as the test — exactly the path we want.
fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sqlrite-mcp"))
}

/// One short-lived MCP server subprocess. Drop = kill.
struct Server {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Server {
    fn spawn(args: &[&str]) -> Self {
        let mut cmd = Command::new(binary_path());
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Discard stderr so engine prettytable noise doesn't fill
            // the test runner's terminal. Real failures surface as
            // missing/unexpected JSON on stdout instead.
            .stderr(Stdio::null());
        let mut child = cmd.spawn().expect("spawn sqlrite-mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    /// Send a JSON-RPC request, read one response line, parse + return.
    fn request(&mut self, body: Value) -> Value {
        writeln!(self.stdin, "{body}").expect("write request");
        self.stdin.flush().expect("flush");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read response");
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("parse response `{}`: {}", line.trim_end(), e))
    }

    /// Send a notification (no id) — no response expected.
    fn notify(&mut self, body: Value) {
        writeln!(self.stdin, "{body}").expect("write notification");
        self.stdin.flush().expect("flush");
    }

    fn handshake(&mut self) {
        let init = self.request(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
            }
        }));
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(init["result"]["serverInfo"]["name"], "sqlrite-mcp");
        self.notify(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }));
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ----------------------------------------------------------------------
// Lifecycle
// ----------------------------------------------------------------------

#[test]
fn initialize_returns_server_info() {
    let mut srv = Server::spawn(&["--in-memory"]);
    let resp = srv.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    }));
    assert_eq!(resp["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(resp["result"]["serverInfo"]["name"], "sqlrite-mcp");
    assert_eq!(
        resp["result"]["capabilities"]["tools"]["listChanged"],
        false
    );
}

#[test]
fn unknown_method_returns_method_not_found() {
    let mut srv = Server::spawn(&["--in-memory"]);
    srv.handshake();
    let resp = srv.request(json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "no/such/method",
        "params": {}
    }));
    assert!(resp["error"].is_object(), "expected error, got {resp}");
    assert_eq!(resp["error"]["code"], -32601);
}

#[test]
fn malformed_json_returns_parse_error() {
    let mut srv = Server::spawn(&["--in-memory"]);
    writeln!(srv.stdin, "{{not valid json").expect("write garbage");
    srv.stdin.flush().expect("flush");
    let mut line = String::new();
    srv.stdout.read_line(&mut line).expect("read response");
    let resp: Value = serde_json::from_str(&line).expect("parse error response");
    assert_eq!(resp["error"]["code"], -32700);
    assert!(resp["id"].is_null());
}

#[test]
fn ping_responds_with_empty_object() {
    let mut srv = Server::spawn(&["--in-memory"]);
    srv.handshake();
    let resp = srv.request(json!({"jsonrpc":"2.0","id":3,"method":"ping","params":{}}));
    assert_eq!(resp["result"], json!({}));
}

// ----------------------------------------------------------------------
// tools/list
// ----------------------------------------------------------------------

#[test]
fn tools_list_returns_expected_set_in_default_mode() {
    let mut srv = Server::spawn(&["--in-memory"]);
    srv.handshake();
    let resp = srv.request(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}));
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    let mut expected = vec![
        "list_tables",
        "describe_table",
        "query",
        "execute",
        "schema_dump",
        "vector_search",
    ];
    if cfg!(feature = "ask") {
        expected.push("ask");
    }
    assert_eq!(names, expected, "tool list mismatch");
}

#[test]
fn tools_list_omits_execute_under_read_only() {
    // In-memory + read-only: open_in_memory ignores --read-only at the
    // engine layer (no on-disk lock to take), but the CLI still sets
    // the flag, and the protocol layer hides `execute` based on it.
    // To cleanly test read-only behavior we need an on-disk DB.
    let tmp = tempfile_path();
    {
        // Pre-create the DB file with a table so open_read_only succeeds.
        let mut srv = Server::spawn(&[tmp.to_str().unwrap()]);
        srv.handshake();
        let _ = srv.request(json!({
            "jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"execute","arguments":{
                "sql":"CREATE TABLE t (id INTEGER PRIMARY KEY)"
            }}
        }));
    }
    let mut srv = Server::spawn(&[tmp.to_str().unwrap(), "--read-only"]);
    srv.handshake();
    let resp = srv.request(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}));
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(
        !names.contains(&"execute"),
        "execute should be hidden in --read-only"
    );
    assert!(names.contains(&"query"), "query should still be available");
    let _ = std::fs::remove_file(&tmp);
}

// ----------------------------------------------------------------------
// tools/call — happy paths
// ----------------------------------------------------------------------

#[test]
fn round_trip_create_insert_query() {
    let mut srv = Server::spawn(&["--in-memory"]);
    srv.handshake();
    // CREATE
    let r = call_tool(
        &mut srv,
        10,
        "execute",
        json!({
            "sql": "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)"
        }),
    );
    assert_tool_success(&r);
    // INSERT
    let r = call_tool(
        &mut srv,
        11,
        "execute",
        json!({
            "sql": "INSERT INTO users (name) VALUES ('alice'), ('bob')"
        }),
    );
    assert_tool_success(&r);
    // list_tables
    let r = call_tool(&mut srv, 12, "list_tables", json!({}));
    let text = tool_text(&r);
    let names: Vec<String> = serde_json::from_str(&text).unwrap();
    assert_eq!(names, vec!["users"]);
    // SELECT through the query tool
    let r = call_tool(
        &mut srv,
        13,
        "query",
        json!({
            "sql": "SELECT id, name FROM users ORDER BY id"
        }),
    );
    let text = tool_text(&r);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["columns"], json!(["id", "name"]));
    assert_eq!(parsed["row_count"], 2);
    assert_eq!(parsed["rows"][0]["id"], 1);
    assert_eq!(parsed["rows"][1]["id"], 2);
}

#[test]
fn describe_table_returns_columns_and_row_count() {
    let mut srv = Server::spawn(&["--in-memory"]);
    srv.handshake();
    let _ = call_tool(
        &mut srv,
        1,
        "execute",
        json!({
            "sql": "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT NOT NULL)"
        }),
    );
    let _ = call_tool(
        &mut srv,
        2,
        "execute",
        json!({
            "sql": "INSERT INTO items (label) VALUES ('a'), ('b'), ('c')"
        }),
    );
    let r = call_tool(&mut srv, 3, "describe_table", json!({"name": "items"}));
    let parsed: Value = serde_json::from_str(&tool_text(&r)).unwrap();
    assert_eq!(parsed["name"], "items");
    assert_eq!(parsed["row_count"], 3);
    let cols = parsed["columns"].as_array().unwrap();
    assert_eq!(cols.len(), 2);
    assert_eq!(cols[0]["name"], "id");
    assert_eq!(cols[0]["primary_key"], true);
    assert_eq!(cols[1]["name"], "label");
    assert_eq!(cols[1]["not_null"], true);
}

#[test]
fn schema_dump_is_create_table_text() {
    let mut srv = Server::spawn(&["--in-memory"]);
    srv.handshake();
    let _ = call_tool(
        &mut srv,
        1,
        "execute",
        json!({
            "sql": "CREATE TABLE x (id INTEGER PRIMARY KEY)"
        }),
    );
    let r = call_tool(&mut srv, 2, "schema_dump", json!({}));
    let text = tool_text(&r);
    assert!(
        text.contains("CREATE TABLE"),
        "schema_dump output should contain CREATE TABLE: {text}"
    );
    assert!(
        text.contains("x"),
        "schema_dump output should mention table x: {text}"
    );
}

// ----------------------------------------------------------------------
// tools/call — error paths
// ----------------------------------------------------------------------

#[test]
fn query_rejects_non_select() {
    let mut srv = Server::spawn(&["--in-memory"]);
    srv.handshake();
    let r = call_tool(
        &mut srv,
        1,
        "query",
        json!({"sql": "INSERT INTO foo VALUES (1)"}),
    );
    assert_eq!(r["result"]["isError"], true);
    let text = tool_text(&r);
    assert!(
        text.contains("SELECT"),
        "expected guidance about SELECT, got: {text}"
    );
}

#[test]
fn execute_rejects_select() {
    let mut srv = Server::spawn(&["--in-memory"]);
    srv.handshake();
    let r = call_tool(&mut srv, 1, "execute", json!({"sql": "SELECT 1"}));
    assert_eq!(r["result"]["isError"], true);
    let text = tool_text(&r);
    assert!(
        text.contains("query"),
        "expected guidance about query tool, got: {text}"
    );
}

#[test]
fn describe_table_rejects_unsafe_identifier() {
    let mut srv = Server::spawn(&["--in-memory"]);
    srv.handshake();
    let r = call_tool(
        &mut srv,
        1,
        "describe_table",
        json!({"name": "users; DROP TABLE x"}),
    );
    assert_eq!(r["result"]["isError"], true);
}

#[test]
fn execute_rejected_under_read_only_with_clear_message() {
    let tmp = tempfile_path();
    {
        let mut srv = Server::spawn(&[tmp.to_str().unwrap()]);
        srv.handshake();
        let _ = call_tool(
            &mut srv,
            1,
            "execute",
            json!({
                "sql": "CREATE TABLE t (id INTEGER PRIMARY KEY)"
            }),
        );
    }
    let mut srv = Server::spawn(&[tmp.to_str().unwrap(), "--read-only"]);
    srv.handshake();
    let r = call_tool(
        &mut srv,
        2,
        "execute",
        json!({"sql": "INSERT INTO t VALUES (1)"}),
    );
    assert_eq!(r["result"]["isError"], true);
    let text = tool_text(&r);
    assert!(
        text.contains("read-only"),
        "expected read-only error, got: {text}"
    );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn unknown_tool_returns_tool_error() {
    let mut srv = Server::spawn(&["--in-memory"]);
    srv.handshake();
    let r = call_tool(&mut srv, 1, "no_such_tool", json!({}));
    assert_eq!(r["result"]["isError"], true);
}

#[test]
fn vector_search_dimension_mismatch_returns_clear_error() {
    let mut srv = Server::spawn(&["--in-memory"]);
    srv.handshake();
    let _ = call_tool(
        &mut srv,
        1,
        "execute",
        json!({
            "sql": "CREATE TABLE docs (id INTEGER PRIMARY KEY, embedding VECTOR(3))"
        }),
    );
    let r = call_tool(
        &mut srv,
        2,
        "vector_search",
        json!({
            "table": "docs",
            "column": "embedding",
            "embedding": [0.1, 0.2],  // 2D, column is 3D
        }),
    );
    assert_eq!(r["result"]["isError"], true);
    let text = tool_text(&r);
    assert!(
        text.contains("dimension"),
        "expected dimension-mismatch error: {text}"
    );
}

#[test]
fn vector_search_returns_nearest_rows() {
    let mut srv = Server::spawn(&["--in-memory"]);
    srv.handshake();
    let _ = call_tool(
        &mut srv,
        1,
        "execute",
        json!({
            "sql": "CREATE TABLE docs (id INTEGER PRIMARY KEY, embedding VECTOR(2))"
        }),
    );
    let _ = call_tool(
        &mut srv,
        2,
        "execute",
        json!({
            "sql": "INSERT INTO docs (embedding) VALUES ([1.0, 0.0])"
        }),
    );
    let _ = call_tool(
        &mut srv,
        3,
        "execute",
        json!({
            "sql": "INSERT INTO docs (embedding) VALUES ([0.0, 1.0])"
        }),
    );
    let _ = call_tool(
        &mut srv,
        4,
        "execute",
        json!({
            "sql": "INSERT INTO docs (embedding) VALUES ([0.9, 0.1])"
        }),
    );
    let r = call_tool(
        &mut srv,
        5,
        "vector_search",
        json!({
            "table": "docs",
            "column": "embedding",
            "embedding": [1.0, 0.0],
            "k": 2,
        }),
    );
    assert_tool_success(&r);
    let parsed: Value = serde_json::from_str(&tool_text(&r)).unwrap();
    let rows = parsed["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    // Closest should be id=1 ([1.0, 0.0] — distance 0).
    assert_eq!(rows[0]["id"], 1);
}

// ----------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------

fn call_tool(srv: &mut Server, id: u64, name: &str, arguments: Value) -> Value {
    srv.request(json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    }))
}

fn assert_tool_success(resp: &Value) {
    assert!(
        resp["result"].is_object(),
        "expected result object, got: {resp}"
    );
    assert_eq!(
        resp["result"]["isError"], false,
        "expected success, got: {resp}"
    );
}

fn tool_text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("missing tool text in: {resp}"))
        .to_string()
}

fn tempfile_path() -> PathBuf {
    let mut p = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!(
        "sqlrite-mcp-test-{}-{nanos}.sqlrite",
        std::process::id()
    ));
    p
}
