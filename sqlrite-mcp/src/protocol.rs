//! JSON-RPC 2.0 dispatcher + MCP lifecycle.
//!
//! Owns:
//!
//! - The [`ServerState`] type — wraps the open `Connection`, the
//!   read-only flag, and an `initialized` boolean (the client must
//!   send `initialize` before anything else; we track that here).
//!
//! - The [`handle`] entrypoint that takes one inbound JSON-RPC
//!   message string and returns the response JSON value (or `None`
//!   if the message was a notification, which JSON-RPC says gets no
//!   reply).
//!
//! - The four MCP methods we implement: `initialize`,
//!   `notifications/initialized`, `tools/list`, `tools/call`. Plus
//!   a no-op `shutdown` and `notifications/cancelled` for politeness.
//!
//! Tool registry lives in [`tools::dispatch`] — this file calls
//! through that function and packages whatever it returns into a
//! tool-result JSON shape.
//!
//! ## MCP version
//!
//! We declare protocol version `2025-11-25` (current as of Phase
//! 7h's design). If the client requests an older version, we echo
//! ours back and let the client decide whether to disconnect — that
//! matches the spec's recommendation.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use sqlrite::Connection;

use crate::error::ProtocolError;
use crate::tools;

/// Protocol version we declare in `initialize`. Pin to a single
/// constant so future bumps are a one-line change.
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// Per-process server state.
pub struct ServerState {
    pub conn: Connection,
    pub read_only: bool,
    /// Set to `true` after a successful `initialize` call. Required
    /// by the spec — methods other than `initialize` should be
    /// rejected before initialization completes.
    pub initialized: bool,
}

impl ServerState {
    pub fn new(conn: Connection, read_only: bool) -> Self {
        Self {
            conn,
            read_only,
            initialized: false,
        }
    }
}

/// Inbound JSON-RPC envelope. Loose typing on `params` (Option<Value>)
/// because each method has its own params shape and we'd rather
/// downcast in the handler than chase a giant tagged enum here.
#[derive(Debug, Deserialize)]
struct Request {
    #[allow(dead_code)]
    jsonrpc: String,
    /// Notifications omit `id`. Requests carry one.
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

/// Outbound success envelope. We don't use a struct for errors —
/// they're rarer and we hand-build the JSON for clarity at the
/// callsite.
#[derive(Debug, Serialize)]
struct Response<'a> {
    jsonrpc: &'static str,
    id: &'a Value,
    result: Value,
}

/// Top-level dispatcher. Takes one inbound JSON-RPC message string
/// and returns the response value (or `None` for notifications).
///
/// Errors from this layer are JSON-RPC errors — the response body
/// itself carries the error code + message, the caller still writes
/// a response to the wire.
pub fn handle(message: &str, state: &mut ServerState) -> Option<Value> {
    // Step 1: parse the JSON-RPC envelope.
    let req: Request = match serde_json::from_str(message) {
        Ok(r) => r,
        Err(err) => {
            // Can't recover the id — JSON-RPC spec says id should be
            // null in this case.
            return Some(error_response(
                &Value::Null,
                &ProtocolError::parse_error(format!("invalid JSON: {err}")),
            ));
        }
    };

    let id = req.id.clone();
    let is_notification = id.is_none();

    // Step 2: lifecycle check. The spec lets us answer `initialize`
    // and any `notifications/*` before initialization, but
    // `tools/list` / `tools/call` / `shutdown` MUST wait.
    let needs_init = !matches!(
        req.method.as_str(),
        "initialize" | "notifications/initialized" | "notifications/cancelled"
    );
    if needs_init && !state.initialized {
        if is_notification {
            return None;
        }
        return Some(error_response(
            &id.unwrap_or(Value::Null),
            &ProtocolError::server_not_initialized(format!(
                "received `{}` before `initialize`",
                req.method
            )),
        ));
    }

    // Step 3: dispatch.
    let result = match req.method.as_str() {
        "initialize" => handle_initialize(state, req.params.as_ref()),
        "notifications/initialized" => {
            state.initialized = true;
            return None; // notification — no reply
        }
        "notifications/cancelled" => {
            // We don't run tools async, so cancellation is a no-op —
            // by the time we'd see the cancel notification, the tool
            // has already completed (or is about to).
            return None;
        }
        "shutdown" => Ok(Value::Null),
        "tools/list" => handle_tools_list(state),
        "tools/call" => handle_tools_call(state, req.params.as_ref()),
        // MCP also defines `ping` for keep-alive.
        "ping" => Ok(json!({})),
        other => {
            if is_notification {
                return None;
            }
            return Some(error_response(
                &id.unwrap_or(Value::Null),
                &ProtocolError::method_not_found(other),
            ));
        }
    };

    if is_notification {
        return None;
    }

    let id = id.unwrap_or(Value::Null);
    match result {
        Ok(value) => Some(
            serde_json::to_value(Response {
                jsonrpc: "2.0",
                id: &id,
                result: value,
            })
            .unwrap(),
        ),
        Err(err) => Some(error_response(&id, &err)),
    }
}

