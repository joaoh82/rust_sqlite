//! Node.js bindings for SQLRite (Phase 5d).
//!
//! Shipped as the `@joaoh82/sqlrite` npm package — scoped because
//! the unscoped `sqlrite` name was rejected by npm's similarity
//! check against `sqlite` / `sqlite3`. Shape inspired by
//! [`better-sqlite3`](https://github.com/WiseLibs/better-sqlite3)
//! (sync API, row-as-object), so JavaScript callers familiar with
//! that library can pick this up immediately:
//!
//! ```js
//! import { Database } from '@joaoh82/sqlrite';
//!
//! const db = new Database('foo.sqlrite');
//! db.exec('CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)');
//! db.prepare("INSERT INTO users (name) VALUES ('alice')").run();
//!
//! for (const row of db.prepare('SELECT id, name FROM users').iterate()) {
//!   console.log(row); // { id: 1, name: 'alice' }
//! }
//!
//! db.close();
//! ```
//!
//! ## Implementation
//!
//! - Wraps the Rust `sqlrite::Connection` directly. Like the Python
//!   binding, we skip the C FFI hop — napi-rs hands us typed JS
//!   values directly.
//! - Sync API, not async — the engine is in-process and most
//!   operations finish in microseconds. Promises would add overhead
//!   and make the API heavier.
//! - Rows come back as plain JS objects keyed by column name, which
//!   matches what Node devs expect from `better-sqlite3`.
//! - Errors surface as JS `Error` instances; the message matches the
//!   Rust `SQLRiteError` Display output.
//! - Parameter binding is deferred until Phase 5a.2 lands real
//!   binding in the engine. The wrapper accepts the positional-args
//!   shape for forward compat but throws on non-empty args.

use std::cell::RefCell;
use std::path::PathBuf;

use napi::bindgen_prelude::*;
use napi::{Env, JsObject, JsUnknown};
use napi_derive::napi;

use sqlrite::ask::{AskConfig as RustAskConfig, CacheTtl, ProviderKind, ask_with_database};
use sqlrite::{Connection as RustConnection, OwnedRow, Rows, Value};

// ---------------------------------------------------------------------------
// Helpers

fn map_err<E: std::fmt::Display>(e: E) -> napi::Error {
    napi::Error::from_reason(e.to_string())
}

/// Converts a `sqlrite::Value` into a napi-compatible JS value using the
/// env to allocate. Used both for row values and for error contexts.
fn value_to_js(env: &Env, v: &Value) -> Result<JsUnknown> {
    match v {
        Value::Integer(n) => Ok(env.create_int64(*n)?.into_unknown()),
        Value::Real(f) => Ok(env.create_double(*f)?.into_unknown()),
        Value::Text(s) => Ok(env.create_string(s)?.into_unknown()),
        Value::Bool(b) => Ok(env.get_boolean(*b)?.into_unknown()),
        // Phase 7a — `VECTOR(N)` columns surface to JS as `Array<number>`.
        // Widening f32→f64 since JS Number is f64-backed; no precision lost.
        // Future polish: optionally hand back a Float32Array (typed array)
        // for memory-efficient transfer of high-dim vectors.
        Value::Vector(elements) => {
            let mut arr = env.create_array_with_length(elements.len())?;
            for (i, x) in elements.iter().enumerate() {
                arr.set_element(i as u32, env.create_double(*x as f64)?)?;
            }
            Ok(arr.into_unknown())
        }
        Value::Null => Ok(env.get_null()?.into_unknown()),
    }
}

fn row_to_js_object(env: &Env, columns: &[String], row: &OwnedRow) -> Result<JsObject> {
    let mut obj = env.create_object()?;
    for (i, col) in columns.iter().enumerate() {
        let v = row.values.get(i).cloned().unwrap_or(Value::Null);
        let js = value_to_js(env, &v)?;
        obj.set_named_property(col, js)?;
    }
    Ok(obj)
}

/// Throws on any non-empty positional-args value. Placeholder until
/// Phase 5a.2 lands real parameter binding across the stack.
///
/// napi-rs auto-coerces `undefined` and `null` on the JS side to
/// `None` in Rust, and arrays land here as `Some(Vec<_>)`. Anything
/// else that isn't an array (a plain object, a string, etc.) never
/// makes it past napi's type check, so we only have to handle the
/// three cases.
fn reject_params_for_now(params: &Option<Vec<JsUnknown>>) -> Result<()> {
    match params {
        None => Ok(()),
        Some(v) if v.is_empty() => Ok(()),
        Some(_) => Err(napi::Error::from_reason(
            "parameter binding is not yet supported — inline values into the SQL \
             (a future Phase 5a.2 release will add real binding)",
        )),
    }
}

