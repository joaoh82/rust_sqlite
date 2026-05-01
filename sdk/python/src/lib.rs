//! Python bindings for SQLRite (Phase 5c).
//!
//! Exposes a `sqlrite` module on the Python side shaped after PEP 249
//! / the stdlib `sqlite3` module — users who know either should be
//! able to pick it up without reading the docs:
//!
//! ```python
//! import sqlrite
//!
//! conn = sqlrite.connect("foo.sqlrite")
//! cur = conn.cursor()
//! cur.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
//! cur.execute("INSERT INTO users (name) VALUES ('alice')")
//! cur.execute("SELECT id, name FROM users")
//! for row in cur:
//!     print(row[0], row[1])
//! conn.close()
//! ```
//!
//! ## Implementation notes
//!
//! - We wrap the Rust `Connection` from the `sqlrite` crate directly
//!   (not via the C FFI from `sqlrite-ffi`). PyO3 marshals types
//!   cheaper than a C round-trip, so going via the Rust API is
//!   strictly better for performance and avoids a double-layer of
//!   error mapping.
//!
//! - Every Rust error surfaces as a Python `sqlrite.SQLRiteError`
//!   exception. No silent swallowing — if something went wrong the
//!   Python caller sees a traceback.
//!
//! - Parameter binding (`cur.execute(sql, params)`) isn't in the
//!   engine yet — deferred to Phase 5a.2. The wrapper accepts the
//!   DB-API signature but raises `TypeError` if a non-empty
//!   parameter tuple is passed. Callers should inline values into
//!   the SQL for the moment (with manual escaping — full support
//!   lands in 5a.2).
//!
//! - GIL handling: we hold the GIL for the duration of each call.
//!   This keeps the bindings simple and is fine for the small-DB
//!   use case; Phase 5c.2 will explore `py.allow_threads` to
//!   release the GIL during long-running queries once the cursor
//!   abstraction lands.

use std::path::PathBuf;
use std::sync::Mutex;

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyList, PyTuple};

use sqlrite::ask::{
    AskConfig as RustAskConfig, AskResponse as RustAskResponse, CacheTtl, ProviderKind, Usage,
    ask_with_database,
};
use sqlrite::{Connection as RustConnection, OwnedRow, Rows, Value};

// ---------------------------------------------------------------------------
// Exception type
//
// Every Rust-side error bubbles up as this. Mirrors DB-API 2.0's
// `DatabaseError` — we keep a single exception type for simplicity;
// finer-grained types (IntegrityError, ProgrammingError, etc.) are
// a natural later refinement once the engine distinguishes them.

pyo3::create_exception!(
    sqlrite,
    SQLRiteError,
    pyo3::exceptions::PyException,
    "Base error class for SQLRite failures."
);

fn map_err<E: std::fmt::Display>(e: E) -> PyErr {
    SQLRiteError::new_err(e.to_string())
}

// ---------------------------------------------------------------------------
// Connection
//
// Wraps `RustConnection` behind a `Mutex` so Python callers can share
// a connection between threads (PyO3 requires `#[pyclass]` types to
// be `Send + Sync`). The Rust `Connection` isn't `Sync`, so the
// Mutex is the straightforward fix — callers still need to serialize
// access, but they won't get a panic.

/// Open a connection to a SQLRite database file. Use `:memory:` to
/// get an in-memory database (matching sqlite3 convention).
#[pyfunction]
#[pyo3(text_signature = "(database, /)")]
fn connect(database: &str) -> PyResult<Connection> {
    let rust_conn = if database == ":memory:" {
        RustConnection::open_in_memory().map_err(map_err)?
    } else {
        RustConnection::open(PathBuf::from(database)).map_err(map_err)?
    };
    Ok(Connection {
        inner: Some(Mutex::new(rust_conn)),
        ask_config: None,
    })
}

/// Open a database file read-only (shared OS lock; coexists with
/// other read-only openers, excluded by any writer).
#[pyfunction]
#[pyo3(text_signature = "(database, /)")]
fn connect_read_only(database: &str) -> PyResult<Connection> {
    let rust_conn = RustConnection::open_read_only(PathBuf::from(database)).map_err(map_err)?;
    Ok(Connection {
        inner: Some(Mutex::new(rust_conn)),
        ask_config: None,
    })
}

