//! `describe_table` — column metadata + row count for one table.
//!
//! The LLM uses this to learn a table's shape before composing a
//! query. We return:
//!
//! - `columns`: array of `{name, type, primary_key, not_null, unique}`
//! - `row_count`: integer (cheap because the engine tracks rowid
//!   ranges; we run `SELECT COUNT(*)` rather than reach into private
//!   internals)
//!
//! Input validation: the table name must match the safe-identifier
//! regex (`[A-Za-z_][A-Za-z0-9_]*`). The engine looks the table up by
//! name in a HashMap so SQL injection isn't a concern at the lookup
//! stage, but we *do* concatenate the name into the COUNT(*) query
//! below — that needs the validation.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::protocol::ServerState;
use crate::tools::is_safe_identifier;

pub fn metadata() -> Value {
    json!({
        "name": "describe_table",
        "description": "Return column metadata and row count for a table. \
                        Use this to learn a table's shape before composing a \
                        SELECT or INSERT. Each column reports its name, declared \
                        type, primary-key flag, NOT NULL flag, and UNIQUE flag.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The table name. Must match `[A-Za-z_][A-Za-z0-9_]*`.",
                },
            },
            "required": ["name"],
            "additionalProperties": false,
        }
    })
}

#[derive(Deserialize)]
struct Args {
    name: String,
}

pub fn handle(args: Value, state: &mut ServerState) -> Result<String, ToolError> {
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ToolError::new(format!("invalid arguments: {e}")))?;

    if !is_safe_identifier(&args.name) {
        return Err(ToolError::new(format!(
            "invalid table name `{}` — only `[A-Za-z_][A-Za-z0-9_]*` is accepted",
            args.name
        )));
    }

    // Reach the table via the engine's typed lookup. `get_table` takes
    // an owned String — clone here so we can still use args.name for
    // the row-count query below.
    let db = state.conn.database();
    let table = db
        .get_table(args.name.clone())
        .map_err(|e| ToolError::new(format!("table `{}` not found: {e}", args.name)))?;

    let columns: Vec<Value> = table
        .columns
        .iter()
        .map(|c| {
            json!({
                "name": c.column_name,
                "type": c.datatype.to_string(),
                "primary_key": c.is_pk,
                "not_null": c.not_null,
                "unique": c.is_unique,
            })
        })
        .collect();

    // Row count via `Table::rowids()`. The engine doesn't yet support
    // SELECT COUNT(*) aggregates (deferred — see the existing Go SDK
    // workaround in `sdk/go/test/sqlrite_test.go`), so we ask the
    // table for its rowid list directly and report `.len()`. Costs an
    // O(N) walk; the right fix is to expose a `Table::row_count()`
    // when the executor grows aggregate support.
    let row_count = table.rowids().len() as i64;

    let result = json!({
        "name": args.name,
        "columns": columns,
        "row_count": row_count,
    });

    serde_json::to_string_pretty(&result)
        .map_err(|e| ToolError::new(format!("internal: failed to serialize: {e}")))
}