// ---------------------------------------------------------------------------
// Database
//
// Wraps `RustConnection` + a detach-from-borrow-via-OwnedRow Rows
// handle stored per-Statement, mirroring the Python SDK's shape.

#[napi]
pub struct Database {
    // RefCell because napi #[napi] methods receive `&mut self` but
    // inner shared state across Statement children reads the same
    // connection — the engine is single-threaded, so a RefCell is
    // sufficient. For cross-thread sharing Node users would call
    // `worker_threads`, which gives each worker its own `.node`
    // import + its own Database instance.
    inner: RefCell<Option<RustConnection>>,
    // Phase 7g.5 — per-connection ask() config. Set via
    // `setAskConfig()` or passed per-call to `ask()` / `askRun()`.
    // When None, `ask()` falls back to `AskConfig.fromEnv()` so
    // env-only consumers get the zero-config experience matching the
    // REPL, Desktop, and Python SDK surfaces.
    ask_config: RefCell<Option<RustAskConfig>>,
}

#[napi]
impl Database {
    /// Opens (or creates) a database file. Pass `":memory:"` for an
    /// in-memory DB (matching better-sqlite3 convention).
    #[napi(constructor)]
    pub fn new(database: String) -> Result<Self> {
        let conn = if database == ":memory:" {
            RustConnection::open_in_memory().map_err(map_err)?
        } else {
            RustConnection::open(PathBuf::from(database)).map_err(map_err)?
        };
        Ok(Self {
            inner: RefCell::new(Some(conn)),
            ask_config: RefCell::new(None),
        })
    }

    /// Opens an existing file read-only — shared OS lock, multi-reader
    /// safe, any write throws.
    #[napi(factory)]
    pub fn open_read_only(database: String) -> Result<Self> {
        let conn = RustConnection::open_read_only(PathBuf::from(database)).map_err(map_err)?;
        Ok(Self {
            inner: RefCell::new(Some(conn)),
            ask_config: RefCell::new(None),
        })
    }

    /// Runs one or more SQL statements. Use for DDL / DML /
    /// transactions — there's no return value, just a throw on error.
    #[napi]
    pub fn exec(&self, sql: String) -> Result<()> {
        let mut borrow = self.inner.borrow_mut();
        let conn = borrow
            .as_mut()
            .ok_or_else(|| napi::Error::from_reason("cannot exec: database is closed"))?;
        conn.execute(&sql).map_err(map_err)?;
        Ok(())
    }

    /// Prepares a SQL statement. Returned `Statement` runs in the
    /// context of this Database — once the Database is closed, its
    /// Statements throw on any operation.
    #[napi]
    pub fn prepare(&self, sql: String) -> Result<Statement> {
        // We verify the SQL parses at prepare time so syntax errors
        // surface early, matching better-sqlite3's behavior.
        let mut borrow = self.inner.borrow_mut();
        let conn = borrow
            .as_mut()
            .ok_or_else(|| napi::Error::from_reason("cannot prepare: database is closed"))?;
        let _ = conn.prepare(&sql).map_err(map_err)?;
        Ok(Statement {
            db_raw: self as *const Database,
            sql,
        })
    }

    /// Closes the connection and releases the OS file lock. Safe to
    /// call multiple times.
    #[napi]
    pub fn close(&self) -> Result<()> {
        *self.inner.borrow_mut() = None;
        Ok(())
    }