/// A database connection. Obtain one via [`connect`].
#[pyclass]
struct Connection {
    // `Option<_>` so `close()` can explicitly drop the inner
    // connection (and release the OS-level file lock) without
    // waiting for GC. Operations on a closed connection raise.
    inner: Option<Mutex<RustConnection>>,
    // Phase 7g.4 — per-connection ask() config. Set via
    // `set_ask_config()` or passed per-call to `ask()` / `ask_run()`.
    // When None, `ask()` falls back to `AskConfig::from_env()` so
    // env-only consumers get the zero-config experience matching the
    // REPL and Desktop surfaces.
    ask_config: Option<RustAskConfig>,
}

impl Connection {
    fn with_inner<F, T>(&mut self, op: &str, f: F) -> PyResult<T>
    where
        F: FnOnce(&mut RustConnection) -> PyResult<T>,
    {
        let guard = self
            .inner
            .as_ref()
            .ok_or_else(|| SQLRiteError::new_err(format!("cannot {op}: connection is closed")))?;
        let mut locked = guard
            .lock()
            .map_err(|_| SQLRiteError::new_err("connection mutex poisoned"))?;
        f(&mut locked)
    }
}

#[pymethods]
impl Connection {
    /// Returns a new cursor. Cursors don't share row state, so
    /// multiple cursors against the same connection can iterate
    /// independently.
    fn cursor(slf: Py<Self>) -> Cursor {
        Cursor {
            conn: slf,
            current_rows: None,
            description: None,
            last_status: None,
        }
    }

    /// Convenience shorthand for `cursor().execute(sql)`. Returns
    /// the cursor so you can chain `.fetchall()` off it.
    #[pyo3(signature = (sql, params=None))]
    fn execute(
        slf: Py<Self>,
        py: Python<'_>,
        sql: &str,
        params: Option<Py<PyAny>>,
    ) -> PyResult<Cursor> {
        let mut cur = Self::cursor(slf);
        cur.execute(py, sql, params)?;
        Ok(cur)
    }

    /// Commits the current transaction. Equivalent to `cursor().execute("COMMIT")`,
    /// but a no-op if no transaction is open (matching the DB-API's
    /// expectation that `commit()` is always safe to call).
    fn commit(&mut self) -> PyResult<()> {
        self.with_inner("commit", |c| {
            if c.in_transaction() {
                c.execute("COMMIT").map(|_| ()).map_err(map_err)?;
            }
            Ok(())
        })
    }

    /// Rolls back the current transaction. No-op if no transaction
    /// is open (again: DB-API expectation).
    fn rollback(&mut self) -> PyResult<()> {
        self.with_inner("rollback", |c| {
            if c.in_transaction() {
                c.execute("ROLLBACK").map(|_| ()).map_err(map_err)?;
            }
            Ok(())
        })
    }

    /// Closes the connection and releases the OS file lock. Safe to
    /// call multiple times; a closed connection raises `SQLRiteError`
    /// on any subsequent operation.
    fn close(&mut self) -> PyResult<()> {
        self.inner = None;
        Ok(())
    }

