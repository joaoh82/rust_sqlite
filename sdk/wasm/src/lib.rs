//! WebAssembly bindings for SQLRite (Phase 5g).
//!
//! Compiles the Rust engine straight to `wasm32-unknown-unknown` and
//! exposes a tiny `Database` class to JavaScript via `wasm-bindgen`.
//! The engine runs entirely in the browser tab — no server, no file
//! I/O.
//!
//! ```js
//! import init, { Database } from '@joaoh82/sqlrite-wasm';
//! await init();
//!
//! const db = new Database();
//! db.exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)");
//! db.exec("INSERT INTO users (name) VALUES ('alice')");
//!
//! const rows = db.query("SELECT id, name FROM users");
//! // → [{ id: 1, name: 'alice' }]
//! ```
//!
//! ## Scope of the MVP
//!
//! - **In-memory only.** `Connection::open(path)` doesn't have a
//!   reasonable browser semantic — the OS file locks and `-wal`
//!   sidecar that file-backed mode needs don't exist in a tab's
//!   sandbox. We only expose `Connection::open_in_memory()`.
//!   OPFS-backed persistence is a natural follow-up but out of
//!   scope here.
//! - **No prepared statements at the JS boundary.** `db.query(sql)`
//!   is the one-shot shape — no `db.prepare` + `.step()` loop. The
//!   engine still does the prepare/execute split internally; we
//!   just don't expose it to JS because the added objects +
//!   lifetimes complicate the bindings without much payoff for the
//!   in-memory MVP.
//! - **Parameter binding** follows the same "not yet, 5a.2 will
//!   add it" story as every other SDK.
//!
//! ## Why this binds Rust directly instead of going through the C FFI
//!
//! Because it can. The engine is pure Rust; wasm-bindgen gives us a
//! direct Rust↔JS boundary in wasm32. No C round-trip, no cgo-shape
//! complications.

use serde::Serialize;
use wasm_bindgen::prelude::*;

use sqlrite::{Connection, Value};

// Phase 7g.7 — schema dump + prompt construction reused from the
// engine and `sqlrite-ask` so we don't drift from the canonical
// rules block. Per Q9 the WASM SDK never makes the HTTP call
// itself; the browser hands the prompt to the caller's JS function
// and parses the response back.
use sqlrite::ask::schema::dump_schema_for_database;
use sqlrite_ask::prompt::{CacheControl, build_system};
use sqlrite_ask::{Usage, parse_response};

// ---------------------------------------------------------------------------
// Setup

/// Runs once when the WASM module is first imported. Wires up
/// `console.error`-backed panic reporting so a Rust panic shows a
/// real stack trace in devtools instead of a generic "unreachable"
/// trap.
#[wasm_bindgen(start)]
pub fn _init() {
    #[cfg(feature = "panic-hook")]
    console_error_panic_hook::set_once();
}

// ---------------------------------------------------------------------------
// Database
//
// Rows are marshalled to JS as plain objects keyed by column name,
// matching the Node.js SDK so WASM callers don't have to learn a
// different shape. We build `serde_json::Map` on the Rust side (it
// preserves insertion order, so projection order survives the
// cross-boundary hop) and hand it to `serde-wasm-bindgen` to
// produce JS objects.

/// A SQLRite database handle. Always in-memory in the WASM build.
/// Drop the handle (set to `null` / let GC collect it) to free the
/// underlying state.
#[wasm_bindgen]
pub struct Database {
    inner: Connection,
}