    #[napi(getter)]
    pub fn in_transaction(&self) -> Result<bool> {
        let borrow = self.inner.borrow();
        let conn = borrow
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("database is closed"))?;
        Ok(conn.in_transaction())
    }

    #[napi(getter)]
    pub fn readonly(&self) -> Result<bool> {
        let borrow = self.inner.borrow();
        let conn = borrow
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("database is closed"))?;
        Ok(conn.is_read_only())
    }

    // -----------------------------------------------------------------
    // Phase 7g.5 — natural-language → SQL.
    //
    // Three entry points, mirroring the Python SDK shape:
    //   * `setAskConfig(cfg)` stores a config on the DB so subsequent
    //     `ask()` calls reuse it without reconfiguring. Pass `null` to
    //     clear and fall back to env/defaults.
    //   * `ask(question, config?)` generates SQL — does NOT execute.
    //     Returns an `AskResponse` with `.sql` / `.explanation` /
    //     `.usage`.
    //   * `askRun(question, config?)` is the convenience that calls
    //     `ask()` then `prepare(resp.sql).all()` — returns the result
    //     rows directly. Empty SQL response (model declined) throws
    //     with the model's explanation rather than executing the
    //     empty string.
    //
    // Config resolution (when `config` arg omitted / null):
    //   1. Per-connection config from setAskConfig() if set.
    //   2. AskConfig.fromEnv() — reads SQLRITE_LLM_API_KEY etc.
    //   3. Built-in defaults (Sonnet 4.6, max_tokens 1024, 5-min cache).
    //
    // GIL note: napi-rs methods run synchronously on Node's main
    // event loop. The HTTP call inside ask_with_database() uses
    // ureq's blocking POST — Node's event loop is busy for the
    // round-trip duration (~hundreds of ms typical, capped at 90s
    // by ureq). A pure-Node HTTP mock listening on the same event
    // loop would deadlock (matches the Python GIL constraint we
    // hit in 7g.4); the test suite spins the mock in a
    // worker_thread to bypass this.

    /// Stash an `AskConfig` on the database. Subsequent `ask()` and
    /// `askRun()` calls without an explicit config use this.
    #[napi]
    pub fn set_ask_config(&self, config: Option<&AskConfig>) {
        *self.ask_config.borrow_mut() = config.map(|c| c.inner.clone());
    }

    /// Generate SQL from a natural-language question. Does **not**
    /// execute — call `db.prepare(resp.sql).all()` (or use `askRun()`
    /// for one-shot). Returns an `AskResponse` carrying `.sql`,
    /// `.explanation`, and `.usage`.
    #[napi]
    pub fn ask(&self, question: String, config: Option<&AskConfig>) -> Result<AskResponse> {
        let resolved = self.resolve_ask_config(config)?;
        let borrow = self.inner.borrow();
        let conn = borrow
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("cannot ask: database is closed"))?;
        let resp = ask_with_database(conn.database(), &question, &resolved).map_err(map_err)?;
        Ok(AskResponse {
            sql: resp.sql,
            explanation: resp.explanation,
            usage: AskUsage {
                input_tokens: resp.usage.input_tokens as i64,
                output_tokens: resp.usage.output_tokens as i64,
                cache_creation_input_tokens: resp.usage.cache_creation_input_tokens as i64,
                cache_read_input_tokens: resp.usage.cache_read_input_tokens as i64,
            },
        })
    }

    /// Generate SQL **and execute it as a SELECT**. Returns rows as
    /// `Array<Object>` (same shape as `prepare(sql).all()`). Errors
    /// the same way `ask()` does on generation failure, and the same
    /// way `prepare().all()` does on bad-SQL execution failure.
    ///
    /// **Throws on empty SQL.** When the model declines to generate
    /// SQL (returns an empty `sql` string with an explanation), this
    /// throws rather than executing the empty string — the
    /// explanation is in the error message.
    ///
    /// Convenience for one-shot scripts. For interactive use, prefer
    /// `ask()` + manual review (the model can be wrong; auto-execute
    /// hides that).
    #[napi]
    pub fn ask_run(
        &self,
        env: Env,
        question: String,
        config: Option<&AskConfig>,
    ) -> Result<Vec<JsUnknown>> {
        let resp = self.ask(question, config)?;
        let trimmed = resp.sql.trim();
        if trimmed.is_empty() {
            return Err(napi::Error::from_reason(format!(
                "model declined to generate SQL: {}",
                if resp.explanation.is_empty() {
                    "(no explanation)"
                } else {
                    resp.explanation.as_str()
                }
            )));
        }
        // Re-borrow for the execution. This is intentionally a fresh
        // borrow — the borrow guard from `ask()` already released
        // before we got here.
        let mut borrow = self.inner.borrow_mut();
        let conn = borrow
            .as_mut()
            .ok_or_else(|| napi::Error::from_reason("cannot askRun: database is closed"))?;
        let stmt = conn.prepare(trimmed).map_err(map_err)?;
        let mut rows: Rows = stmt.query().map_err(map_err)?;
        let columns = rows.columns().to_vec();
        let mut out: Vec<JsUnknown> = Vec::new();
        while let Some(row) = rows.next().map_err(map_err)? {
            let owned = row.to_owned_row();
            out.push(row_to_js_object(&env, &columns, &owned)?.into_unknown());
        }
        Ok(out)
    }
}