    /// Context-manager entry — returns self unchanged.
    fn __enter__(slf: PyRefMut<'_, Self>) -> PyRefMut<'_, Self> {
        slf
    }

    /// Context-manager exit — commits on clean exit, rolls back on
    /// exception (mirrors the stdlib `sqlite3` behavior), then closes.
    #[pyo3(signature = (exc_type=None, _exc_value=None, _traceback=None))]
    fn __exit__(
        &mut self,
        exc_type: Option<Py<PyAny>>,
        _exc_value: Option<Py<PyAny>>,
        _traceback: Option<Py<PyAny>>,
    ) -> PyResult<bool> {
        if self.inner.is_some() {
            if exc_type.is_some() {
                self.rollback()?;
            } else {
                self.commit()?;
            }
        }
        self.close()?;
        // Return False to signal "don't suppress any exception the
        // with-block may have raised".
        Ok(false)
    }

    /// `True` while a `BEGIN … COMMIT/ROLLBACK` block is open.
    #[getter]
    fn in_transaction(&self) -> PyResult<bool> {
        let guard = self
            .inner
            .as_ref()
            .ok_or_else(|| SQLRiteError::new_err("connection is closed"))?;
        let locked = guard
            .lock()
            .map_err(|_| SQLRiteError::new_err("connection mutex poisoned"))?;
        Ok(locked.in_transaction())
    }

    /// `True` if this connection was opened read-only.
    #[getter]
    fn read_only(&self) -> PyResult<bool> {
        let guard = self
            .inner
            .as_ref()
            .ok_or_else(|| SQLRiteError::new_err("connection is closed"))?;
        let locked = guard
            .lock()
            .map_err(|_| SQLRiteError::new_err("connection mutex poisoned"))?;
        Ok(locked.is_read_only())
    }

    // ---------------------------------------------------------------
    // Phase 7g.4 — natural-language → SQL.
    //
    // Three entry points:
    //   * `set_ask_config(...)` stores a config on the connection so
    //     subsequent `ask()` calls reuse it without reconfiguring.
    //   * `ask(question, config=None)` generates SQL — does NOT execute.
    //     Returns an `AskResponse` with `.sql` / `.explanation` / `.usage`.
    //   * `ask_run(question, config=None)` is the convenience that
    //     calls `ask()` then `execute()` on the generated SQL,
    //     returning a `Cursor` you can `.fetchall()` from.
    //
    // Config resolution (when `config` arg is None):
    //   1. The per-connection `ask_config` if set via set_ask_config()
    //   2. AskConfig::from_env() — reads SQLRITE_LLM_API_KEY etc.
    //   3. Built-in defaults (Sonnet 4.6, max_tokens 1024, 5-min cache TTL)
    //
    // The schema dump + LLM HTTP call run entirely on the Rust side
    // (no GIL re-acquisition for the duration of the network round-
    // trip). API key is read from the AskConfig — never logged, never
    // serialized into AskResponse, never crosses the FFI boundary
    // back to Python (we only return sql/explanation/usage).

    /// Stash an `AskConfig` on the connection. Subsequent `ask()` and
    /// `ask_run()` calls without an explicit config use this. Pass
    /// `None` to clear and fall back to env/defaults.
    #[pyo3(signature = (config))]
    fn set_ask_config(&mut self, config: Option<&AskConfig>) {
        self.ask_config = config.map(|c| c.inner.clone());
    }

    /// Generate SQL from a natural-language question. Does **not**
    /// execute — call `cur.execute(resp.sql)` (or `ask_run()` for
    /// one-shot). Returns an `AskResponse` with `.sql`,
    /// `.explanation`, and `.usage`.
    ///
    /// **GIL handling.** Releases the GIL for the duration of the
    /// HTTP call. Without this, a Python-side HTTP mock server (or
    /// any other thread) can't run concurrently with `ask()` — they'd
    /// sit blocked waiting for the GIL while ureq waited for them
    /// to respond. Same threading rule as the rest of PyO3 land:
    /// hold the GIL only for Python-data work, release it for I/O.
    #[pyo3(signature = (question, config=None))]
    fn ask(
        &mut self,
        py: Python<'_>,
        question: &str,
        config: Option<&AskConfig>,
    ) -> PyResult<AskResponse> {
        let resolved = self.resolve_ask_config(config)?;
        // Borrow the engine connection for schema dump + LLM call.
        // `ask_with_database` takes &Database (read-only), so we
        // hold the lock for the duration of one call.
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| SQLRiteError::new_err("cannot ask: connection is closed"))?;
        // We can't take the mutex inside py.allow_threads (the
        // borrow on `self` needs the GIL released semantics it
        // doesn't have access to), so we lock first, then release
        // the GIL across the network call. The lock guard lives
        // through the allow_threads block — that's fine, it's a
        // pure-Rust mutex with no Python state.
        let locked = inner
            .lock()
            .map_err(|_| SQLRiteError::new_err("connection mutex poisoned"))?;
        let resp = py
            .allow_threads(|| ask_with_database(locked.database(), question, &resolved))
            .map_err(map_err)?;
        Ok(AskResponse::from_rust(resp))
    }