/// Build a JSON-RPC error response for a given id + error.
fn error_response(id: &Value, err: &ProtocolError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": err.code,
            "message": err.message,
        }
    })
}

// ----------------------------------------------------------------------
// Lifecycle methods
// ----------------------------------------------------------------------

fn handle_initialize(
    state: &mut ServerState,
    params: Option<&Value>,
) -> Result<Value, ProtocolError> {
    // We don't actually need anything from the client's params, but
    // peek at `protocolVersion` so we can echo it back if the client
    // asks for our supported version (rather than always echoing
    // ours, which would be silently wrong if they're on an older
    // version we still happen to support). Today we declare exactly
    // one supported version, so we ignore the client's value.
    let _ = params;

    // Mark initialized eagerly. Strictly the spec says we should wait
    // for `notifications/initialized`, but tools-only servers don't
    // do anything between the response and the notification, so it
    // doesn't matter — and it makes the loop cleaner if we accept
    // tools/list even from a client that skipped the notification
    // (some implementations do).
    state.initialized = true;

    Ok(json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            // listChanged: false because our tool set is static for
            // this binary version + feature set.
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "sqlrite-mcp",
            "version": env!("CARGO_PKG_VERSION"),
        }
    }))
}

// ----------------------------------------------------------------------
// tools/list
// ----------------------------------------------------------------------

fn handle_tools_list(state: &ServerState) -> Result<Value, ProtocolError> {
    Ok(json!({
        "tools": tools::list(state.read_only),
    }))
}

// ----------------------------------------------------------------------
// tools/call
// ----------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ToolsCallParams {
    name: String,
    #[serde(default)]
    arguments: Value,
}

fn handle_tools_call(
    state: &mut ServerState,
    params: Option<&Value>,
) -> Result<Value, ProtocolError> {
    let params = params.ok_or_else(|| {
        ProtocolError::invalid_params("tools/call requires `name` + `arguments` params")
    })?;
    let parsed: ToolsCallParams = serde_json::from_value(params.clone()).map_err(|err| {
        ProtocolError::invalid_params(format!("invalid tools/call params: {err}"))
    })?;

    // Hidden + rejected: `execute` under --read-only. We could rely
    // on the engine's read-only locking to surface a SQL error, but
    // a tool-level rejection gives the LLM a clearer message ("this
    // tool is disabled in read-only mode") than a lock-acquisition
    // failure deep inside the engine.
    if parsed.name == "execute" && state.read_only {
        return Ok(tool_error_result(
            "the `execute` tool is disabled in read-only mode (--read-only). \
             Use `query` for SELECT statements, or restart the server without --read-only.",
        ));
    }

    match tools::dispatch(&parsed.name, parsed.arguments, state) {
        Ok(text) => Ok(tool_text_result(text, false)),
        Err(crate::error::ToolError(msg)) => Ok(tool_text_result(msg, true)),
    }
}

/// Build a `{ content: [{type: "text", text}], isError }` shape —
/// the canonical MCP tool result.
pub(crate) fn tool_text_result(text: String, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

/// Convenience wrapper for tool-error responses.
pub(crate) fn tool_error_result(msg: impl Into<String>) -> Value {
    tool_text_result(msg.into(), true)
}
