//! Stdio transport — line-delimited JSON-RPC 2.0.
//!
//! The MCP wire format on stdio is "one JSON value per line, UTF-8,
//! terminated by `\n`" (per the 2025-11-25 spec — no Content-Length
//! framing like LSP, no length prefixes). That makes the runner ~80
//! lines: read a line, parse it, dispatch it through `protocol::handle`,
//! write the response back as one line of JSON, repeat until EOF.
//!
//! The loop is strictly serial. MCP clients can pipeline requests
//! (multiple `id`s in flight), but our SQL engine is sync + not
//! concurrent-safe for mutation, so we process them one at a time
//! in arrival order. The client sees serialized completion, which
//! is correct — just not parallel. Documented in `docs/mcp.md`.
//!
//! `BufReader::with_capacity(MAX_LINE_BYTES)` because LLM clients
//! sometimes pass very long single-line JSON (a 64 KB SQL string in
//! `tools/call`'s arguments). The default 8 KB buffer would split
//! such a line and break parsing. We cap at 1 MiB; anything larger
//! is rejected with a JSON-RPC parse error.

use std::io::{BufRead, BufReader, Read, Write};

use sqlrite::Connection;

use crate::error::ProtocolError;
use crate::protocol::{ServerState, handle};

/// Cap on a single inbound message size. 1 MiB is generous; the
/// largest realistic payload is a `tools/call` for `execute` with a
/// `INSERT … VALUES (…)` blob, which rarely exceeds 100 KiB.
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Run the transport loop until stdin closes or we hit a fatal I/O
/// error. Returns `Ok(())` on clean EOF; `Err` only for I/O failures
/// on stdout that prevent us from communicating at all (a broken
/// pipe from the client is treated as clean shutdown).
pub fn run<R: Read, W: Write>(
    stdin: R,
    mut stdout: W,
    conn: Connection,
    read_only: bool,
) -> std::io::Result<()> {
    let mut reader = BufReader::with_capacity(MAX_LINE_BYTES, stdin);
    let mut state = ServerState::new(conn, read_only);
    let mut line = String::new();

    loop {
        line.clear();
        let n = match reader.read_line(&mut line) {
            Ok(0) => return Ok(()), // EOF — clean shutdown.
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
            Err(err) => return Err(err),
        };

        // Strip the trailing newline(s). `read_line` keeps the `\n`
        // (and may include `\r\n` on Windows); the JSON parser is fine
        // with either, but trimming makes diagnostics cleaner.
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            continue; // Skip blank lines defensively.
        }

        // Reject oversized messages outright. `read_line` doesn't
        // enforce the buffer cap by itself — it grows the String to
        // fit. Check post-read so we can return a JSON-RPC error
        // tied to the request's id (if we can extract it) instead of
        // crashing the loop.
        if n > MAX_LINE_BYTES {
            write_protocol_error(
                &mut stdout,
                None,
                ProtocolError::parse_error(format!(
                    "request exceeds maximum size of {} bytes",
                    MAX_LINE_BYTES
                )),
            )?;
            continue;
        }

        // Catch panics in the dispatcher so a misbehaving tool
        // (corrupt cell payload, future engine bug) doesn't take
        // the whole server process down. Convert to a JSON-RPC
        // internal error tied to the offending request.
        let response = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle(trimmed, &mut state)
        })) {
            Ok(resp) => resp,
            Err(panic) => {
                let msg = panic_to_string(panic);
                eprintln!("[sqlrite-mcp] panic in handler: {msg}");
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {
                        "code": crate::error::jsonrpc_codes::INTERNAL_ERROR,
                        "message": format!("internal server error: {msg}"),
                    }
                }))
            }
        };

        if let Some(resp) = response {
            // One line of JSON, then `\n`. Flush after every message
            // so the client sees responses immediately rather than
            // waiting for a buffer to fill.
            serde_json::to_writer(&mut stdout, &resp)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
        // None response = JSON-RPC notification (no id, no reply).
        // Spec-correct to stay silent.
    }
}

/// Emit a JSON-RPC error response with the given id (or `null` if we
/// couldn't extract one from the request). Used when the request was
/// so malformed we can't go through the normal dispatch path.
fn write_protocol_error<W: Write>(
    mut stdout: W,
    id: Option<serde_json::Value>,
    err: ProtocolError,
) -> std::io::Result<()> {
    let resp = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(serde_json::Value::Null),
        "error": {
            "code": err.code,
            "message": err.message,
        }
    });
    serde_json::to_writer(&mut stdout, &resp)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

/// Best-effort conversion of a panic payload to a string. Panics are
/// usually `&'static str` or `String`; anything else gets a generic
/// placeholder.
fn panic_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}
