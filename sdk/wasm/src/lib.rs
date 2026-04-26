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

