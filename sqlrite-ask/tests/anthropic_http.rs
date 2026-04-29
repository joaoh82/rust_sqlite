//! End-to-end test of `AnthropicProvider` against a real localhost
//! HTTP server. Exercises the actual ureq + serde_json path so a
//! breaking refactor on either dep would surface here, not in
//! production.
//!
//! We run a `tiny_http` server on an OS-assigned port (port 0), point
//! `AskConfig::base_url` at it, and assert on the request body the
//! provider serializes. The provider uses an HTTPS URL by default;
//! `with_base_url` lets us override to plain HTTP for the mock.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use sqlrite::Connection;
use sqlrite_ask::{AskConfig, AskError, CacheTtl, ask};

struct Mock {
    server: Arc<tiny_http::Server>,
    addr: String,
    captured: Arc<Mutex<Option<CapturedRequest>>>,
    handle: Option<thread::JoinHandle<()>>,
}

struct CapturedRequest {
    body: serde_json::Value,
    headers: Vec<(String, String)>,
}

impl Mock {
    fn start(canned_status: u16, canned_body: &'static str) -> Self {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind localhost"));
        let addr = format!("http://{}", server.server_addr());
        let captured: Arc<Mutex<Option<CapturedRequest>>> = Arc::new(Mutex::new(None));

        let server_for_thread = server.clone();
        let captured_for_thread = captured.clone();

        let handle = thread::spawn(move || {
            // Handle exactly one request — every test makes a single
            // ask() call. After that the server drops with this
            // thread.
            if let Ok(mut req) = server_for_thread.recv() {
                let headers: Vec<(String, String)> = req
                    .headers()
                    .iter()
                    .map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string()))
                    .collect();
                let mut body = String::new();
                req.as_reader().read_to_string(&mut body).unwrap();
                let parsed: serde_json::Value =
                    serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                *captured_for_thread.lock().unwrap() = Some(CapturedRequest {
                    body: parsed,
                    headers,
                });
                let response = tiny_http::Response::from_string(canned_body)
                    .with_status_code(canned_status)
                    .with_header(
                        "Content-Type: application/json"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    );
                let _ = req.respond(response);
            }
        });

        Self {
            server,
            addr,
            captured,
            handle: Some(handle),
        }
    }

    fn captured(&self) -> Option<CapturedRequest> {
        self.captured.lock().unwrap().take()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        // Force the recv() in the worker thread to return so it
        // doesn't outlive the test.
        self.server.unblock();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

const SUCCESS_BODY: &str = r#"{
    "id": "msg_test",
    "type": "message",
    "role": "assistant",
    "model": "claude-sonnet-4-6",
    "content": [
        {"type": "text", "text": "{\"sql\": \"SELECT * FROM users\", \"explanation\": \"reads all users\"}"}
    ],
    "stop_reason": "end_turn",
    "usage": {"input_tokens": 1234, "output_tokens": 56, "cache_creation_input_tokens": 1000, "cache_read_input_tokens": 0}
}"#;

#[test]
fn end_to_end_against_localhost_mock() {
    let mock = Mock::start(200, SUCCESS_BODY);
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();

    let config = AskConfig {
        api_key: Some("test-key".to_string()),
        base_url: Some(mock.addr.clone()),
        ..AskConfig::default()
    };

    let resp = ask(&conn, "list all users", &config).expect("ask succeeds");
    assert_eq!(resp.sql, "SELECT * FROM users");
    assert_eq!(resp.explanation, "reads all users");
    assert_eq!(resp.usage.input_tokens, 1234);
    assert_eq!(resp.usage.cache_creation_input_tokens, 1000);
    assert_eq!(resp.usage.cache_read_input_tokens, 0);

    // Inspect what we sent.
    let captured = mock.captured().expect("server received request");
    assert_eq!(captured.body["model"], "claude-sonnet-4-6");
    assert_eq!(captured.body["max_tokens"], 1024);
    assert_eq!(captured.body["messages"][0]["role"], "user");
    assert_eq!(captured.body["messages"][0]["content"], "list all users");
    assert!(
        captured.body["system"][1]["text"]
            .as_str()
            .unwrap()
            .contains("CREATE TABLE users")
    );
    // Cache marker on the schema block.
    assert_eq!(
        captured.body["system"][1]["cache_control"]["type"],
        "ephemeral"
    );

    // Auth headers wired correctly.
    let mut saw_api_key = false;
    let mut saw_version = false;
    for (k, v) in &captured.headers {
        if k.eq_ignore_ascii_case("x-api-key") && v == "test-key" {
            saw_api_key = true;
        }
        if k.eq_ignore_ascii_case("anthropic-version") && v == "2023-06-01" {
            saw_version = true;
        }
    }
    assert!(
        saw_api_key,
        "missing x-api-key header; saw: {:?}",
        captured.headers
    );
    assert!(
        saw_version,
        "missing anthropic-version header; saw: {:?}",
        captured.headers
    );
}

#[test]
fn cache_ttl_one_hour_propagates_to_request() {
    let mock = Mock::start(200, SUCCESS_BODY);
    let conn = Connection::open_in_memory().unwrap();

    let config = AskConfig {
        api_key: Some("test-key".to_string()),
        base_url: Some(mock.addr.clone()),
        cache_ttl: CacheTtl::OneHour,
        ..AskConfig::default()
    };

    let _ = ask(&conn, "anything", &config).unwrap();
    let captured = mock.captured().unwrap();
    assert_eq!(captured.body["system"][1]["cache_control"]["ttl"], "1h");
}

#[test]
fn api_error_response_is_surfaced() {
    let mock = Mock::start(
        400,
        r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens too large"}}"#,
    );
    let conn = Connection::open_in_memory().unwrap();
    let config = AskConfig {
        api_key: Some("test-key".to_string()),
        base_url: Some(mock.addr.clone()),
        ..AskConfig::default()
    };

    let err = ask(&conn, "anything", &config).unwrap_err();
    match err {
        AskError::ApiStatus { status, detail } => {
            assert_eq!(status, 400);
            assert!(
                detail.contains("invalid_request_error") && detail.contains("max_tokens too large"),
                "got: {detail}"
            );
        }
        other => panic!("expected ApiStatus, got {other:?}"),
    }
}

#[test]
fn http_transport_error_is_surfaced() {
    // Point at a port nothing is listening on. ureq will return a
    // transport error, and we want that to land as `AskError::Http`,
    // not as a panic.
    //
    // Quick port scan to find one that's free, then immediately
    // (re)use it without binding — racy but fine for a unit test.
    let port = {
        let s = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let p = s.local_addr().unwrap().port();
        drop(s);
        p
    };
    // Tiny pause so the OS reaps the bound socket. Without this the
    // test occasionally flakes by getting a "connection refused" too
    // quickly to count, which is still the right error variant —
    // belt-and-braces.
    std::thread::sleep(Duration::from_millis(10));

    let conn = Connection::open_in_memory().unwrap();
    let config = AskConfig {
        api_key: Some("test-key".to_string()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        ..AskConfig::default()
    };
    let err = ask(&conn, "anything", &config).unwrap_err();
    assert!(
        matches!(err, AskError::Http(_)),
        "expected Http error, got {err:?}"
    );
}