    /// Generate SQL **and execute it**. Returns a `Cursor` with the
    /// results — call `.fetchall()` / `.fetchone()` / iterate. Errors
    /// the same way `ask()` does on generation failure, and the same
    /// way `cursor.execute()` does on bad-SQL execution failure (the
    /// model produced something the engine can't run).
    ///
    /// Convenience for one-shot scripts and notebooks. For interactive
    /// REPL-style use, prefer `ask()` + manual review (the model can
    /// be wrong; auto-execute hides that).
    #[pyo3(signature = (question, config=None))]
    fn ask_run(
        slf: Py<Self>,
        py: Python<'_>,
        question: &str,
        config: Option<&AskConfig>,
    ) -> PyResult<Cursor> {
        let resp = {
            let mut conn = slf.borrow_mut(py);
            conn.ask(py, question, config)?
        };
        if resp.sql.trim().is_empty() {
            return Err(SQLRiteError::new_err(format!(
                "model declined to generate SQL: {}",
                if resp.explanation.is_empty() {
                    "(no explanation)"
                } else {
                    resp.explanation.as_str()
                }
            )));
        }
        Self::execute(slf, py, &resp.sql, None)
    }
}

impl Connection {
    /// Resolve the effective AskConfig for an `ask()` / `ask_run()`
    /// call: per-call config wins, then per-connection, then env, then
    /// defaults. See the comment block above the methods for the
    /// rationale.
    fn resolve_ask_config(&self, per_call: Option<&AskConfig>) -> PyResult<RustAskConfig> {
        if let Some(cfg) = per_call {
            return Ok(cfg.inner.clone());
        }
        if let Some(cfg) = &self.ask_config {
            return Ok(cfg.clone());
        }
        RustAskConfig::from_env().map_err(map_err)
    }
}

// ---------------------------------------------------------------------------
// AskConfig (Phase 7g.4)
//
// Mirrors the Rust AskConfig but with Python-friendly attribute
// access. Constructed via `AskConfig(api_key=..., model=...)` or
// `AskConfig.from_env()`. Stored on the connection via
// `conn.set_ask_config(cfg)` or passed per-call to `conn.ask(q, cfg)`.

/// LLM call configuration for `Connection.ask()` and `ask_run()`.
///
/// Construct from kwargs:
///
///     cfg = sqlrite.AskConfig(
///         api_key="sk-ant-...",
///         model="claude-sonnet-4-6",
///         max_tokens=1024,
///         cache_ttl="5m",
///     )
///
/// Or from environment vars (`SQLRITE_LLM_API_KEY` etc.):
///
///     cfg = sqlrite.AskConfig.from_env()
///
/// Stored on the connection so subsequent `ask()` calls reuse it:
///
///     conn.set_ask_config(cfg)
///     resp = conn.ask("How many users?")          # uses cfg
///
/// Or passed per-call (overrides any per-connection config):
///
///     resp = conn.ask("How many users?", cfg)
#[pyclass]
#[derive(Clone)]
struct AskConfig {
    inner: RustAskConfig,
}

