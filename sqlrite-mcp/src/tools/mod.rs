//! Tool registry + dispatch.
//!
//! Each tool is one file under this module. The registry is just two
//! tables — [`list`] returns the metadata MCP clients see in
//! `tools/list`, and [`dispatch`] routes a `tools/call` to the right
//! handler.
//!
//! No trait + `Box<dyn>` magic. A `match name` dispatcher is ~40
//! lines and reads top-to-bottom; the cost of being clever wouldn't
//! pay back over 7 tools.
//!
//! ## Adding a new tool
//!
//! 1. Create `tools/<name>.rs` with a `pub fn handle(args, state)`
//!    function returning `Result<String, ToolError>`.
//! 2. Add a `mod` line below.
//! 3. Add a metadata entry in [`list`] (name + description + input
//!    schema as a `serde_json::json!` literal).
//! 4. Add a `match` arm in [`dispatch`] that calls the handler.
//! 5. Update `docs/mcp.md` and (if it changes user-visible behavior)
//!    `sqlrite-mcp/README.md`.

use serde_json::{Value, json};

use crate::error::ToolError;
use crate::protocol::ServerState;

#[cfg(feature = "ask")]
mod ask;
mod bm25_search;
mod describe_table;
mod execute;
mod list_tables;
mod query;
mod schema_dump;
mod vector_search;

/// MCP `tools/list` result. Returns the metadata for each tool —
/// name, description, JSON Schema for input. The order matters
/// only for human readability in the client UI; we go DB-discovery
/// → SELECT → DML → retrieval (vector + bm25) → ask.
pub fn list(read_only: bool) -> Vec<Value> {
    let mut tools = Vec::with_capacity(8);

    tools.push(list_tables::metadata());
    tools.push(describe_table::metadata());
    tools.push(query::metadata());

    // The `execute` tool is hidden in read-only mode. The client
    // can't see it in `tools/list`, so the LLM doesn't even attempt
    // a write. Belt + suspenders: `protocol::handle_tools_call`
    // also rejects it server-side if a client calls it anyway.
    if !read_only {
        tools.push(execute::metadata());
    }

    tools.push(schema_dump::metadata());
    tools.push(vector_search::metadata());
    tools.push(bm25_search::metadata());

    #[cfg(feature = "ask")]
    tools.push(ask::metadata());

    tools
}

/// Dispatch a `tools/call`. The protocol layer has already validated
/// that we're past `initialize`; this function just routes by name.
///
/// Returns the raw text the tool wants in its `content[0].text`
/// block. Tool-execution errors come back as [`ToolError`]; the
/// protocol layer wraps them into `isError: true` results.
pub fn dispatch(name: &str, args: Value, state: &mut ServerState) -> Result<String, ToolError> {
    match name {
        "list_tables" => list_tables::handle(args, state),
        "describe_table" => describe_table::handle(args, state),
        "query" => query::handle(args, state),
        "execute" => execute::handle(args, state),
        "schema_dump" => schema_dump::handle(args, state),
        "vector_search" => vector_search::handle(args, state),
        "bm25_search" => bm25_search::handle(args, state),
        #[cfg(feature = "ask")]
        "ask" => ask::handle(args, state),
        // Unknown tool: this is a tool-error not a protocol-error,
        // because the LLM might just be guessing a tool name and we
        // want it to recover by re-reading `tools/list`.
        other => Err(ToolError::new(format!(
            "unknown tool: `{other}`. Call `tools/list` to see available tools."
        ))),
    }
}

// ----------------------------------------------------------------------
// Shared helpers — used by multiple tool handlers.
// ----------------------------------------------------------------------

/// Cap on the size of a tool's text response. Beyond this, the row
/// list is truncated and a marker line is appended. Picked to stay
/// well under the per-message budget most LLM clients allocate
/// (Claude Code allows ~256 KiB per turn; 64 KiB leaves headroom
/// for the rest of the response wrapping).
pub(crate) const TOOL_OUTPUT_CAP_BYTES: usize = 64 * 1024;

/// Best-effort identifier validator. Used by `describe_table` to
/// avoid passing the LLM's `name` argument directly through to a
/// `SELECT COUNT(*) FROM <name>` query.
///
/// SQLite identifiers can in fact contain almost anything if quoted,
/// but for tool input we only allow the un-quoted-identifier subset
/// (`[A-Za-z_][A-Za-z0-9_]*`). If a real database has weirder table
/// names, the user can still call `query` directly — `describe_table`
/// just won't be available for them.
pub(crate) fn is_safe_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Convert a single engine `Value` into a JSON value suitable for
/// dropping into a row object. Vectors become bracket arrays of
/// numbers (matches the literal syntax users write in INSERTs);
/// NULL becomes JSON null.
pub(crate) fn value_to_json(v: &sqlrite::Value) -> Value {
    match v {
        sqlrite::Value::Integer(i) => json!(i),
        sqlrite::Value::Real(f) => {
            // serde_json refuses to encode NaN / Inf — those slip
            // through to here only via aggregate ops on bad data.
            // Fall back to null so the response stays valid JSON.
            if f.is_finite() { json!(f) } else { Value::Null }
        }
        sqlrite::Value::Text(s) => json!(s),
        sqlrite::Value::Bool(b) => json!(b),
        sqlrite::Value::Vector(v) => {
            let arr: Vec<Value> = v
                .iter()
                .map(|f| if f.is_finite() { json!(f) } else { Value::Null })
                .collect();
            Value::Array(arr)
        }
        sqlrite::Value::Null => Value::Null,
    }
}
