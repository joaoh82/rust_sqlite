//! `query` — run a SELECT, return rows as JSON.
//!
//! Tool-side restriction: SELECT only. DDL/DML attempts get redirected
//! to `execute` with a clear tool-error message — that gives the LLM
//! a friendlier signal than a SQL parse failure deep inside the engine.
//!
//! Default limit on rows returned: 100. Caller can override via the
//! `limit` argument up to a hard cap (1000), beyond which the response
//! gets too large to be useful in an LLM turn anyway.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::protocol::ServerState;
use crate::tools::{TOOL_OUTPUT_CAP_BYTES, value_to_json};

const DEFAULT_LIMIT: u64 = 100;
const HARD_CAP_LIMIT: u64 = 1000;

pub fn metadata() -> Value {
    json!({
        "name": "query",
        "description": "Execute a SELECT query against the database and return matching \
                        rows as a JSON array of objects (key = column name). \
                        SELECT-only — for INSERT/UPDATE/DELETE/CREATE use the \
                        `execute` tool instead. Add `LIMIT` clauses on large tables; \
                        the tool also caps results at `limit` (default 100, max 1000) \
                        and notes any truncation in the response.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "sql": {
                    "type": "string",
                    "description": "A single SELECT statement.",
                },
                "limit": {
                    "type": "integer",
                    "description": "Max rows to return (1..=1000, default 100). \
                                    This is enforced at the tool layer in addition \
                                    to any LIMIT clause in the SQL itself.",
                    "minimum": 1,
                    "maximum": 1000,
                },
            },
            "required": ["sql"],
            "additionalProperties": false,
        }
    })
}

#[derive(Deserialize)]
struct Args {
    sql: String,
    #[serde(default)]
    limit: Option<u64>,
}

pub fn handle(args: Value, state: &mut ServerState) -> Result<String, ToolError> {
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ToolError::new(format!("invalid arguments: {e}")))?;

    let trimmed = args.sql.trim_start();
    let lowered = trimmed.to_ascii_lowercase();
    if !lowered.starts_with("select") {
        return Err(ToolError::new(
            "the `query` tool only accepts SELECT statements. \
             Use the `execute` tool for INSERT, UPDATE, DELETE, CREATE, etc."
                .to_string(),
        ));
    }

    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).min(HARD_CAP_LIMIT) as usize;

    let stmt = state.conn.prepare(&args.sql)?;
    let mut rows = stmt.query()?;
    let columns = rows.columns().to_vec();

    let mut out: Vec<Value> = Vec::new();
    let mut total_seen: usize = 0;
    let mut byte_truncated = false;
    let mut size_estimate: usize = 0;

    while let Some(row) = rows.next()? {
        total_seen += 1;
        if out.len() >= limit {
            // Drain the remaining rows so we can report a faithful
            // "saw N rows, kept M" count. Cheap because Rows is
            // already materialized in-memory in the current engine.
            continue;
        }

        let mut obj = serde_json::Map::with_capacity(columns.len());
        for (i, col) in columns.iter().enumerate() {
            let v: sqlrite::Value = row.get(i)?;
            let json_val = value_to_json(&v);
            // Rough byte estimate before serialization. Conservative —
            // we count the raw value's display length plus column-name
            // overhead. Doesn't need to be exact, just enough to catch
            // a runaway LOB column.
            size_estimate += col.len() + 8 + json_val.to_string().len();
            obj.insert(col.clone(), json_val);
        }

        if size_estimate > TOOL_OUTPUT_CAP_BYTES {
            byte_truncated = true;
            // Don't push this row; it's the one that crossed the line.
            break;
        }
        out.push(Value::Object(obj));
    }

    // If we byte-truncated, drain the rest to get the true total.
    if byte_truncated {
        while rows.next()?.is_some() {
            total_seen += 1;
        }
    }

    let kept = out.len();
    let truncated = byte_truncated || (total_seen > kept);
    let mut result = json!({
        "columns": columns,
        "rows": out,
        "row_count": kept,
    });
    if truncated {
        let reason = if byte_truncated {
            format!(
                "response truncated at {} bytes ({} of {} rows shown)",
                TOOL_OUTPUT_CAP_BYTES, kept, total_seen
            )
        } else {
            format!(
                "row limit {} reached ({} of {} rows shown)",
                limit, kept, total_seen
            )
        };
        result["truncated"] = json!(true);
        result["truncation_reason"] = json!(reason);
        result["total_seen"] = json!(total_seen);
    }

    serde_json::to_string_pretty(&result)
        .map_err(|e| ToolError::new(format!("internal: failed to serialize rows: {e}")))
}