#[pymethods]
impl AskConfig {
    /// Construct from kwargs. Any kwarg left unset uses the same
    /// default the Rust side does (provider=anthropic, model=
    /// `claude-sonnet-4-6`, max_tokens=1024, cache_ttl="5m").
    ///
    /// `provider`: `"anthropic"` (only currently supported).
    /// `cache_ttl`: `"5m"` (default), `"1h"`, or `"off"`.
    /// `base_url`: override the API base URL — production callers
    ///   leave this None; tests point it at a localhost mock.
    #[new]
    #[pyo3(signature = (
        provider="anthropic",
        api_key=None,
        model=None,
        max_tokens=None,
        cache_ttl=None,
        base_url=None,
    ))]
    fn new(
        provider: &str,
        api_key: Option<String>,
        model: Option<String>,
        max_tokens: Option<u32>,
        cache_ttl: Option<&str>,
        base_url: Option<String>,
    ) -> PyResult<Self> {
        let mut inner = RustAskConfig::default();
        inner.provider = match provider.to_ascii_lowercase().as_str() {
            "anthropic" => ProviderKind::Anthropic,
            other => {
                return Err(SQLRiteError::new_err(format!(
                    "unknown provider: {other} (supported: anthropic)"
                )));
            }
        };
        if let Some(k) = api_key {
            if !k.is_empty() {
                inner.api_key = Some(k);
            }
        }
        if let Some(m) = model {
            if !m.is_empty() {
                inner.model = m;
            }
        }
        if let Some(t) = max_tokens {
            inner.max_tokens = t;
        }
        if let Some(c) = cache_ttl {
            inner.cache_ttl = match c.to_ascii_lowercase().as_str() {
                "5m" | "5min" | "5minutes" => CacheTtl::FiveMinutes,
                "1h" | "1hr" | "1hour" => CacheTtl::OneHour,
                "off" | "none" | "disabled" => CacheTtl::Off,
                other => {
                    return Err(SQLRiteError::new_err(format!(
                        "unknown cache_ttl: {other} (expected 5m, 1h, or off)"
                    )));
                }
            };
        }
        if let Some(u) = base_url {
            if !u.is_empty() {
                inner.base_url = Some(u);
            }
        }
        Ok(AskConfig { inner })
    }

    /// Build an `AskConfig` from environment variables. Reads:
    ///   * `SQLRITE_LLM_PROVIDER` (default: anthropic)
    ///   * `SQLRITE_LLM_API_KEY`
    ///   * `SQLRITE_LLM_MODEL` (default: claude-sonnet-4-6)
    ///   * `SQLRITE_LLM_MAX_TOKENS` (default: 1024)
    ///   * `SQLRITE_LLM_CACHE_TTL` (default: 5m)
    ///
    /// A missing API key is NOT an error here — `from_env()` returns
    /// a config with `api_key=None`, and the `ask()` call later raises
    /// the friendlier `SQLRiteError("missing API key")`.
    #[staticmethod]
    fn from_env() -> PyResult<Self> {
        Ok(AskConfig {
            inner: RustAskConfig::from_env().map_err(map_err)?,
        })
    }

    #[getter]
    fn api_key(&self) -> Option<&str> {
        self.inner.api_key.as_deref()
    }

    #[getter]
    fn model(&self) -> &str {
        &self.inner.model
    }

    #[getter]
    fn max_tokens(&self) -> u32 {
        self.inner.max_tokens
    }

    #[getter]
    fn cache_ttl(&self) -> &'static str {
        match self.inner.cache_ttl {
            CacheTtl::FiveMinutes => "5m",
            CacheTtl::OneHour => "1h",
            CacheTtl::Off => "off",
        }
    }

    #[getter]
    fn provider(&self) -> &'static str {
        match self.inner.provider {
            ProviderKind::Anthropic => "anthropic",
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "AskConfig(provider={:?}, model={:?}, max_tokens={}, cache_ttl={:?}, api_key={})",
            self.provider(),
            self.model(),
            self.max_tokens(),
            self.cache_ttl(),
            if self.inner.api_key.is_some() {
                "<set>"
            } else {
                "None"
            },
        )
    }
}

// ---------------------------------------------------------------------------
// AskResponse (Phase 7g.4)
//
// What conn.ask() returns. Carries the generated SQL, the model's
// one-sentence rationale, and token usage. The API key is NOT in
// here — by design.

/// Result of a `conn.ask()` call.
///
///     resp = conn.ask("How many users?")
///     print(resp.sql)              # generated SQL string
///     print(resp.explanation)      # one-sentence rationale
///     print(resp.usage.input_tokens, resp.usage.cache_read_input_tokens)
#[pyclass]
struct AskResponse {
    #[pyo3(get)]
    sql: String,
    #[pyo3(get)]
    explanation: String,
    #[pyo3(get)]
    usage: AskUsage,
}

