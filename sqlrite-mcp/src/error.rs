//! Error taxonomy for the MCP server.
//!
//! Two layers, deliberately separate:
//!
//! - [`ProtocolError`] — JSON-RPC-level failures the LLM client can't
//!   recover from (malformed request, unknown method, server in wrong
//!   lifecycle state). Surfaced as JSON-RPC `error` responses with
//!   the standard codes from the JSON-RPC 2.0 spec.
//!
//! - [`ToolError`] — failures inside a tool handler (SQL parse error,
//!   table not found, vector dimension mismatch, ask returned empty
//!   SQL). Surfaced inside the tool result with `isError: true` and
//!   the error message in a `text` content block — that's MCP's
//!   convention for "the tool ran but the operation failed", and it
//!   lets the LLM read the error and retry with adjusted arguments.
//!
//! See `docs/mcp.md` § "Errors and how they surface" for the
//! user-facing reference.

/// JSON-RPC 2.0 standard error codes
/// (https://www.jsonrpc.org/specification#error_object).
///
/// MCP itself reserves the `-32000..=-32099` server-error range for
/// protocol-specific errors; we use a small subset of those.
#[allow(dead_code)]
pub mod jsonrpc_codes {
    /// Invalid JSON was received by the server.
    pub const PARSE_ERROR: i64 = -32700;
    /// The JSON sent is not a valid Request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The method does not exist / is not available.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid method parameter(s).
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal JSON-RPC error.
    pub const INTERNAL_ERROR: i64 = -32603;
    /// Server is in the wrong lifecycle state for the request
    /// (e.g. `tools/list` before `initialize`).
    pub const SERVER_NOT_INITIALIZED: i64 = -32002;
}

/// Protocol-level error. Becomes a JSON-RPC `error` response.
#[derive(Debug)]
pub struct ProtocolError {
    pub code: i64,
    pub message: String,
}

#[allow(dead_code)] // round-out-the-API constructors, used selectively
impl ProtocolError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::new(jsonrpc_codes::PARSE_ERROR, message)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(jsonrpc_codes::INVALID_REQUEST, message)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            jsonrpc_codes::METHOD_NOT_FOUND,
            format!("method not found: {method}"),
        )
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(jsonrpc_codes::INVALID_PARAMS, message)
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::new(jsonrpc_codes::INTERNAL_ERROR, message)
    }

    pub fn server_not_initialized(message: impl Into<String>) -> Self {
        Self::new(jsonrpc_codes::SERVER_NOT_INITIALIZED, message)
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for ProtocolError {}

/// Tool-level error. Becomes a tool result with `isError: true`.
///
/// This is intentionally just a wrapper around a string — the LLM
/// reads the message and decides how to react. Structured fields
/// would be over-engineering for a surface the LLM treats as a
/// natural-language response anyway.
#[derive(Debug)]
pub struct ToolError(pub String);

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ToolError {}

impl From<sqlrite::SQLRiteError> for ToolError {
    fn from(err: sqlrite::SQLRiteError) -> Self {
        Self::new(err.to_string())
    }
}