// Free helper hung off Database (not in the #[napi] block) so it
// stays implementation-private — JS can't call it directly.
impl Database {
    fn resolve_ask_config(&self, per_call: Option<&AskConfig>) -> Result<RustAskConfig> {
        if let Some(cfg) = per_call {
            return Ok(cfg.inner.clone());
        }
        if let Some(cfg) = self.ask_config.borrow().as_ref() {
            return Ok(cfg.clone());
        }
        RustAskConfig::from_env().map_err(map_err)
    }
}

// ---------------------------------------------------------------------------
// AskConfig (Phase 7g.5)
//
// Constructed from a JS option object (idiomatic Node) instead of
// kwargs. Same field names as the Python SDK but camelCase per JS
// convention (apiKey vs api_key, maxTokens vs max_tokens, etc.).
//
// Three precedence layers when calling `db.ask(q, cfg?)`:
//   1. per-call cfg   (highest)
//   2. setAskConfig() stored on db
//   3. AskConfig.fromEnv() — SQLRITE_LLM_* env vars
//   4. AskConfig() defaults — anthropic / claude-sonnet-4-6 / 1024 / 5m

/// Options accepted by the AskConfig constructor.
///
/// All fields are optional; unset fields take the same defaults as
/// the Rust side (provider=anthropic, model=`claude-sonnet-4-6`,
/// maxTokens=1024, cacheTtl="5m").
#[napi(object)]
pub struct AskConfigOptions {
    /// `"anthropic"` (only currently supported).
    pub provider: Option<String>,
    /// API key for the LLM provider. Read from SQLRITE_LLM_API_KEY by
    /// `AskConfig.fromEnv()`. Treat as a secret — `AskConfig.toString()`
    /// deliberately omits the key value.
    pub api_key: Option<String>,
    /// Model ID (e.g. `"claude-sonnet-4-6"`, `"claude-haiku-4-5"`).
    pub model: Option<String>,
    /// Per-call max output tokens. Default 1024.
    pub max_tokens: Option<u32>,
    /// Anthropic prompt-cache TTL: `"5m"` (default), `"1h"`, or `"off"`.
    pub cache_ttl: Option<String>,
    /// Override the API base URL — production callers leave undefined;
    /// tests point it at a localhost mock.
    pub base_url: Option<String>,
}

/// Configuration for `db.ask()` / `db.askRun()` calls.
///
/// ```js
/// const cfg = new AskConfig({
///   apiKey: 'sk-ant-...',
///   model: 'claude-haiku-4-5',
///   cacheTtl: '1h',
/// });
/// db.setAskConfig(cfg);
/// const resp = db.ask('How many users?');
/// ```
///
/// Or build from env (SQLRITE_LLM_API_KEY etc.):
///
/// ```js
/// const cfg = AskConfig.fromEnv();
/// ```
#[napi]
pub struct AskConfig {
    inner: RustAskConfig,
}

#[napi]
impl AskConfig {
    /// Build from an options object. Any field left undefined uses
    /// the matching default.
    #[napi(constructor)]
    pub fn new(options: Option<AskConfigOptions>) -> Result<Self> {
        let mut inner = RustAskConfig::default();
        let Some(opts) = options else {
            return Ok(AskConfig { inner });
        };
        if let Some(p) = opts.provider {
            inner.provider = match p.to_ascii_lowercase().as_str() {
                "anthropic" => ProviderKind::Anthropic,
                other => {
                    return Err(napi::Error::from_reason(format!(
                        "unknown provider: {other} (supported: anthropic)"
                    )));
                }
            };
        }
        if let Some(k) = opts.api_key {
            if !k.is_empty() {
                inner.api_key = Some(k);
            }
        }
        if let Some(m) = opts.model {
            if !m.is_empty() {
                inner.model = m;
            }
        }
        if let Some(t) = opts.max_tokens {
            inner.max_tokens = t;
        }
        if let Some(c) = opts.cache_ttl {
            inner.cache_ttl = match c.to_ascii_lowercase().as_str() {
                "5m" | "5min" | "5minutes" => CacheTtl::FiveMinutes,
                "1h" | "1hr" | "1hour" => CacheTtl::OneHour,
                "off" | "none" | "disabled" => CacheTtl::Off,
                other => {
                    return Err(napi::Error::from_reason(format!(
                        "unknown cacheTtl: {other} (expected 5m, 1h, or off)"
                    )));
                }
            };
        }
        if let Some(u) = opts.base_url {
            if !u.is_empty() {
                inner.base_url = Some(u);
            }
        }
        Ok(AskConfig { inner })
    }

