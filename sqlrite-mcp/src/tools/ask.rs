//! `ask` — natural-language → SQL via `sqlrite::ask::ask_with_database`.
//!
//! This is Phase 7g.8 living inside Phase 7h. Gated behind the crate's
//! `ask` cargo feature: the `tools/list` registry omits it when the
//! feature is off, and the dispatcher's `match` arm is `#[cfg]`-out.
//!
//! Configuration follows the same three-layer precedence the other
//! SDK adapters use: per-call args > `AskConfig::from_env()` > defaults.
//! Per-call overrides are read from the tool's `arguments` (`model`,
//! `max_tokens`, `cache_ttl`); env vars supply the rest (most importantly
//! `SQLRITE_LLM_API_KEY`, which the MCP client must export to the spawned
//! `sqlrite-mcp` process).
//!
//! Optional `execute: true` argument: after generating SQL, runs it
//! through `query` (if SELECT) or `execute` (if DML/DDL — and only
//! when not `--read-only`) and inlines the rows or status. Defaults
//! to `false` because the MCP client typically has its own loop and
//! prefers to call `query`/`execute` itself.

use serde::Deserialize;
use serde_json::{Value, json};

use sqlrite::ask::{AskConfig, AskResponse, CacheTtl, ask_with_database};

use crate::error::ToolError;
use crate::protocol::ServerState;
use crate::tools::{TOOL_OUTPUT_CAP_BYTES, value_to_json};

pub fn metadata() -> Value {
    json!({
        "name": "ask",
        "description": "Generate SQL from a natural-language question, grounded in this \
                        database's schema. Returns the generated SQL plus the model's \
                        one-sentence explanation. Optionally executes the SQL in the \
                        same call (`execute: true`); otherwise the caller decides what \
                        to do with the SQL — typically reviewing it before passing it \
                        to the `query` or `execute` tool. Requires `SQLRITE_LLM_API_KEY` \
                        in the server process's environment.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "Natural-language question (e.g. \"How many users are over 30?\").",
                },
                "execute": {
                    "type": "boolean",
                    "description": "If true, the generated SQL is also executed against this \
                                    database and the rows (for SELECT) or status string (for \
                                    DML/DDL) is included in the response. Default: false.",
                },
                "model": {
                    "type": "string",
                    "description": "Override the LLM model (default: claude-sonnet-4-6).",
                },
                "max_tokens": {
                    "type": "integer",
                    "description": "Override max output tokens (default: 1024).",
                    "minimum": 1,
                },
                "cache_ttl": {
                    "type": "string",
                    "enum": ["5m", "1h", "off"],
                    "description": "Override Anthropic prompt-cache TTL on the schema block (default: 5m).",
                },
            },
            "required": ["question"],
            "additionalProperties": false,
        }
    })
}

#[derive(Deserialize)]
struct Args {
    question: String,
    #[serde(default)]
    execute: bool,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    cache_ttl: Option<String>,
}

pub fn handle(args: Value, state: &mut ServerState) -> Result<String, ToolError> {
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ToolError::new(format!("invalid arguments: {e}")))?;

    if args.question.trim().is_empty() {
        return Err(ToolError::new("question must not be empty".to_string()));
    }

    // Build the config: env-derived base, then per-call overrides.
    let mut cfg = AskConfig::from_env().map_err(|e| {
        ToolError::new(format!(
            "ask config: {e} (set SQLRITE_LLM_API_KEY in the environment of the \
             spawned `sqlrite-mcp` process — typically via the MCP client's server \
             config)"
        ))
    })?;
    if let Some(m) = args.model {
        cfg.model = m;
    }
    if let Some(mt) = args.max_tokens {
        cfg.max_tokens = mt;
    }
    if let Some(ttl) = args.cache_ttl {
        cfg.cache_ttl = match ttl.as_str() {
            "5m" => CacheTtl::FiveMinutes,
            "1h" => CacheTtl::OneHour,
            "off" => CacheTtl::Off,
            other => {
                return Err(ToolError::new(format!(
                    "invalid cache_ttl `{other}`. Use `5m`, `1h`, or `off`."
                )));
            }
        };
    }

    // Run the LLM call.
    let resp: AskResponse = ask_with_database(state.conn.database(), &args.question, &cfg)
        .map_err(|e| ToolError::new(format!("ask failed: {e}")))?;

    let mut result = json!({
        "sql": resp.sql,
        "explanation": resp.explanation,
        "usage": {
            "input_tokens": resp.usage.input_tokens,
            "output_tokens": resp.usage.output_tokens,
            "cache_creation_input_tokens": resp.usage.cache_creation_input_tokens,
            "cache_read_input_tokens": resp.usage.cache_read_input_tokens,
        },
        "executed": false,
    });

    // Optional inline execution. Only kicks in if (a) caller asked
    // for it, (b) we got non-empty SQL back, (c) for DML/DDL, the
    // server isn't read-only.
    if args.execute && !resp.sql.trim().is_empty() {
        let trimmed = resp.sql.trim_start().to_ascii_lowercase();
        let is_select = trimmed.starts_with("select");

        if is_select {
            let exec_result = run_inline_select(&resp.sql, state);
            match exec_result {
                Ok(rows) => {
                    result["executed"] = json!(true);
                    result["rows"] = rows;
                }
                Err(e) => {
                    result["execute_error"] = json!(e.0);
                }
            }
        } else if state.read_only {
            result["execute_error"] = json!(
                "generated SQL is non-SELECT; not executed because server is in --read-only mode"
            );
        } else {
            match state.conn.execute(&resp.sql) {
                Ok(status) => {
                    result["executed"] = json!(true);
                    result["status"] = json!(status);
                }
                Err(e) => {
                    result["execute_error"] = json!(e.to_string());
                }
            }
        }
    }

    serde_json::to_string_pretty(&result)
        .map_err(|e| ToolError::new(format!("internal: failed to serialize ask response: {e}")))
}

fn run_inline_select(sql: &str, state: &mut ServerState) -> Result<Value, ToolError> {
    let stmt = state.conn.prepare(sql)?;
    let mut rows = stmt.query()?;
    let columns = rows.columns().to_vec();
    let mut out: Vec<Value> = Vec::new();
    let mut size_estimate = 0;
    while let Some(row) = rows.next()? {
        let mut obj = serde_json::Map::with_capacity(columns.len());
        for (i, col) in columns.iter().enumerate() {
            let v: sqlrite::Value = row.get(i)?;
            let json_val = value_to_json(&v);
            size_estimate += col.len() + 8 + json_val.to_string().len();
            obj.insert(col.clone(), json_val);
        }
        if size_estimate > TOOL_OUTPUT_CAP_BYTES {
            break;
        }
        out.push(Value::Object(obj));
    }
    Ok(Value::Array(out))
}