impl AskResponse {
    fn from_rust(resp: RustAskResponse) -> Self {
        AskResponse {
            sql: resp.sql,
            explanation: resp.explanation,
            usage: AskUsage::from_rust(resp.usage),
        }
    }
}

#[pymethods]
impl AskResponse {
    fn __repr__(&self) -> String {
        format!(
            "AskResponse(sql={:?}, explanation={:?})",
            self.sql, self.explanation
        )
    }
}

/// Token usage breakdown from a `conn.ask()` call. Inspect to verify
/// prompt-caching is actually working — if `cache_read_input_tokens`
/// is zero across repeated calls with the same schema, something in
/// the prefix is invalidating the cache.
#[pyclass]
#[derive(Clone)]
struct AskUsage {
    #[pyo3(get)]
    input_tokens: u64,
    #[pyo3(get)]
    output_tokens: u64,
    #[pyo3(get)]
    cache_creation_input_tokens: u64,
    #[pyo3(get)]
    cache_read_input_tokens: u64,
}

impl AskUsage {
    fn from_rust(u: Usage) -> Self {
        AskUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
        }
    }
}

#[pymethods]
impl AskUsage {
    fn __repr__(&self) -> String {
        format!(
            "AskUsage(input_tokens={}, output_tokens={}, \
             cache_creation_input_tokens={}, cache_read_input_tokens={})",
            self.input_tokens,
            self.output_tokens,
            self.cache_creation_input_tokens,
            self.cache_read_input_tokens
        )
    }
}

// ---------------------------------------------------------------------------
// Cursor
//
// Holds an optional owned `Rows` iterator from the last SELECT. Non-
// SELECT statements don't populate `current_rows`; iteration /
// fetchone / fetchall on a non-query cursor just returns empty.

#[pyclass]
struct Cursor {
    conn: Py<Connection>,
    // Once a SELECT runs, `current_rows` owns the row iterator we
    // drain via fetchone / fetchall / __next__.
    current_rows: Option<Rows>,
    // Last statement's column names, for `.description`. PEP 249
    // says `description` is a 7-tuple per column; we fill in only
    // the name and leave the rest None.
    description: Option<Vec<String>>,
    // Status string the engine emitted. Exposed for debugging /
    // doctests but not part of PEP 249.
    last_status: Option<String>,
}

impl Cursor {
    fn take_rows_for_iteration(&mut self) -> Option<&mut Rows> {
        self.current_rows.as_mut()
    }
}

#[pymethods]
impl Cursor {
    /// Executes a single SQL statement.
    ///
    /// `params`: reserved for a future parameter-binding
    /// implementation. Until Phase 5a.2 lands, passing any non-empty
    /// value raises `TypeError` — inline your values into the SQL
    /// for now (with manual escaping).
    #[pyo3(signature = (sql, params=None))]
    fn execute(&mut self, py: Python<'_>, sql: &str, params: Option<Py<PyAny>>) -> PyResult<()> {
        if let Some(p) = params.as_ref() {
            // Allow `None` and empty tuple/list for DB-API
            // compatibility; anything else errors.
            let non_empty = Python::with_gil(|py| {
                if p.is_none(py) {
                    return false;
                }
                if let Ok(seq) = p.bind(py).downcast::<PyTuple>() {
                    return !seq.is_empty();
                }
                if let Ok(seq) = p.bind(py).downcast::<PyList>() {
                    return !seq.is_empty();
                }
                true
            });
            if non_empty {
                return Err(PyTypeError::new_err(
                    "parameter binding is not yet supported — inline values into the SQL \
                     (a future Phase 5a.2 release will add real binding)",
                ));
            }
        }

        // Drive the shared connection. We detach the `Rows` iterator
        // from its borrow on Connection by collecting into
        // `OwnedRow` up front, then keep a Rows-like iterator here.
        let mut conn = self.conn.borrow_mut(py);
        conn.with_inner("execute", |c| {
            // Classify: is this a SELECT? If so, prepare + query and
            // stash the Rows iterator on `self`. Otherwise just run
            // it via `c.execute`.
            let trimmed = sql.trim_start();
            let is_query = trimmed
                .get(..6)
                .map(|s| s.eq_ignore_ascii_case("select"))
                .unwrap_or(false);

            if is_query {
                let stmt = c.prepare(sql).map_err(map_err)?;
                let rows = stmt.query().map_err(map_err)?;
                self.description = Some(rows.columns().to_vec());
                self.current_rows = Some(rows);
                self.last_status = Some("SELECT Statement prepared.".to_string());
            } else {
                let status = c.execute(sql).map_err(map_err)?;
                self.current_rows = None;
                self.description = None;
                self.last_status = Some(status);
            }
            Ok(())
        })
    }