    /// Build from environment variables. Reads:
    ///   * SQLRITE_LLM_PROVIDER (default: anthropic)
    ///   * SQLRITE_LLM_API_KEY
    ///   * SQLRITE_LLM_MODEL (default: claude-sonnet-4-6)
    ///   * SQLRITE_LLM_MAX_TOKENS (default: 1024)
    ///   * SQLRITE_LLM_CACHE_TTL (default: 5m)
    ///
    /// A missing API key is NOT an error here — `db.ask()` raises the
    /// friendlier "missing API key" message later.
    #[napi(factory)]
    pub fn from_env() -> Result<Self> {
        Ok(AskConfig {
            inner: RustAskConfig::from_env().map_err(map_err)?,
        })
    }

    /// `true` when an API key has been set (either explicitly or via
    /// env). Doesn't expose the key value.
    #[napi(getter)]
    pub fn has_api_key(&self) -> bool {
        self.inner.api_key.is_some()
    }

    #[napi(getter)]
    pub fn model(&self) -> String {
        self.inner.model.clone()
    }

    #[napi(getter)]
    pub fn max_tokens(&self) -> u32 {
        self.inner.max_tokens
    }

    #[napi(getter)]
    pub fn cache_ttl(&self) -> &'static str {
        match self.inner.cache_ttl {
            CacheTtl::FiveMinutes => "5m",
            CacheTtl::OneHour => "1h",
            CacheTtl::Off => "off",
        }
    }

    #[napi(getter)]
    pub fn provider(&self) -> &'static str {
        match self.inner.provider {
            ProviderKind::Anthropic => "anthropic",
        }
    }

    /// String form. **Deliberately does not include the API key
    /// value** — printing the config in a log line / debugger /
    /// console.log won't leak the secret. Shows `apiKey=<set>` or
    /// `apiKey=null` so callers can tell whether a key is configured.
    #[napi]
    pub fn to_string(&self) -> String {
        format!(
            "AskConfig(provider={:?}, model={:?}, maxTokens={}, cacheTtl={:?}, apiKey={})",
            self.provider(),
            self.model(),
            self.max_tokens(),
            self.cache_ttl(),
            if self.inner.api_key.is_some() {
                "<set>"
            } else {
                "null"
            },
        )
    }
}

// ---------------------------------------------------------------------------
// AskResponse (Phase 7g.5)

/// Returned by `db.ask()`. Carries the generated SQL, the model's
/// one-sentence rationale, and token usage. The API key is **not**
/// in here — by design.
#[napi(object)]
pub struct AskResponse {
    pub sql: String,
    pub explanation: String,
    pub usage: AskUsage,
}

/// Token usage breakdown from an `ask()` call. Inspect to verify
/// prompt-caching is actually working — if `cacheReadInputTokens`
/// stays zero across repeated calls with the same schema, something
/// in the prefix is invalidating the cache.
#[napi(object)]
pub struct AskUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_read_input_tokens: i64,
}

// ---------------------------------------------------------------------------
// Statement
//
// Unlike better-sqlite3, our Statement does NOT own a compiled plan
// (the engine doesn't cache plans yet). It stores the SQL and the
// parent Database pointer; each run()/get()/all()/iterate() call
// re-prepares and executes. That's fine for the Phase 5d MVP and
// will get cheaper once 5a.2 lands prepared-statement caching.

#[napi]
pub struct Statement {
    /// Raw pointer to the parent `Database`. napi-rs handles lifetime
    /// management across JS/Rust via its own ObjectRef system; we
    /// don't hand it a Rust reference because Statement isn't a
    /// `#[napi(constructor)]` entry point — it's returned from
    /// `prepare()` and its lifetime is tied to the JS-side
    /// reachability of the Database object that created it.
    db_raw: *const Database,
    sql: String,
}

// Both fields are trivially Send; the RefCell inside Database
// prevents concurrent access on the Rust side.
unsafe impl Send for Statement {}

