//! `schema_dump` — full `CREATE TABLE` script for the database.
//!
//! Same output the `ask` tool feeds the LLM as ground truth, exposed
//! directly so a client can prime its own context once instead of
//! iterating `describe_table` per table. Cheap (walks `Database.tables`
//! alphabetically, formats each as a `CREATE TABLE ...;` statement).

use serde_json::{Value, json};

use crate::error::ToolError;
use crate::protocol::ServerState;

pub fn metadata() -> Value {
    json!({
        "name": "schema_dump",
        "description": "Return the full schema of the database as a sequence of \
                        `CREATE TABLE` statements (the same dump the `ask` tool \
                        feeds the LLM). Useful for priming your own context with the \
                        whole schema in one call rather than walking every table \
                        with `describe_table`. Tables are emitted in alphabetical \
                        order so the output is deterministic.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        }
    })
}

pub fn handle(_args: Value, state: &mut ServerState) -> Result<String, ToolError> {
    let dump = sqlrite::ask::schema::dump_schema_for_database(state.conn.database());
    Ok(dump)
}
