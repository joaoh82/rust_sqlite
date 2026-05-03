//! `bm25_search` — top-k keyword retrieval against an FTS-indexed
//! TEXT column.
//!
//! Convenience wrapper over `SELECT * FROM <t> WHERE fts_match(<col>,
//! 'q') ORDER BY bm25_score(<col>, 'q') DESC LIMIT k`. The LLM could
//! compose that query itself via the `query` tool, but a typed
//! `bm25_search(table, column, query, k)` removes a few common
//! mistakes — forgetting the `WHERE fts_match` pre-filter, dropping
//! the `DESC` (BM25 is "higher = better"), or quoting the query
//! string wrong.
//!
//! Picks up the engine's FTS optimizer hook automatically: if the
//! column has a `CREATE INDEX … USING fts (col)` index built (Phase
//! 8b), the engine recognizes the `ORDER BY bm25_score(col, 'q') DESC
//! LIMIT k` shape and probes the posting list directly. No special
//! handling needed at this layer — we just emit the canonical SQL.
//!
//! Symmetric with [`super::vector_search`] (Phase 7h), which does the
//! same thing for vector cosine/L2/dot distances.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::protocol::ServerState;
use crate::tools::{TOOL_OUTPUT_CAP_BYTES, is_safe_identifier, value_to_json};

const DEFAULT_K: usize = 10;
const HARD_CAP_K: usize = 100;

pub fn metadata() -> Value {
    json!({
        "name": "bm25_search",
        "description": "Find the top-k rows ranked by BM25 keyword relevance against \
                        an FTS-indexed TEXT column. Requires a `CREATE INDEX … USING fts \
                        (column)` to exist on the column; errors otherwise. Uses any-term \
                        OR semantics (a row matches if it contains ANY of the query \
                        terms). Returns the table's columns for the k highest-scoring \
                        rows, in descending BM25 order. Pairs naturally with `vector_search` \
                        for hybrid retrieval — the LLM can call both and fuse results \
                        client-side, or compose them in a single SQL via the `query` tool.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "table": {
                    "type": "string",
                    "description": "Table name. Must match `[A-Za-z_][A-Za-z0-9_]*`.",
                },
                "column": {
                    "type": "string",
                    "description": "FTS-indexed TEXT column on the table. Must match \
                                    `[A-Za-z_][A-Za-z0-9_]*`.",
                },
                "query": {
                    "type": "string",
                    "description": "Free-text query. Tokenized the same way the index \
                                    was built (ASCII split + lowercase for the MVP).",
                },
                "k": {
                    "type": "integer",
                    "description": "Number of top-ranked rows to return (1..=100, default 10).",
                    "minimum": 1,
                    "maximum": 100,
                },
            },
            "required": ["table", "column", "query"],
            "additionalProperties": false,
        }
    })
}

#[derive(Deserialize)]
struct Args {
    table: String,
    column: String,
    query: String,
    #[serde(default)]
    k: Option<usize>,
}

pub fn handle(args: Value, state: &mut ServerState) -> Result<String, ToolError> {
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ToolError::new(format!("invalid arguments: {e}")))?;

    if !is_safe_identifier(&args.table) {
        return Err(ToolError::new(format!(
            "invalid table name `{}`",
            args.table
        )));
    }
    if !is_safe_identifier(&args.column) {
        return Err(ToolError::new(format!(
            "invalid column name `{}`",
            args.column
        )));
    }
    if args.query.is_empty() {
        return Err(ToolError::new(
            "query must be a non-empty string".to_string(),
        ));
    }

    let k = args.k.unwrap_or(DEFAULT_K).clamp(1, HARD_CAP_K);

    // Pre-flight: confirm the column exists, is TEXT, and has an FTS
    // index attached. Cheaper than a SQL-level error after a partial
    // scan, and the message names exactly what's missing — useful for
    // an LLM trying to recover.
    {
        let db = state.conn.database();
        let table = db
            .get_table(args.table.clone())
            .map_err(|e| ToolError::new(format!("table `{}` not found: {e}", args.table)))?;
        let target = table
            .columns
            .iter()
            .find(|c| c.column_name == args.column)
            .ok_or_else(|| {
                ToolError::new(format!(
                    "column `{}` not found on table `{}`",
                    args.column, args.table,
                ))
            })?;
        if !matches!(target.datatype, sqlrite::sql::db::table::DataType::Text) {
            return Err(ToolError::new(format!(
                "column `{}` on table `{}` is `{}`, not a TEXT column",
                args.column, args.table, target.datatype,
            )));
        }
        if !table
            .fts_indexes
            .iter()
            .any(|i| i.column_name == args.column)
        {
            return Err(ToolError::new(format!(
                "column `{}` on table `{}` has no FTS index — \
                 run `CREATE INDEX <name> ON {} USING fts ({})` first",
                args.column, args.table, args.table, args.column,
            )));
        }
    }

    // Embed the query string into a SQL literal. The engine's parser
    // accepts single-quoted strings; escape any embedded apostrophes
    // by doubling them (SQL standard).
    let query_lit = sql_string_literal(&args.query);

    // Canonical FTS top-k shape — matches `try_fts_probe`'s recognition.
    // We intentionally include the WHERE clause even though the probe
    // overwrites `matching` (Q6 trade-off in the Phase 8 plan); having
    // it here keeps the SQL semantically correct on the brute-force
    // fallback path and surfaces a clean error if the user's column
    // happened to lose its FTS index between pre-flight and execution.
    let sql = format!(
        "SELECT * FROM {tbl} WHERE fts_match({col}, {q}) \
         ORDER BY bm25_score({col}, {q}) DESC LIMIT {k}",
        tbl = args.table,
        col = args.column,
        q = query_lit,
        k = k,
    );

    let stmt = state.conn.prepare(&sql)?;
    let mut rows = stmt.query()?;
    let columns = rows.columns().to_vec();
    let mut out: Vec<Value> = Vec::with_capacity(k);
    let mut size_estimate = 0;
    let mut byte_truncated = false;

    while let Some(row) = rows.next()? {
        let mut obj = serde_json::Map::with_capacity(columns.len());
        for (i, col) in columns.iter().enumerate() {
            let v: sqlrite::Value = row.get(i)?;
            let json_val = value_to_json(&v);
            size_estimate += col.len() + 8 + json_val.to_string().len();
            obj.insert(col.clone(), json_val);
        }
        if size_estimate > TOOL_OUTPUT_CAP_BYTES {
            byte_truncated = true;
            break;
        }
        out.push(Value::Object(obj));
    }

    let mut result = json!({
        "table": args.table,
        "column": args.column,
        "query": args.query,
        "k_requested": k,
        "rows": out,
    });
    if byte_truncated {
        result["truncated"] = json!(true);
        result["truncation_reason"] = json!(format!(
            "response truncated at {} bytes ({} of {} rows shown)",
            TOOL_OUTPUT_CAP_BYTES,
            out.len(),
            k,
        ));
    }
    serde_json::to_string_pretty(&result)
        .map_err(|e| ToolError::new(format!("internal: failed to serialize results: {e}")))
}

/// Wrap `s` as a single-quoted SQL string literal, doubling any
/// embedded single quotes per SQL standard. The engine's tokenizer
/// then strips both the wrapping quotes and reduces `''` back to `'`.
fn sql_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_string_literal_doubles_quotes() {
        assert_eq!(sql_string_literal("rust"), "'rust'");
        assert_eq!(sql_string_literal("it's fast"), "'it''s fast'");
        assert_eq!(sql_string_literal(""), "''");
    }
}