impl Statement {
    fn with_db<F, T>(&self, op: &str, f: F) -> Result<T>
    where
        F: FnOnce(&Database) -> Result<T>,
    {
        // Safety: Statement's JS wrapper keeps a reference to the
        // parent Database object, so `db_raw` stays valid as long
        // as the Statement handle exists on the JS side.
        let db = unsafe { self.db_raw.as_ref() }.ok_or_else(|| {
            napi::Error::from_reason(format!("cannot {op}: parent database dropped"))
        })?;
        f(db)
    }

    fn run_query(&self, env: &Env) -> Result<(Vec<String>, Vec<OwnedRow>)> {
        self.with_db("query", |db| {
            let mut borrow = db.inner.borrow_mut();
            let conn = borrow
                .as_mut()
                .ok_or_else(|| napi::Error::from_reason("cannot query: database is closed"))?;
            let stmt = conn.prepare(&self.sql).map_err(map_err)?;
            let mut rows: Rows = stmt.query().map_err(map_err)?;
            let columns = rows.columns().to_vec();
            let mut out: Vec<OwnedRow> = Vec::new();
            while let Some(row) = rows.next().map_err(map_err)? {
                out.push(row.to_owned_row());
            }
            let _ = env; // env used by caller for row_to_js_object
            Ok((columns, out))
        })
    }
}

#[napi]
impl Statement {
    /// Executes a non-query statement (INSERT / UPDATE / DELETE / etc.)
    /// `params` must be `undefined`, `null`, or an empty array until
    /// Phase 5a.2 lands parameter binding — anything else throws.
    #[napi]
    pub fn run(&self, params: Option<Vec<JsUnknown>>) -> Result<RunResult> {
        reject_params_for_now(&params)?;
        self.with_db("run", |db| {
            let mut borrow = db.inner.borrow_mut();
            let conn = borrow
                .as_mut()
                .ok_or_else(|| napi::Error::from_reason("cannot run: database is closed"))?;
            conn.execute(&self.sql).map_err(map_err)?;
            Ok(RunResult {
                // `changes` and `lastInsertRowid` aren't tracked by
                // the engine yet; better-sqlite3 returns them here,
                // so we mirror the shape with zeros.
                changes: 0,
                last_insert_rowid: 0,
            })
        })
    }

    /// Runs a SELECT and returns the first row as an object (or null
    /// if empty).
    #[napi]
    pub fn get(&self, env: Env, params: Option<Vec<JsUnknown>>) -> Result<JsUnknown> {
        reject_params_for_now(&params)?;
        let (columns, mut rows) = self.run_query(&env)?;
        if rows.is_empty() {
            return Ok(env.get_null()?.into_unknown());
        }
        let first = rows.remove(0);
        Ok(row_to_js_object(&env, &columns, &first)?.into_unknown())
    }

    /// Runs a SELECT and returns every row as an array of objects.
    #[napi]
    pub fn all(&self, env: Env, params: Option<Vec<JsUnknown>>) -> Result<Vec<JsUnknown>> {
        reject_params_for_now(&params)?;
        let (columns, rows) = self.run_query(&env)?;
        let mut out: Vec<JsUnknown> = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(row_to_js_object(&env, &columns, row)?.into_unknown());
        }
        Ok(out)
    }

    /// Eager iterator — returns an array (better-sqlite3 uses a real
    /// JS iterator for memory efficiency; the Phase 5a.2 cursor work
    /// will let us do the same. For now, `iterate()` behaves like
    /// `all()` so callers write `for (const row of stmt.iterate())`
    /// ergonomically).
    #[napi]
    pub fn iterate(&self, env: Env, params: Option<Vec<JsUnknown>>) -> Result<Vec<JsUnknown>> {
        self.all(env, params)
    }

    /// Column names the statement will produce, in projection order.
    /// Runs the query once to discover them (the engine doesn't yet
    /// have a plan-inspection API separate from execution).
    #[napi]
    pub fn columns(&self, env: Env) -> Result<Vec<String>> {
        let (columns, _) = self.run_query(&env)?;
        Ok(columns)
    }
}

/// Matches better-sqlite3's `RunResult` shape. Both fields are 0 for
/// now — the engine doesn't track affected-row counts or
/// last-insert-rowid at the public API layer yet. Kept so upgrading
/// to real tracking doesn't break the JS surface.
#[napi(object)]
pub struct RunResult {
    pub changes: i64,
    pub last_insert_rowid: i64,
}
