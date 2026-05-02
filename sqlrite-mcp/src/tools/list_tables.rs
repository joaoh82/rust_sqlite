//! `list_tables` — return every user-visible table name.
//!
//! Cheapest tool we expose; LLMs typically call it first to discover
//! what's in the database before deciding which more-specific tool
//! to call. Excludes `sqlrite_master` (the engine's own catalog) so
//! the LLM doesn't waste a turn asking about it.

use serde_json::{Value, json};

use crate::error::ToolError;
use crate::protocol::ServerState;

pub fn metadata() -> Value {
    json!({
        "name": "list_tables",
        "description": "List the names of every user-defined table in the database. \
                        Returns a JSON array of strings, sorted alphabetically. \
                        Excludes the engine's `sqlrite_master` catalog. Call this \
                        first to discover what's available before using `describe_table` \
                        or `query`.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        }
    })
}

pub fn handle(_args: Value, state: &mut ServerState) -> Result<String, ToolError> {
    let db = state.conn.database();
    let mut names: Vec<&String> = db
        .tables
        .keys()
        .filter(|n| n.as_str() != sqlrite::MASTER_TABLE_NAME)
        .collect();
    names.sort();

    let json = serde_json::to_string_pretty(&names)
        .map_err(|e| ToolError::new(format!("internal: failed to serialize table names: {e}")))?;
    Ok(json)
}