    /// Iterate a list of SQL statements. Each call is separate —
    /// this is different from SQLite's `executescript`; we keep the
    /// DB-API-style `executemany(sql, param_list)` signature but
    /// currently just ignore the param_list.
    #[pyo3(signature = (sql, seq_of_params=None))]
    fn executemany(
        &mut self,
        py: Python<'_>,
        sql: &str,
        seq_of_params: Option<Py<PyAny>>,
    ) -> PyResult<()> {
        if let Some(p) = seq_of_params.as_ref() {
            let n = Python::with_gil(|py| -> PyResult<usize> {
                if p.is_none(py) {
                    return Ok(0);
                }
                if let Ok(seq) = p.bind(py).downcast::<PyList>() {
                    return Ok(seq.len());
                }
                if let Ok(seq) = p.bind(py).downcast::<PyTuple>() {
                    return Ok(seq.len());
                }
                Err(PyTypeError::new_err(
                    "executemany expected a list or tuple of parameter sequences",
                ))
            })?;
            if n > 0 {
                return Err(PyTypeError::new_err(
                    "parameter binding is not yet supported — Phase 5a.2",
                ));
            }
        }
        self.execute(py, sql, None)
    }

    /// Runs several statements in one call, separated by `;`. Matches
    /// sqlite3's `executescript`.
    fn executescript(&mut self, py: Python<'_>, sql: &str) -> PyResult<()> {
        for stmt in sql.split(';') {
            let trimmed = stmt.trim();
            if trimmed.is_empty() {
                continue;
            }
            self.execute(py, trimmed, None)?;
        }
        Ok(())
    }

    /// Returns the next row as a tuple, or `None` when the query is
    /// exhausted. Raises if no SELECT has been run.
    fn fetchone(&mut self, py: Python<'_>) -> PyResult<Option<Py<PyTuple>>> {
        let Some(rows) = self.take_rows_for_iteration() else {
            return Ok(None);
        };
        match rows.next().map_err(map_err)? {
            Some(row) => {
                let owned = row.to_owned_row();
                Ok(Some(owned_row_to_tuple(py, &owned)?))
            }
            None => Ok(None),
        }
    }

    /// Returns up to `size` remaining rows. If `size` is None,
    /// returns all remaining rows (== `fetchall`).
    #[pyo3(signature = (size=None))]
    fn fetchmany(&mut self, py: Python<'_>, size: Option<usize>) -> PyResult<Py<PyList>> {
        let Some(rows) = self.take_rows_for_iteration() else {
            return Ok(PyList::empty(py).into());
        };
        let limit = size.unwrap_or(usize::MAX);
        let mut out: Vec<Py<PyTuple>> = Vec::new();
        while out.len() < limit {
            match rows.next().map_err(map_err)? {
                Some(row) => {
                    let owned = row.to_owned_row();
                    out.push(owned_row_to_tuple(py, &owned)?);
                }
                None => break,
            }
        }
        Ok(PyList::new(py, out)?.into())
    }

    /// Returns every remaining row as a list of tuples.
    fn fetchall(&mut self, py: Python<'_>) -> PyResult<Py<PyList>> {
        self.fetchmany(py, None)
    }

