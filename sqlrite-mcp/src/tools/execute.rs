//! `execute` — run DDL / DML / transaction control.
//!
//! Disabled in `--read-only` mode. The protocol layer hides this tool
//! from `tools/list` AND rejects calls server-side, so a misbehaving
//! client can't bypass the flag.
//!
//! Returns the engine's status string ("3 rows inserted",
//! "table created", etc.) — same shape the REPL prints.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::protocol::ServerState;

pub fn metadata() -> Value {
    json!({
        "name": "execute",
        "description": "Execute a DDL, DML, or transaction-control statement against the \
                        database. Returns a status string describing what changed \
                        (e.g. \"3 rows inserted\", \"table users created\"). \
                        SELECT statements should go through the `query` tool instead — \
                        this tool returns a status string only, not row data. \
                        Disabled when the server runs with `--read-only`.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "sql": {
                    "type": "string",
                    "description": "A single non-SELECT statement (CREATE / INSERT / \
                                    UPDATE / DELETE / BEGIN / COMMIT / ROLLBACK / \
                                    DROP / ALTER, etc.).",
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
}

pub fn handle(args: Value, state: &mut ServerState) -> Result<String, ToolError> {
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ToolError::new(format!("invalid arguments: {e}")))?;

    let trimmed = args.sql.trim_start().to_ascii_lowercase();
    if trimmed.starts_with("select") {
        return Err(ToolError::new(
            "the `execute` tool returns a status string, not rows. \
             Use the `query` tool for SELECT statements."
                .to_string(),
        ));
    }

    let status = state.conn.execute(&args.sql)?;
    Ok(status)
}