#[wasm_bindgen]
impl Database {
    /// Creates an in-memory database. The only mode supported by
    /// the WASM build — file-backed mode isn't meaningful in a
    /// browser sandbox.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<Database, JsValue> {
        Connection::open_in_memory()
            .map(|c| Database { inner: c })
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Runs one SQL statement that doesn't produce rows (CREATE /
    /// INSERT / UPDATE / DELETE / BEGIN / COMMIT / ROLLBACK). For
    /// SELECT use [`query`].
    pub fn exec(&mut self, sql: &str) -> Result<(), JsValue> {
        self.inner
            .execute(sql)
            .map(|_| ())
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Runs a SELECT and returns an array of row objects. Each
    /// object's keys are column names in projection order; values
    /// are typed JS primitives — `number` for Integer/Real,
    /// `string` for Text, `boolean` for Bool, `null` for NULL.
    pub fn query(&mut self, sql: &str) -> Result<JsValue, JsValue> {
        let mut stmt = self
            .inner
            .prepare(sql)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let mut rows_iter = stmt
            .query()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        let columns: Vec<String> = rows_iter.columns().to_vec();
        let mut out: Vec<serde_json::Map<String, serde_json::Value>> = Vec::new();
        while let Some(row) = rows_iter
            .next()
            .map_err(|e| JsValue::from_str(&e.to_string()))?
        {
            let owned = row.to_owned_row();
            let mut obj = serde_json::Map::with_capacity(columns.len());
            for (i, col) in columns.iter().enumerate() {
                let v = owned.values.get(i).cloned().unwrap_or(Value::Null);
                obj.insert(col.clone(), value_to_json(&v));
            }
            out.push(obj);
        }

        // `serde-wasm-bindgen`'s default serializer hands `Map`s
        // across as JS `Map` objects, which means `Object.keys(row)`
        // returns nothing on the JS side. Flipping
        // `serialize_maps_as_objects(true)` makes each row a plain
        // `Object` — what JS callers actually expect.
        let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
        out.serialize(&serializer)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Number of columns in the projection of a SELECT. Useful when
    /// a caller wants to build their own UI column list without
    /// iterating rows.
    pub fn columns(&mut self, sql: &str) -> Result<JsValue, JsValue> {
        let mut stmt = self
            .inner
            .prepare(sql)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let rows_iter = stmt
            .query()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let cols: Vec<String> = rows_iter.columns().to_vec();
        serde_wasm_bindgen::to_value(&cols).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Returns `true` while a `BEGIN … COMMIT/ROLLBACK` block is open.
    #[wasm_bindgen(getter, js_name = inTransaction)]
    pub fn in_transaction(&self) -> bool {
        self.inner.in_transaction()
    }

    /// Always `false` in the WASM build (file-backed / read-only
    /// opens aren't exposed). Kept for API-shape parity with the
    /// Node.js SDK.
    #[wasm_bindgen(getter)]
    pub fn readonly(&self) -> bool {
        false
    }

    // -----------------------------------------------------------------
    // Phase 7g.7 — natural-language → SQL.
    //
    // **Different shape than the other SDKs** (per Q9). Reasoning:
    //
    //   * **CORS.** Browsers block direct cross-origin POSTs from a
    //     WASM module to api.anthropic.com / api.openai.com unless
    //     the LLM provider serves CORS headers. They don't, by
    //     design — they don't want users embedding API keys in
    //     client-side JS.
    //   * **API-key exposure.** Even if CORS were OK, putting the
    //     API key into a WASM-loaded page exposes it to anyone with
    //     devtools.
    //   * **Both problems disappear server-side.** Node, Python, Go,
    //     Desktop (Tauri runs the call in the Rust backend, not the
    //     webview) all do the HTTP from a trusted process.
    //
    // Solution: split the work. WASM does the schema-aware prompt
    // construction in-page (it has the schema, it has the rules
    // block); the caller's JS code hands the resulting payload to
    // their own backend, which forwards it to the LLM provider with
    // their API key, then hands the model's raw response back into
    // `db.askParse()`. The browser tab never sees the key, never
    // POSTs to a third-party LLM endpoint, and never deals with CORS.
    //
    // ## Public surface
    //
    //   * `db.askPrompt(question, options?)` — returns the request
    //     body the caller should POST to the LLM provider. JSON
    //     shape matches Anthropic's `/v1/messages` body so callers
    //     can forward it as-is to Anthropic; OpenAI / Ollama users
    //     translate on their backend side.
    //   * `db.askParse(rawApiResponse)` — parses Anthropic's
    //     response back into `{ sql, explanation, usage }`. Tolerant
    //     of fenced JSON / leading prose in the model's text content
    //     (same parser as every other SDK uses).

    /// Build the LLM-provider request payload for `question` against
    /// the current schema. Returns a JS object ready to POST to the
    /// caller's backend.
    ///
    /// ```js
    /// const payload = db.askPrompt('How many users?');
    /// // → { model, max_tokens, system: [...], messages: [...] }
    /// const response = await fetch('/api/llm/complete', {
    ///   method: 'POST',
    ///   body: JSON.stringify(payload),
    /// });
    /// const apiResponse = await response.json();
    /// const result = db.askParse(JSON.stringify(apiResponse));
    /// // → { sql, explanation, usage: {...} }
    /// ```
    ///
    /// `options` (optional) accepts:
    ///   * `model` — override the default `claude-sonnet-4-6`.
    ///   * `maxTokens` — override the default `1024`.
    ///   * `cacheTtl` — `"5m"` (default), `"1h"`, or `"off"`.
    #[wasm_bindgen(js_name = askPrompt)]
    pub fn ask_prompt(
        &self,
        question: &str,
        options: Option<AskPromptOptions>,
    ) -> Result<JsValue, JsValue> {
        let opts = options.unwrap_or_default();
        let model = opts
            .model
            .unwrap_or_else(|| "claude-sonnet-4-6".to_string());
        let max_tokens = opts.max_tokens.unwrap_or(1024);
        let cache_ttl = opts.cache_ttl.as_deref().unwrap_or("5m");

        let cache_marker = match cache_ttl.to_ascii_lowercase().as_str() {
            "5m" | "5min" | "5minutes" => Some(CacheControl::ephemeral()),
            "1h" | "1hr" | "1hour" => Some(CacheControl::ephemeral_1h()),
            "off" | "none" | "disabled" => None,
            other => {
                return Err(JsValue::from_str(&format!(
                    "unknown cacheTtl: {other} (expected 5m, 1h, or off)"
                )));
            }
        };

        let schema = dump_schema_for_database(self.inner.database());
        let system_blocks = build_system(&schema, cache_marker);
        let messages = vec![PromptUserMessage {
            role: "user",
            content: question.to_string(),
        }];

        let payload = AskPromptPayload {
            model,
            max_tokens,
            system: system_blocks
                .iter()
                .map(|b| PromptSystemBlock {
                    kind: b.kind,
                    text: b.text.clone(),
                    cache_control: b.cache_control.as_ref().map(|c| PromptCacheControl {
                        kind: c.kind,
                        ttl: c.ttl,
                    }),
                })
                .collect(),
            messages,
        };

        let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
        payload
            .serialize(&serializer)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Parse an Anthropic API response (the full JSON the JS caller's
    /// fetch returned) back into `{ sql, explanation, usage }`.
    ///
    /// Pass the raw response body as a string. The parser:
    ///   * Extracts the first text content block from `content[]`.
    ///   * Reads token counts from `usage`.
    ///   * Parses the model's text as JSON (tolerant to fenced /
    ///     leading-prose shapes — see `sqlrite_ask::parse_response`).
    ///
    /// On parse failure (the model emitted unparseable text, or the
    /// API response was malformed), throws a JS Error with the
    /// underlying reason.
    #[wasm_bindgen(js_name = askParse)]
    pub fn ask_parse(&self, raw_api_response: &str) -> Result<JsValue, JsValue> {
        // Extract the model's text + usage from the API response
        // shape (Anthropic format). Other providers' responses can
        // be massaged into this shape on the JS side before calling
        // askParse.
        let parsed: serde_json::Value = serde_json::from_str(raw_api_response)
            .map_err(|e| JsValue::from_str(&format!("api response not JSON: {e}")))?;

        let text = parsed
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| {
                arr.iter().find_map(|block| {
                    if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                        block.get("text").and_then(|t| t.as_str()).map(String::from)
                    } else {
                        None
                    }
                })
            })
            .ok_or_else(|| {
                JsValue::from_str("api response missing content[].text — was this an Anthropic /v1/messages response?")
            })?;

        let usage = parsed
            .get("usage")
            .and_then(|u| {
                Some(Usage {
                    input_tokens: u.get("input_tokens")?.as_u64().unwrap_or(0),
                    output_tokens: u.get("output_tokens")?.as_u64().unwrap_or(0),
                    cache_creation_input_tokens: u
                        .get("cache_creation_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    cache_read_input_tokens: u
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                })
            })
            .unwrap_or_default();

        let resp = parse_response(&text, usage)
            .map_err(|e| JsValue::from_str(&format!("parse model output: {e}")))?;

        // Hand the result back as a plain JS object.
        let out = AskParseResult {
            sql: resp.sql,
            explanation: resp.explanation,
            usage: AskUsageJs {
                input_tokens: resp.usage.input_tokens,
                output_tokens: resp.usage.output_tokens,
                cache_creation_input_tokens: resp.usage.cache_creation_input_tokens,
                cache_read_input_tokens: resp.usage.cache_read_input_tokens,
            },
        };
        let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
        out.serialize(&serializer)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// askPrompt request shape (mirrors Anthropic /v1/messages body)

#[derive(Default)]
#[wasm_bindgen]
pub struct AskPromptOptions {
    /// Model ID (default: `"claude-sonnet-4-6"`).
    #[wasm_bindgen(getter_with_clone)]
    pub model: Option<String>,
    /// `max_tokens` for the LLM call (default: 1024).
    pub max_tokens: Option<u32>,
    /// Anthropic prompt-cache TTL on the schema block: `"5m"`
    /// (default), `"1h"`, or `"off"`.
    #[wasm_bindgen(getter_with_clone)]
    pub cache_ttl: Option<String>,
}

#[wasm_bindgen]
impl AskPromptOptions {
    /// Construct an empty options object. JS can mutate fields in
    /// place: `const opts = new AskPromptOptions(); opts.model = '...'`.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Serialize)]
struct AskPromptPayload {
    model: String,
    max_tokens: u32,
    system: Vec<PromptSystemBlock>,
    messages: Vec<PromptUserMessage>,
}

#[derive(Serialize)]
struct PromptSystemBlock {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<PromptCacheControl>,
}

#[derive(Serialize)]
struct PromptCacheControl {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl: Option<&'static str>,
}

#[derive(Serialize)]
struct PromptUserMessage {
    role: &'static str,
    content: String,
}

// ---------------------------------------------------------------------------
// askParse result shape

#[derive(Serialize)]
struct AskParseResult {
    sql: String,
    explanation: String,
    usage: AskUsageJs,
}

#[derive(Serialize)]
struct AskUsageJs {
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Integer(n) => serde_json::Value::Number((*n).into()),
        Value::Real(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Text(s) => serde_json::Value::String(s.clone()),
        // Phase 7a — `VECTOR(N)` columns surface to JS as `Array<number>`.
        // Widening f32→f64 (JS Number is f64-backed; serde_json::Number
        // requires finite f64). NaN / Inf elements collapse to null
        // entries — same fallback the Real arm already uses.
        Value::Vector(elements) => serde_json::Value::Array(
            elements
                .iter()
                .map(|x| {
                    serde_json::Number::from_f64(*x as f64)
                        .map(serde_json::Value::Number)
                        .unwrap_or(serde_json::Value::Null)
                })
                .collect(),
        ),
    }
}