    /// DB-API 2.0 column metadata. Returns a list of 7-tuples with
    /// the column name in position 0 and None for the other fields
    /// (type_code, display_size, internal_size, precision, scale,
    /// null_ok), matching what `sqlite3.Cursor.description` returns.
    #[getter]
    fn description(&self, py: Python<'_>) -> PyResult<Option<Py<PyList>>> {
        let Some(cols) = self.description.as_ref() else {
            return Ok(None);
        };
        let mut out: Vec<Py<PyTuple>> = Vec::with_capacity(cols.len());
        for name in cols {
            out.push(
                PyTuple::new(
                    py,
                    [
                        name.into_pyobject(py)?.into_any().unbind(),
                        py.None(),
                        py.None(),
                        py.None(),
                        py.None(),
                        py.None(),
                        py.None(),
                    ],
                )?
                .into(),
            );
        }
        Ok(Some(PyList::new(py, out)?.into()))
    }

    /// `-1` per PEP 249 (we don't track affected-row counts yet).
    #[getter]
    fn rowcount(&self) -> i64 {
        -1
    }

    /// `__iter__(self)` returns self — lets `for row in cursor:`
    /// work via the PEP 249 iteration protocol.
    fn __iter__(slf: PyRefMut<'_, Self>) -> PyRefMut<'_, Self> {
        slf
    }

    /// Yields the next row as a tuple, or signals StopIteration.
    fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<Py<PyTuple>>> {
        self.fetchone(py)
    }

    fn close(&mut self) -> PyResult<()> {
        self.current_rows = None;
        self.description = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Value → Python conversions

fn value_to_pyobject(py: Python<'_>, v: &Value) -> PyResult<Py<PyAny>> {
    match v {
        Value::Integer(n) => Ok(n.into_pyobject(py)?.into_any().unbind()),
        Value::Real(f) => Ok(f.into_pyobject(py)?.into_any().unbind()),
        Value::Text(s) => Ok(s.into_pyobject(py)?.into_any().unbind()),
        Value::Bool(b) => {
            // `bool::into_pyobject` returns a Borrowed<PyBool> (Python's
            // True/False singletons are never owned), so clone into a
            // Bound before erasing the type.
            Ok(b.into_pyobject(py)?.to_owned().into_any().unbind())
        }
        // Phase 7a — `VECTOR(N)` columns surface to Python as a `list[float]`.
        // Widening f32→f64 here so Python's float (which is f64-backed)
        // doesn't lose information; numpy interop / array module are
        // future polish.
        Value::Vector(elements) => {
            let widened: Vec<f64> = elements.iter().map(|x| *x as f64).collect();
            Ok(widened.into_pyobject(py)?.into_any().unbind())
        }
        Value::Null => Ok(py.None()),
    }
}

fn owned_row_to_tuple(py: Python<'_>, row: &OwnedRow) -> PyResult<Py<PyTuple>> {
    let mut objs: Vec<Py<PyAny>> = Vec::with_capacity(row.values.len());
    for v in &row.values {
        objs.push(value_to_pyobject(py, v)?);
    }
    Ok(PyTuple::new(py, objs)?.into())
}

// ---------------------------------------------------------------------------
// Module entry point

/// The `sqlrite` Python module.
#[pymodule]
#[pyo3(name = "sqlrite")]
fn sqlrite_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("SQLRiteError", m.py().get_type::<SQLRiteError>())?;
    m.add_function(wrap_pyfunction!(connect, m)?)?;
    m.add_function(wrap_pyfunction!(connect_read_only, m)?)?;
    m.add_class::<Connection>()?;
    m.add_class::<Cursor>()?;
    // Phase 7g.4 — natural-language → SQL surface.
    m.add_class::<AskConfig>()?;
    m.add_class::<AskResponse>()?;
    m.add_class::<AskUsage>()?;
    Ok(())
}

// Tests live on the Python side under `sdk/python/tests/`. A PyO3
// cdylib built with the `extension-module` feature doesn't link
// libpython, so running it as a standalone `cargo test` binary
// would segfault on the first Python API call — the real coverage
// comes from `python -m pytest sdk/python/tests/` after a
// `maturin develop`.
