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
        })
    }

    /// Opens an existing file read-only — shared OS lock, multi-reader
    /// safe, any write throws.
    #[napi(factory)]
    pub fn open_read_only(database: String) -> Result<Self> {
        let conn = RustConnection::open_read_only(PathBuf::from(database)).map_err(map_err)?;
        Ok(Self {
            inner: RefCell::new(Some(conn)),
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
