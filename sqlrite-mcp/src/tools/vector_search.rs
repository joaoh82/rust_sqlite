//! `vector_search` — k-nearest-neighbor lookup against a vector column.
//!
//! Convenience wrapper over `SELECT *, vec_distance_<metric>(col, embedding) AS
//! distance FROM table ORDER BY distance LIMIT k`. The LLM could
//! compose that query itself via the `query` tool, but having a typed
//! `vector_search(table, column, embedding, k, metric)` is materially
//! easier for the model — fewer chances to forget the bracket syntax,
//! get the metric name right, or include the distance projection.
//!
//! Picks up the engine's HNSW path automatically: if the column has a
//! `CREATE INDEX … USING hnsw (col)` index built (Phase 7d), the
//! engine's optimizer recognizes the `ORDER BY vec_distance_l2(col, …)
//! LIMIT k` shape and probes the HNSW graph instead of full-scanning.
//! No special handling needed at this layer — we just emit the same
//! SQL the optimizer is wired to recognize.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::protocol::ServerState;
use crate::tools::{TOOL_OUTPUT_CAP_BYTES, is_safe_identifier, value_to_json};

const DEFAULT_K: usize = 10;
const HARD_CAP_K: usize = 100;

pub fn metadata() -> Value {
    json!({
        "name": "vector_search",
        "description": "Find the k nearest rows to a query embedding in a VECTOR column. \
                        Uses an HNSW index automatically if one is built on the column \
                        (CREATE INDEX … USING hnsw); otherwise falls back to a brute-force \
                        scan. Returns the table's columns for the k closest rows, in \
                        ascending distance order. Supported metrics: `l2` (Euclidean, \
                        default), `cosine`, `dot`. (The numeric distance value is not \
                        included in the response — the engine doesn't yet support \
                        function calls in SELECT projections.)",
        "inputSchema": {
            "type": "object",
            "properties": {
                "table": {
                    "type": "string",
                    "description": "Table name. Must match `[A-Za-z_][A-Za-z0-9_]*`.",
                },
                "column": {
                    "type": "string",
                    "description": "VECTOR column on the table. Must match `[A-Za-z_][A-Za-z0-9_]*`.",
                },
                "embedding": {
                    "type": "array",
                    "items": { "type": "number" },
                    "description": "Query vector. Length must match the column's declared dimension.",
                },
                "k": {
                    "type": "integer",
                    "description": "Number of nearest neighbors to return (1..=100, default 10).",
                    "minimum": 1,
                    "maximum": 100,
                },
                "metric": {
                    "type": "string",
                    "enum": ["l2", "cosine", "dot"],
                    "description": "Distance metric (default `l2`).",
                },
            },
            "required": ["table", "column", "embedding"],
            "additionalProperties": false,
        }
    })
}

#[derive(Deserialize)]
struct Args {
    table: String,
    column: String,
    embedding: Vec<f64>,
    #[serde(default)]
    k: Option<usize>,
    #[serde(default)]
    metric: Option<String>,
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
    if args.embedding.is_empty() {
        return Err(ToolError::new(
            "embedding must be a non-empty array of numbers".to_string(),
        ));
    }

    let metric_fn = match args.metric.as_deref().unwrap_or("l2") {
        "l2" => "vec_distance_l2",
        "cosine" => "vec_distance_cosine",
        "dot" => "vec_distance_dot",
        other => {
            return Err(ToolError::new(format!(
                "unsupported metric `{other}`. Use `l2`, `cosine`, or `dot`."
            )));
        }
    };

    let k = args.k.unwrap_or(DEFAULT_K).clamp(1, HARD_CAP_K);

    // Format the embedding as the engine's bracket-array literal:
    // [0.1, 0.2, 0.3]. Use a fixed precision so two callers asking
    // the same question produce byte-identical SQL — handy for the
    // engine's prepared-plan cache (when 5b lands) and for any
    // request-level caching downstream.
    let embedding_lit = format_vector_literal(&args.embedding);

    // Sanity-check dimension up front. Cheaper than letting the
    // engine fail after a partial scan, with a clearer error
    // message that names both sides of the mismatch.
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
        if let sqlrite::sql::db::table::DataType::Vector(dim) = target.datatype {
            if args.embedding.len() != dim as usize {
                return Err(ToolError::new(format!(
                    "embedding has {} dimensions but column `{}` is VECTOR({}) — \
                     dimension mismatch",
                    args.embedding.len(),
                    args.column,
                    dim,
                )));
            }
        } else {
            return Err(ToolError::new(format!(
                "column `{}` on table `{}` is `{}`, not a VECTOR column",
                args.column, args.table, target.datatype,
            )));
        }
    }

    // SQL shape: `SELECT * FROM <t> ORDER BY <metric>(<col>, <emb>) LIMIT k`.
    //
    // The engine doesn't currently allow function calls in the
    // SELECT projection list ("Only bare column references are
    // supported in the projection list"), so we can't return a
    // computed `distance` column. ORDER BY accepts the function
    // expression just fine — and it's where the HNSW optimizer hook
    // is wired anyway, so the LLM still gets the nearest-k semantics
    // it asked for. The distance value itself is omitted; if a caller
    // really needs it, they can compute it client-side from the
    // returned embedding column.
    let sql = format!(
        "SELECT * FROM {tbl} ORDER BY {fn_}({col}, {emb}) ASC LIMIT {k}",
        fn_ = metric_fn,
        col = args.column,
        emb = embedding_lit,
        tbl = args.table,
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
        "metric": args.metric.unwrap_or_else(|| "l2".to_string()),
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

/// Format a `&[f64]` as a SQLRite vector literal: `[1.0, 2.0, 3.0]`.
/// Uses a debug-style float formatting so e.g. `1.0` doesn't print as
/// `1` (which the parser also accepts, but the round-trippable form
/// is friendlier to anyone debugging the generated SQL).
fn format_vector_literal(v: &[f64]) -> String {
    let mut s = String::with_capacity(v.len() * 6 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        if !x.is_finite() {
            // The engine's parser rejects NaN/Inf in literals; emit
            // 0.0 and trust the dimension-mismatch path above to have
            // caught most callers, plus the engine's eventual rejection
            // for the rest. Could also return an error here; keeping
            // it permissive matches the `query` tool's approach.
            s.push_str("0.0");
        } else {
            // Use Display, which gives `1` for `1.0`. Append `.0` if
            // there's no decimal point so the literal is unambiguously
            // floating point.
            let formatted = format!("{x}");
            if formatted.contains('.') || formatted.contains('e') || formatted.contains('E') {
                s.push_str(&formatted);
            } else {
                s.push_str(&formatted);
                s.push_str(".0");
            }
        }
    }
    s.push(']');
    s
}
