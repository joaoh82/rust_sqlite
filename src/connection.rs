//! Public `Connection` / `Statement` / `Rows` / `Row` API (Phase 5a + SQLR-23).
//!
//! This is the stable surface external consumers bind against — Rust
//! callers use it directly, language SDKs (Python, Node.js, Go) bind
//! against the C FFI wrapper over these same types in Phase 5b, and
//! the WASM build in Phase 5g re-exposes them via `wasm-bindgen`.
//!
//! The shape mirrors `rusqlite` / Python's `sqlite3` so users
//! familiar with either can pick it up immediately:
//!
//! ```no_run
//! use sqlrite::Connection;
//!
//! let mut conn = Connection::open("foo.sqlrite")?;
//! conn.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")?;
//! conn.execute("INSERT INTO users (name) VALUES ('alice')")?;
//!
//! let mut stmt = conn.prepare("SELECT id, name FROM users")?;
//! let mut rows = stmt.query()?;
//! while let Some(row) = rows.next()? {
//!     let id: i64 = row.get(0)?;
//!     let name: String = row.get(1)?;
//!     println!("{id}: {name}");
//! }
//! # Ok::<(), sqlrite::SQLRiteError>(())
//! ```
//!
//! **Relationship to the internal engine.** A `Connection` owns a
//! `Database` (which owns a `Pager` for file-backed connections).
//! `execute` and `query` go through the same `process_command`
//! pipeline the REPL uses, just with typed row return instead of
//! pre-rendered tables. The internal `Database` / `Pager` stay
//! accessible via `sqlrite::sql::...` for the engine's own tests
//! and for the desktop app — but those paths aren't considered
//! stable API.
//!
//! # Prepared statements & parameter binding (SQLR-23)
//!
//! `Connection::prepare` parses the SQL once and stashes the AST on
//! the returned `Statement`. Subsequent calls to `Statement::query` /
//! `Statement::run` execute against the cached AST without re-running
//! sqlparser. Bound versions ([`Statement::query_with_params`] /
//! [`Statement::execute_with_params`]) accept a `&[Value]` slice that is
//! substituted into the cached AST at execute time — including
//! `Value::Vector(...)` for HNSW-eligible KNN queries, where binding
//! the query vector skips per-iter lexing of the 4 KB bracket-array
//! literal.
//!
//! [`Connection::prepare_cached`] adds a small per-connection LRU
//! (default cap 16) so a hot SQL string is parsed exactly once across
//! every call, not once per `prepare()`. Matches the rusqlite pattern.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;

use crate::sql::dialect::SqlriteDialect;
use sqlparser::ast::Statement as AstStatement;
use sqlparser::parser::Parser;

use crate::error::{Result, SQLRiteError};
use crate::sql::db::database::Database;
use crate::sql::db::table::Value;
use crate::sql::executor::execute_select_rows;
use crate::sql::pager::{AccessMode, open_database_with_mode, save_database};
use crate::sql::params::{rewrite_placeholders, substitute_params};
use crate::sql::parser::select::SelectQuery;
use crate::sql::process_ast_with_render;

/// Default capacity of the per-connection prepared-statement plan cache.
/// Matches rusqlite's default; tweak with [`Connection::set_prepared_cache_capacity`].
const DEFAULT_PREP_CACHE_CAP: usize = 16;

/// A handle to a SQLRite database. Opens a file or an in-memory DB;
/// drop it to close. Every mutating statement auto-saves (except inside
/// an explicit `BEGIN`/`COMMIT` block — see [Transactions](#transactions)).
///
/// ## Transactions
///
/// ```no_run
/// # use sqlrite::Connection;
/// let mut conn = Connection::open("foo.sqlrite")?;
/// conn.execute("BEGIN")?;
/// conn.execute("INSERT INTO users (name) VALUES ('alice')")?;
/// conn.execute("INSERT INTO users (name) VALUES ('bob')")?;
/// conn.execute("COMMIT")?;
/// # Ok::<(), sqlrite::SQLRiteError>(())
/// ```
///
/// `Connection` is `Send` but not `Sync` — clone it (it's currently
/// unclonable) or share via a `Mutex<Connection>` if you need
/// multi-threaded access.
pub struct Connection {
    db: Database,
    /// SQLR-23 — small SQL→cached-plan LRU. Keyed by the verbatim SQL
    /// string the caller passed to `prepare_cached`. Stored as a
    /// `VecDeque` rather than a HashMap+linked-list because the
    /// expected capacity is small (default 16) — linear scan is fine
    /// and the implementation stays dependency-free.
    prep_cache: VecDeque<(String, Arc<CachedPlan>)>,
    prep_cache_cap: usize,
}

impl Connection {
    /// Opens (or creates) a database file for read-write access.
    ///
    /// If the file doesn't exist, an empty one is materialized with the
    /// current format version. Takes an exclusive advisory lock on the
    /// file and its `-wal` sidecar; returns `Err` if either is already
    /// locked by another process.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let db_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("db")
            .to_string();
        let db = if path.exists() {
            open_database_with_mode(path, db_name, AccessMode::ReadWrite)?
        } else {
            // Fresh file: materialize on disk and keep the attached
            // pager. Setting `source_path` before `save_database` lets
            // its `same_path` branch create the pager and stash it
            // back on the Database — no reopen needed (and trying to
            // reopen here would hit the file's own lock).
            let mut fresh = Database::new(db_name);
            fresh.source_path = Some(path.to_path_buf());
            save_database(&mut fresh, path)?;
            fresh
        };
        Ok(Self::wrap(db))
    }

    /// Opens an existing database file for read-only access. Takes a
    /// shared advisory lock, so multiple read-only connections can
    /// coexist on the same file; any open writer excludes them.
    /// Mutating statements return `cannot execute: database is opened
    /// read-only`.
    pub fn open_read_only<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let db_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("db")
            .to_string();
        let db = open_database_with_mode(path, db_name, AccessMode::ReadOnly)?;
        Ok(Self::wrap(db))
    }

    /// Opens a transient in-memory database. No file is touched and no
    /// locks are taken; state lives for the lifetime of the
    /// `Connection` and is discarded on drop.
    pub fn open_in_memory() -> Result<Self> {
        Ok(Self::wrap(Database::new("memdb".to_string())))
    }

    fn wrap(db: Database) -> Self {
        Self {
            db,
            prep_cache: VecDeque::new(),
            prep_cache_cap: DEFAULT_PREP_CACHE_CAP,
        }
    }

    /// Parses and executes one SQL statement. For DDL (`CREATE TABLE`,
    /// `CREATE INDEX`), DML (`INSERT`, `UPDATE`, `DELETE`) and
    /// transaction control (`BEGIN`, `COMMIT`, `ROLLBACK`). Returns
    /// the status message the engine produced (e.g.
    /// `"INSERT Statement executed."`).
    ///
    /// For `SELECT`, `execute` works but discards the row data and
    /// just returns the rendered status — use [`Connection::prepare`]
    /// and [`Statement::query`] to iterate typed rows.
    pub fn execute(&mut self, sql: &str) -> Result<String> {
        crate::sql::process_command(sql, &mut self.db)
    }

    /// Prepares a statement for repeated execution or row iteration.
    /// SQLR-23: the SQL is parsed once at prepare time (sqlparser walk
    /// plus placeholder rewriting), and the resulting AST is cached
    /// on the [`Statement`] for re-execution without further parsing.
    ///
    /// Use [`Statement::query`] / [`Statement::run`] for unbound
    /// execution, or [`Statement::query_with_params`] /
    /// [`Statement::execute_with_params`] to substitute `?`
    /// placeholders.
    pub fn prepare<'c>(&'c mut self, sql: &str) -> Result<Statement<'c>> {
        let plan = Arc::new(CachedPlan::compile(sql)?);
        Ok(Statement { conn: self, plan })
    }

    /// Same as [`Connection::prepare`], but consults a small
    /// per-connection LRU first. SQLR-23 — for hot statements
    /// (the body of an INSERT loop, a frequently-rerun lookup) the
    /// sqlparser walk is amortized to once across the connection's
    /// lifetime, not once per `prepare()`.
    ///
    /// Default cache capacity is 16; tune with
    /// [`Connection::set_prepared_cache_capacity`].
    pub fn prepare_cached<'c>(&'c mut self, sql: &str) -> Result<Statement<'c>> {
        // Lookup-or-insert. Found entries are also moved to the back
        // (most-recently-used) so capacity-eviction runs LRU.
        let plan = if let Some(pos) = self.prep_cache.iter().position(|(k, _)| k == sql) {
            let (k, v) = self.prep_cache.remove(pos).unwrap();
            self.prep_cache.push_back((k, Arc::clone(&v)));
            v
        } else {
            let plan = Arc::new(CachedPlan::compile(sql)?);
            self.prep_cache
                .push_back((sql.to_string(), Arc::clone(&plan)));
            while self.prep_cache.len() > self.prep_cache_cap {
                self.prep_cache.pop_front();
            }
            plan
        };
        Ok(Statement { conn: self, plan })
    }

    /// SQLR-23 — sets the maximum number of cached prepared plans
    /// (matches `prepare_cached`'s default 16). Reducing below the
    /// current size evicts the oldest entries; setting to 0 disables
    /// caching but `prepare_cached` still works (it just always
    /// re-parses).
    pub fn set_prepared_cache_capacity(&mut self, cap: usize) {
        self.prep_cache_cap = cap;
        while self.prep_cache.len() > cap {
            self.prep_cache.pop_front();
        }
    }

    /// SQLR-23 — current number of plans held by the prepared-statement
    /// cache. Useful for tests / introspection; not load-bearing for
    /// the public API.
    pub fn prepared_cache_len(&self) -> usize {
        self.prep_cache.len()
    }

    /// Returns `true` while a `BEGIN … COMMIT/ROLLBACK` block is open
    /// against this connection.
    pub fn in_transaction(&self) -> bool {
        self.db.in_transaction()
    }

    /// Returns the current auto-VACUUM threshold (SQLR-10). After a
    /// page-releasing DDL (DROP TABLE / DROP INDEX / ALTER TABLE DROP
    /// COLUMN) commits, the engine compacts the file in place if the
    /// freelist exceeds this fraction of `page_count`. New connections
    /// default to `Some(0.25)` (SQLite parity); `None` means the
    /// trigger is disabled. See [`Connection::set_auto_vacuum_threshold`].
    pub fn auto_vacuum_threshold(&self) -> Option<f32> {
        self.db.auto_vacuum_threshold()
    }

    /// Sets the auto-VACUUM threshold (SQLR-10). `Some(t)` with `t` in
    /// `0.0..=1.0` arms the trigger; `None` disables it. Values outside
    /// `0.0..=1.0` (or NaN / infinite) return a typed error rather than
    /// silently saturating. The setting is per-connection runtime
    /// state — closing the connection drops it; new connections start
    /// at the default `Some(0.25)`.
    ///
    /// Calling this on an in-memory or read-only database is allowed
    /// (it just won't fire — there's nothing to compact / no writes
    /// will reach the trigger).
    pub fn set_auto_vacuum_threshold(&mut self, threshold: Option<f32>) -> Result<()> {
        self.db.set_auto_vacuum_threshold(threshold)
    }

    /// Returns `true` if the connection was opened read-only. Mutating
    /// statements on a read-only connection return a typed error.
    pub fn is_read_only(&self) -> bool {
        self.db.is_read_only()
    }

    /// Escape hatch for advanced callers — the internal `Database`
    /// backing this connection. Not part of the stable API; will move
    /// or change as Phase 5's cursor abstraction lands.
    #[doc(hidden)]
    pub fn database(&self) -> &Database {
        &self.db
    }

    #[doc(hidden)]
    pub fn database_mut(&mut self) -> &mut Database {
        &mut self.db
    }
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("in_transaction", &self.db.in_transaction())
            .field("read_only", &self.db.is_read_only())
            .field("tables", &self.db.tables.len())
            .field("prep_cache_len", &self.prep_cache.len())
            .finish()
    }
}

/// SQLR-23 — the parse-once-execute-many representation. Built by
/// `CachedPlan::compile` (sqlparser walk + placeholder rewriting +
/// SELECT narrowing) and shared between every `Statement` that hits
/// the same SQL string in `prepare_cached`.
#[derive(Debug)]
struct CachedPlan {
    /// Original SQL — kept for diagnostic output.
    #[allow(dead_code)]
    sql: String,
    /// AST after `?` → `?N` placeholder rewriting. Cloned per execute
    /// so the substitution pass leaves the cached copy intact.
    ast: AstStatement,
    /// Total `?` placeholder count in the source SQL. Strict bind
    /// validation in `query_with_params` / `execute_with_params`
    /// uses this.
    param_count: usize,
    /// SELECT narrowing — cached so `query()` doesn't redo the
    /// `SelectQuery::new` walk for unbound SELECTs. `None` for
    /// non-SELECT statements.
    select: Option<SelectQuery>,
}

impl CachedPlan {
    fn compile(sql: &str) -> Result<Self> {
        let dialect = SqlriteDialect::new();
        let mut ast = Parser::parse_sql(&dialect, sql).map_err(SQLRiteError::from)?;
        let Some(mut stmt) = ast.pop() else {
            return Err(SQLRiteError::General("no statement to prepare".to_string()));
        };
        if !ast.is_empty() {
            return Err(SQLRiteError::General(
                "prepare() accepts a single statement; found more than one".to_string(),
            ));
        }
        let param_count = rewrite_placeholders(&mut stmt);
        let select = match &stmt {
            AstStatement::Query(_) => Some(SelectQuery::new(&stmt)?),
            _ => None,
        };
        Ok(Self {
            sql: sql.to_string(),
            ast: stmt,
            param_count,
            select,
        })
    }
}

/// A prepared statement bound to a specific connection lifetime.
///
/// SQLR-23 — `Statement` carries the parsed AST (parsed exactly once
/// at prepare time), not just the raw SQL. `query` / `run` execute
/// against the cached AST; `query_with_params` / `execute_with_params`
/// clone the AST and substitute `?` placeholders before dispatch.
pub struct Statement<'c> {
    conn: &'c mut Connection,
    plan: Arc<CachedPlan>,
}

impl std::fmt::Debug for Statement<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Statement")
            .field("sql", &self.plan.sql)
            .field("param_count", &self.plan.param_count)
            .field(
                "kind",
                &match self.plan.select {
                    Some(_) => "Select",
                    None => "Other",
                },
            )
            .finish()
    }
}

impl<'c> Statement<'c> {
    /// Number of `?` placeholders detected in the source SQL. Strict
    /// arity validation: passing a slice of a different length to
    /// `query_with_params` / `execute_with_params` returns a typed
    /// error.
    pub fn parameter_count(&self) -> usize {
        self.plan.param_count
    }

    /// Executes a prepared non-query statement. Equivalent to
    /// [`Connection::execute`] — included for parity with the
    /// typed-row `query()` so callers who want `Statement::run` /
    /// `Statement::query` symmetry get it.
    ///
    /// Errors if the prepared SQL contains `?` placeholders — use
    /// [`Statement::execute_with_params`] for those.
    pub fn run(&mut self) -> Result<String> {
        if self.plan.param_count > 0 {
            return Err(SQLRiteError::General(format!(
                "statement has {} `?` placeholder(s); call execute_with_params()",
                self.plan.param_count
            )));
        }
        let ast = self.plan.ast.clone();
        process_ast_with_render(ast, &mut self.conn.db).map(|o| o.status)
    }

    /// SQLR-23 — executes a prepared non-SELECT statement after binding
    /// `?` placeholders to `params` (positional, in source order).
    ///
    /// Use this for parameterized INSERT / UPDATE / DELETE — the
    /// substitution clones the cached AST, fills in the `?` slots
    /// from `params`, and dispatches without re-running sqlparser.
    /// For SELECT, prefer [`Statement::query_with_params`].
    pub fn execute_with_params(&mut self, params: &[Value]) -> Result<String> {
        self.check_arity(params)?;
        let mut ast = self.plan.ast.clone();
        if !params.is_empty() {
            substitute_params(&mut ast, params)?;
        }
        process_ast_with_render(ast, &mut self.conn.db).map(|o| o.status)
    }

    /// Runs a SELECT and returns a [`Rows`] iterator over typed rows.
    /// Errors if the prepared statement isn't a SELECT.
    ///
    /// SQLR-23 — uses the SELECT narrowing cached at prepare time;
    /// no per-call sqlparser walk. Errors if the prepared SQL
    /// contains `?` placeholders — use [`Statement::query_with_params`]
    /// for those.
    pub fn query(&self) -> Result<Rows> {
        if self.plan.param_count > 0 {
            return Err(SQLRiteError::General(format!(
                "statement has {} `?` placeholder(s); call query_with_params()",
                self.plan.param_count
            )));
        }
        let Some(sq) = self.plan.select.as_ref() else {
            return Err(SQLRiteError::General(
                "query() only works on SELECT statements; use run() for DDL/DML".to_string(),
            ));
        };
        let result = execute_select_rows(sq.clone(), &self.conn.db)?;
        Ok(Rows {
            columns: result.columns,
            rows: result.rows.into_iter(),
        })
    }

    /// SQLR-23 — runs a SELECT and returns a [`Rows`] iterator after
    /// binding `?` placeholders to `params`. Positional, source-order
    /// indexing — `params[0]` is `?1`, `params[1]` is `?2`, etc.
    ///
    /// Vector parameters (`Value::Vector(...)`) substitute as the
    /// in-band bracket-array shape the executor recognizes, so a
    /// bound query vector still triggers the HNSW probe optimizer
    /// (Phase 7d.2 KNN shortcut).
    pub fn query_with_params(&self, params: &[Value]) -> Result<Rows> {
        self.check_arity(params)?;
        if self.plan.select.is_none() {
            return Err(SQLRiteError::General(
                "query_with_params() only works on SELECT statements; use execute_with_params() \
                 for DDL/DML"
                    .to_string(),
            ));
        }
        // Re-narrow against the substituted AST. The narrow walk is
        // cheap (it pulls projection/WHERE/ORDER BY into typed
        // structs), and rerunning it ensures the substituted literals
        // (e.g. a bracket-array vector) flow through `SelectQuery`.
        let mut ast = self.plan.ast.clone();
        if !params.is_empty() {
            substitute_params(&mut ast, params)?;
        }
        let sq = SelectQuery::new(&ast)?;
        let result = execute_select_rows(sq, &self.conn.db)?;
        Ok(Rows {
            columns: result.columns,
            rows: result.rows.into_iter(),
        })
    }

    fn check_arity(&self, params: &[Value]) -> Result<()> {
        if params.len() != self.plan.param_count {
            return Err(SQLRiteError::General(format!(
                "expected {} parameter{}, got {}",
                self.plan.param_count,
                if self.plan.param_count == 1 { "" } else { "s" },
                params.len()
            )));
        }
        Ok(())
    }

    /// Column names this statement will produce, in projection order.
    /// `None` for non-SELECT statements.
    pub fn column_names(&self) -> Option<Vec<String>> {
        match &self.plan.select {
            Some(_) => {
                // We can't know the concrete column list without
                // running the query (it depends on the table schema
                // and the projection). Callers who need it up front
                // should call query() and inspect Rows::columns.
                None
            }
            None => None,
        }
    }
}

/// Iterator of typed [`Row`] values produced by a `SELECT` query.
///
/// Today `Rows` is backed by an eager `Vec<Vec<Value>>` — the cursor
/// abstraction in Phase 5a's follow-up will swap this for a lazy
/// walker that streams rows off the B-Tree without materializing
/// them upfront. The `Rows::next` API is designed for that: it
/// returns `Result<Option<Row>>` rather than `Option<Result<Row>>`,
/// so a mid-stream I/O error surfaces cleanly.
pub struct Rows {
    columns: Vec<String>,
    rows: std::vec::IntoIter<Vec<Value>>,
}

impl std::fmt::Debug for Rows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rows")
            .field("columns", &self.columns)
            .field("remaining", &self.rows.len())
            .finish()
    }
}

impl Rows {
    /// Column names in projection order.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Advances to the next row. Returns `Ok(None)` when the query is
    /// exhausted, `Ok(Some(row))` otherwise, `Err(_)` on an I/O or
    /// decode failure (relevant once Phase 5a's cursor work lands —
    /// today this is always `Ok(_)`).
    pub fn next(&mut self) -> Result<Option<Row<'_>>> {
        Ok(self.rows.next().map(|values| Row {
            columns: &self.columns,
            values,
        }))
    }

    /// Collects every remaining row into a `Vec<Row>`. Convenient for
    /// small result sets; avoid on large queries — that's what the
    /// streaming [`Rows::next`] API is for.
    pub fn collect_all(mut self) -> Result<Vec<OwnedRow>> {
        let mut out = Vec::new();
        while let Some(r) = self.next()? {
            out.push(r.to_owned_row());
        }
        Ok(out)
    }
}

/// A single row borrowed from a [`Rows`] iterator. Lives only as long
/// as the iterator; call `Row::to_owned_row` to detach it if you need
/// to keep it past the next `next()` call.
pub struct Row<'r> {
    columns: &'r [String],
    values: Vec<Value>,
}

impl<'r> Row<'r> {
    /// Value at column index `idx`. Returns a clean error if out of
    /// bounds or the type conversion fails.
    pub fn get<T: FromValue>(&self, idx: usize) -> Result<T> {
        let v = self.values.get(idx).ok_or_else(|| {
            SQLRiteError::General(format!(
                "column index {idx} out of bounds (row has {} columns)",
                self.values.len()
            ))
        })?;
        T::from_value(v)
    }

    /// Value at column named `name`. Case-sensitive.
    pub fn get_by_name<T: FromValue>(&self, name: &str) -> Result<T> {
        let idx = self
            .columns
            .iter()
            .position(|c| c == name)
            .ok_or_else(|| SQLRiteError::General(format!("no column named '{name}' in row")))?;
        self.get(idx)
    }

    /// Column names for this row.
    pub fn columns(&self) -> &[String] {
        self.columns
    }

    /// Detaches from the parent `Rows` iterator. Useful when you want
    /// to keep rows past the next `Rows::next()` call.
    pub fn to_owned_row(&self) -> OwnedRow {
        OwnedRow {
            columns: self.columns.to_vec(),
            values: self.values.clone(),
        }
    }
}

/// A row detached from the `Rows` iterator — owns its data, no
/// borrow ties it to the parent iterator.
#[derive(Debug, Clone)]
pub struct OwnedRow {
    pub columns: Vec<String>,
    pub values: Vec<Value>,
}

impl OwnedRow {
    pub fn get<T: FromValue>(&self, idx: usize) -> Result<T> {
        let v = self.values.get(idx).ok_or_else(|| {
            SQLRiteError::General(format!(
                "column index {idx} out of bounds (row has {} columns)",
                self.values.len()
            ))
        })?;
        T::from_value(v)
    }

    pub fn get_by_name<T: FromValue>(&self, name: &str) -> Result<T> {
        let idx = self
            .columns
            .iter()
            .position(|c| c == name)
            .ok_or_else(|| SQLRiteError::General(format!("no column named '{name}' in row")))?;
        self.get(idx)
    }
}

/// Conversion from SQLRite's internal [`Value`] enum into a typed Rust
/// value. Implementations cover the common built-ins — `i64`, `f64`,
/// `String`, `bool`, and `Option<T>` for nullable columns. Extend on
/// demand.
pub trait FromValue: Sized {
    fn from_value(v: &Value) -> Result<Self>;
}

impl FromValue for i64 {
    fn from_value(v: &Value) -> Result<Self> {
        match v {
            Value::Integer(n) => Ok(*n),
            Value::Null => Err(SQLRiteError::General(
                "expected Integer, got NULL".to_string(),
            )),
            other => Err(SQLRiteError::General(format!(
                "cannot convert {other:?} to i64"
            ))),
        }
    }
}

impl FromValue for f64 {
    fn from_value(v: &Value) -> Result<Self> {
        match v {
            Value::Real(f) => Ok(*f),
            Value::Integer(n) => Ok(*n as f64),
            Value::Null => Err(SQLRiteError::General("expected Real, got NULL".to_string())),
            other => Err(SQLRiteError::General(format!(
                "cannot convert {other:?} to f64"
            ))),
        }
    }
}

impl FromValue for String {
    fn from_value(v: &Value) -> Result<Self> {
        match v {
            Value::Text(s) => Ok(s.clone()),
            Value::Null => Err(SQLRiteError::General("expected Text, got NULL".to_string())),
            other => Err(SQLRiteError::General(format!(
                "cannot convert {other:?} to String"
            ))),
        }
    }
}

impl FromValue for bool {
    fn from_value(v: &Value) -> Result<Self> {
        match v {
            Value::Bool(b) => Ok(*b),
            Value::Integer(n) => Ok(*n != 0),
            Value::Null => Err(SQLRiteError::General("expected Bool, got NULL".to_string())),
            other => Err(SQLRiteError::General(format!(
                "cannot convert {other:?} to bool"
            ))),
        }
    }
}

/// Nullable columns: `Option<T>` maps `NULL → None` and everything else
/// through the inner type's `FromValue` impl.
impl<T: FromValue> FromValue for Option<T> {
    fn from_value(v: &Value) -> Result<Self> {
        match v {
            Value::Null => Ok(None),
            other => Ok(Some(T::from_value(other)?)),
        }
    }
}

/// Identity impl so `row.get::<_, Value>(0)` works when you want
/// untyped access.
impl FromValue for Value {
    fn from_value(v: &Value) -> Result<Self> {
        Ok(v.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("sqlrite-conn-{pid}-{nanos}-{name}.sqlrite"));
        p
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let _ = std::fs::remove_file(std::path::PathBuf::from(wal));
    }

    #[test]
    fn in_memory_roundtrip() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER);")
            .unwrap();
        conn.execute("INSERT INTO users (name, age) VALUES ('alice', 30);")
            .unwrap();
        conn.execute("INSERT INTO users (name, age) VALUES ('bob', 25);")
            .unwrap();

        let stmt = conn.prepare("SELECT id, name, age FROM users;").unwrap();
        let mut rows = stmt.query().unwrap();
        assert_eq!(rows.columns(), &["id", "name", "age"]);
        let mut collected: Vec<(i64, String, i64)> = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            collected.push((
                row.get::<i64>(0).unwrap(),
                row.get::<String>(1).unwrap(),
                row.get::<i64>(2).unwrap(),
            ));
        }
        assert_eq!(collected.len(), 2);
        assert!(collected.iter().any(|(_, n, a)| n == "alice" && *a == 30));
        assert!(collected.iter().any(|(_, n, a)| n == "bob" && *a == 25));
    }

    #[test]
    fn file_backed_persists_across_connections() {
        let path = tmp_path("persist");
        {
            let mut c1 = Connection::open(&path).unwrap();
            c1.execute("CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT);")
                .unwrap();
            c1.execute("INSERT INTO items (label) VALUES ('one');")
                .unwrap();
        }
        {
            let mut c2 = Connection::open(&path).unwrap();
            let stmt = c2.prepare("SELECT label FROM items;").unwrap();
            let mut rows = stmt.query().unwrap();
            let first = rows.next().unwrap().expect("one row");
            assert_eq!(first.get::<String>(0).unwrap(), "one");
            assert!(rows.next().unwrap().is_none());
        }
        cleanup(&path);
    }

    #[test]
    fn read_only_connection_rejects_writes() {
        let path = tmp_path("ro_reject");
        {
            let mut c = Connection::open(&path).unwrap();
            c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY);")
                .unwrap();
            c.execute("INSERT INTO t (id) VALUES (1);").unwrap();
        } // writer drops → releases exclusive lock

        let mut ro = Connection::open_read_only(&path).unwrap();
        assert!(ro.is_read_only());
        let err = ro.execute("INSERT INTO t (id) VALUES (2);").unwrap_err();
        assert!(format!("{err}").contains("read-only"));
        cleanup(&path);
    }

    #[test]
    fn transactions_work_through_connection() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER);")
            .unwrap();
        conn.execute("INSERT INTO t (x) VALUES (1);").unwrap();

        conn.execute("BEGIN;").unwrap();
        assert!(conn.in_transaction());
        conn.execute("INSERT INTO t (x) VALUES (2);").unwrap();
        conn.execute("ROLLBACK;").unwrap();
        assert!(!conn.in_transaction());

        let stmt = conn.prepare("SELECT x FROM t;").unwrap();
        let rows = stmt.query().unwrap().collect_all().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 1);
    }

    #[test]
    fn get_by_name_works() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (a INTEGER, b TEXT);").unwrap();
        conn.execute("INSERT INTO t (a, b) VALUES (42, 'hello');")
            .unwrap();

        let stmt = conn.prepare("SELECT a, b FROM t;").unwrap();
        let mut rows = stmt.query().unwrap();
        let row = rows.next().unwrap().unwrap();
        assert_eq!(row.get_by_name::<i64>("a").unwrap(), 42);
        assert_eq!(row.get_by_name::<String>("b").unwrap(), "hello");
    }

    #[test]
    fn null_column_maps_to_none() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, note TEXT);")
            .unwrap();
        // id INTEGER PRIMARY KEY autoincrements; `note` is left unspecified.
        conn.execute("INSERT INTO t (id) VALUES (1);").unwrap();

        let stmt = conn.prepare("SELECT id, note FROM t;").unwrap();
        let mut rows = stmt.query().unwrap();
        let row = rows.next().unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 1);
        // note is NULL → Option<String> resolves to None.
        assert_eq!(row.get::<Option<String>>(1).unwrap(), None);
    }

    #[test]
    fn prepare_rejects_multiple_statements() {
        let mut conn = Connection::open_in_memory().unwrap();
        let err = conn.prepare("SELECT 1; SELECT 2;").unwrap_err();
        assert!(format!("{err}").contains("single statement"));
    }

    #[test]
    fn query_on_non_select_errors() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY);")
            .unwrap();
        let stmt = conn.prepare("INSERT INTO t VALUES (1);").unwrap();
        let err = stmt.query().unwrap_err();
        assert!(format!("{err}").contains("SELECT"));
    }

    /// SQLR-10: fresh connections expose the SQLite-parity 25% default,
    /// the setter validates its input, and `None` opts out cleanly.
    #[test]
    fn auto_vacuum_threshold_default_and_setter() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(
            conn.auto_vacuum_threshold(),
            Some(0.25),
            "fresh connection should ship with the SQLite-parity default"
        );

        conn.set_auto_vacuum_threshold(None).unwrap();
        assert_eq!(conn.auto_vacuum_threshold(), None);

        conn.set_auto_vacuum_threshold(Some(0.5)).unwrap();
        assert_eq!(conn.auto_vacuum_threshold(), Some(0.5));

        // Out-of-range values must be rejected with a typed error and
        // must not stomp the previously-set value.
        let err = conn.set_auto_vacuum_threshold(Some(1.5)).unwrap_err();
        assert!(
            format!("{err}").contains("auto_vacuum_threshold"),
            "expected typed range error, got: {err}"
        );
        assert_eq!(
            conn.auto_vacuum_threshold(),
            Some(0.5),
            "rejected setter call must not mutate the threshold"
        );
    }

    #[test]
    fn index_out_of_bounds_errors_cleanly() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (a INTEGER PRIMARY KEY);")
            .unwrap();
        conn.execute("INSERT INTO t (a) VALUES (1);").unwrap();
        let stmt = conn.prepare("SELECT a FROM t;").unwrap();
        let mut rows = stmt.query().unwrap();
        let row = rows.next().unwrap().unwrap();
        let err = row.get::<i64>(99).unwrap_err();
        assert!(format!("{err}").contains("out of bounds"));
    }

    // -----------------------------------------------------------------
    // SQLR-23 — prepared-statement plan cache + parameter binding
    // -----------------------------------------------------------------

    #[test]
    fn parameter_count_reflects_question_marks() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (a INTEGER, b TEXT);").unwrap();
        let stmt = conn.prepare("SELECT a, b FROM t WHERE a = ?").unwrap();
        assert_eq!(stmt.parameter_count(), 1);
        let stmt = conn
            .prepare("SELECT a, b FROM t WHERE a = ? AND b = ?")
            .unwrap();
        assert_eq!(stmt.parameter_count(), 2);
        let stmt = conn.prepare("SELECT a FROM t").unwrap();
        assert_eq!(stmt.parameter_count(), 0);
    }

    #[test]
    fn query_with_params_binds_scalars() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT);")
            .unwrap();
        conn.execute("INSERT INTO t (a, b) VALUES (1, 'alice');")
            .unwrap();
        conn.execute("INSERT INTO t (a, b) VALUES (2, 'bob');")
            .unwrap();
        conn.execute("INSERT INTO t (a, b) VALUES (3, 'carol');")
            .unwrap();

        let stmt = conn.prepare("SELECT b FROM t WHERE a = ?").unwrap();
        let rows = stmt
            .query_with_params(&[Value::Integer(2)])
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<String>(0).unwrap(), "bob");
    }

    #[test]
    fn execute_with_params_binds_insert_values() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (a INTEGER, b TEXT);").unwrap();

        let mut stmt = conn.prepare("INSERT INTO t (a, b) VALUES (?, ?)").unwrap();
        stmt.execute_with_params(&[Value::Integer(7), Value::Text("hi".into())])
            .unwrap();
        stmt.execute_with_params(&[Value::Integer(8), Value::Text("yo".into())])
            .unwrap();

        let stmt = conn.prepare("SELECT a, b FROM t").unwrap();
        let rows = stmt.query().unwrap().collect_all().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .any(|r| r.get::<i64>(0).unwrap() == 7 && r.get::<String>(1).unwrap() == "hi")
        );
        assert!(
            rows.iter()
                .any(|r| r.get::<i64>(0).unwrap() == 8 && r.get::<String>(1).unwrap() == "yo")
        );
    }

    #[test]
    fn arity_mismatch_returns_clean_error() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (a INTEGER, b TEXT);").unwrap();
        let stmt = conn
            .prepare("SELECT * FROM t WHERE a = ? AND b = ?")
            .unwrap();
        let err = stmt.query_with_params(&[Value::Integer(1)]).unwrap_err();
        assert!(format!("{err}").contains("expected 2 parameter"));
    }

    #[test]
    fn run_and_query_reject_when_placeholders_present() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (a INTEGER);").unwrap();
        let mut stmt_select = conn.prepare("SELECT a FROM t WHERE a = ?").unwrap();
        let err = stmt_select.query().unwrap_err();
        assert!(format!("{err}").contains("query_with_params"));
        let err = stmt_select.run().unwrap_err();
        assert!(format!("{err}").contains("execute_with_params"));
    }

    #[test]
    fn null_param_compares_against_null() {
        // a = NULL is *false* in SQL three-valued logic; binding NULL
        // must match SQLite's behavior so callers can rely on the same
        // semantics.
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (a INTEGER);").unwrap();
        conn.execute("INSERT INTO t (a) VALUES (1);").unwrap();
        let stmt = conn.prepare("SELECT a FROM t WHERE a = ?").unwrap();
        let rows = stmt
            .query_with_params(&[Value::Null])
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows.len(), 0);
    }

    #[test]
    fn vector_param_substitutes_through_select() {
        // Non-HNSW path: a small VECTOR table + brute-force ORDER BY
        // exercises the substitution into the ORDER BY expression
        // and the bracket-array shape eval_expr_scope expects.
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE v (id INTEGER PRIMARY KEY, e VECTOR(3));")
            .unwrap();
        conn.execute("INSERT INTO v (id, e) VALUES (1, [1.0, 0.0, 0.0]);")
            .unwrap();
        conn.execute("INSERT INTO v (id, e) VALUES (2, [0.0, 1.0, 0.0]);")
            .unwrap();
        conn.execute("INSERT INTO v (id, e) VALUES (3, [0.0, 0.0, 1.0]);")
            .unwrap();

        let stmt = conn
            .prepare("SELECT id FROM v ORDER BY vec_distance_l2(e, ?) ASC LIMIT 1")
            .unwrap();
        let rows = stmt
            .query_with_params(&[Value::Vector(vec![1.0, 0.0, 0.0])])
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 1);
    }

    #[test]
    fn prepare_cached_reuses_plans() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (a INTEGER);").unwrap();
        for n in 1..=3 {
            conn.execute(&format!("INSERT INTO t (a) VALUES ({n});"))
                .unwrap();
        }

        // First call populates the cache; second hits the same entry.
        let _ = conn.prepare_cached("SELECT a FROM t WHERE a = ?").unwrap();
        let _ = conn.prepare_cached("SELECT a FROM t WHERE a = ?").unwrap();
        assert_eq!(conn.prepared_cache_len(), 1);

        // Distinct SQL widens the cache.
        let _ = conn.prepare_cached("SELECT a FROM t").unwrap();
        assert_eq!(conn.prepared_cache_len(), 2);
    }

    #[test]
    fn prepare_cached_evicts_when_over_capacity() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (a INTEGER);").unwrap();
        conn.set_prepared_cache_capacity(2);
        let _ = conn.prepare_cached("SELECT a FROM t").unwrap();
        let _ = conn.prepare_cached("SELECT a FROM t WHERE a = ?").unwrap();
        assert_eq!(conn.prepared_cache_len(), 2);
        // Third distinct SQL evicts the oldest entry (the FROM-only SELECT).
        let _ = conn.prepare_cached("SELECT a FROM t WHERE a > ?").unwrap();
        assert_eq!(conn.prepared_cache_len(), 2);
    }

    /// SQLR-23 — the headline VECTOR-binding case. With an HNSW index
    /// attached, the optimizer hook recognizes
    /// `ORDER BY vec_distance_l2(col, ?) LIMIT k` even when the second
    /// arg is a bound parameter, because substitution lowers
    /// `Value::Vector` into the same bracket-array shape an inline
    /// `[…]` literal produces. Self-query: querying for one of the
    /// corpus's own vectors must return that vector as the nearest.
    #[test]
    fn vector_bind_through_hnsw_optimizer() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE v (id INTEGER PRIMARY KEY, e VECTOR(4));")
            .unwrap();
        let corpus: [(i64, [f32; 4]); 5] = [
            (1, [1.0, 0.0, 0.0, 0.0]),
            (2, [0.0, 1.0, 0.0, 0.0]),
            (3, [0.0, 0.0, 1.0, 0.0]),
            (4, [0.0, 0.0, 0.0, 1.0]),
            (5, [0.5, 0.5, 0.5, 0.5]),
        ];
        for (id, vec) in corpus {
            conn.execute(&format!(
                "INSERT INTO v (id, e) VALUES ({id}, [{}, {}, {}, {}]);",
                vec[0], vec[1], vec[2], vec[3]
            ))
            .unwrap();
        }
        conn.execute("CREATE INDEX v_hnsw ON v USING hnsw (e);")
            .unwrap();

        let stmt = conn
            .prepare("SELECT id FROM v ORDER BY vec_distance_l2(e, ?) ASC LIMIT 1")
            .unwrap();
        // Query with id=3's vector — expect id=3 back.
        let rows = stmt
            .query_with_params(&[Value::Vector(vec![0.0, 0.0, 1.0, 0.0])])
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 3);

        // Query with id=1's vector — expect id=1.
        let rows = stmt
            .query_with_params(&[Value::Vector(vec![1.0, 0.0, 0.0, 0.0])])
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 1);
    }

    /// SQLR-28 — cosine probe: an HNSW index built `WITH (metric =
    /// 'cosine')` must serve `ORDER BY vec_distance_cosine(col, [...])`
    /// from the graph. Self-query: querying for one of the corpus's
    /// own vectors must come back as the nearest under cosine
    /// distance.
    #[test]
    fn cosine_self_query_through_hnsw_optimizer() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE v (id INTEGER PRIMARY KEY, e VECTOR(4));")
            .unwrap();
        let corpus: [(i64, [f32; 4]); 5] = [
            (1, [1.0, 0.0, 0.0, 0.0]),
            (2, [0.0, 1.0, 0.0, 0.0]),
            (3, [0.0, 0.0, 1.0, 0.0]),
            (4, [0.0, 0.0, 0.0, 1.0]),
            (5, [0.5, 0.5, 0.5, 0.5]),
        ];
        for (id, vec) in corpus {
            conn.execute(&format!(
                "INSERT INTO v (id, e) VALUES ({id}, [{}, {}, {}, {}]);",
                vec[0], vec[1], vec[2], vec[3]
            ))
            .unwrap();
        }
        conn.execute("CREATE INDEX v_hnsw ON v USING hnsw (e) WITH (metric = 'cosine');")
            .unwrap();

        // Self-query for id=2's vector — expected nearest under cosine
        // distance is id=2 itself (cos distance 0).
        let rows = conn
            .prepare("SELECT id FROM v ORDER BY vec_distance_cosine(e, [0.0, 1.0, 0.0, 0.0]) ASC LIMIT 1")
            .unwrap()
            .query_with_params(&[])
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 2);
    }

    /// SQLR-28 — dot probe: same shape as the cosine test, but the
    /// index is built `WITH (metric = 'dot')` and the query uses
    /// `vec_distance_dot`. Confirms the third metric variant lights up
    /// the graph shortcut, not just l2 / cosine.
    #[test]
    fn dot_self_query_through_hnsw_optimizer() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE v (id INTEGER PRIMARY KEY, e VECTOR(3));")
            .unwrap();
        // Data: distinguishable magnitudes so the dot metric resolves
        // a clear winner. `vec_distance_dot(a, b) = -(a·b)` — smaller
        // (more negative) is closer.
        let corpus: [(i64, [f32; 3]); 4] = [
            (1, [1.0, 0.0, 0.0]),
            (2, [2.0, 0.0, 0.0]),
            (3, [0.0, 1.0, 0.0]),
            (4, [0.0, 0.0, 1.0]),
        ];
        for (id, vec) in corpus {
            conn.execute(&format!(
                "INSERT INTO v (id, e) VALUES ({id}, [{}, {}, {}]);",
                vec[0], vec[1], vec[2]
            ))
            .unwrap();
        }
        conn.execute("CREATE INDEX v_hnsw ON v USING hnsw (e) WITH (metric = 'dot');")
            .unwrap();

        // Query [3, 0, 0]: dot products are 3, 6, 0, 0 → distances
        // -3, -6, 0, 0. id=2 has the smallest (most negative) distance.
        let rows = conn
            .prepare("SELECT id FROM v ORDER BY vec_distance_dot(e, [3.0, 0.0, 0.0]) ASC LIMIT 1")
            .unwrap()
            .query_with_params(&[])
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 2);
    }

    /// SQLR-28 — metric mismatch must NOT take the graph shortcut.
    /// An L2-built index queried with `vec_distance_cosine` falls
    /// through to brute-force, which still returns the correct
    /// answer. We confirm the answer is correct; the slow-path
    /// behaviour itself is implicit (no error, no panic, no wrong
    /// result), which is the user-visible contract that matters.
    #[test]
    fn metric_mismatch_falls_back_to_brute_force() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE v (id INTEGER PRIMARY KEY, e VECTOR(2));")
            .unwrap();
        let half_sqrt2 = std::f32::consts::FRAC_1_SQRT_2;
        let corpus: [(i64, [f32; 2]); 3] = [
            (1, [1.0, 0.0]),
            (2, [half_sqrt2, half_sqrt2]),
            (3, [0.0, 1.0]),
        ];
        for (id, vec) in corpus {
            conn.execute(&format!(
                "INSERT INTO v (id, e) VALUES ({id}, [{}, {}]);",
                vec[0], vec[1]
            ))
            .unwrap();
        }
        // Default L2 index — no WITH clause.
        conn.execute("CREATE INDEX v_hnsw_l2 ON v USING hnsw (e);")
            .unwrap();

        // Query with cosine. Index can't help; brute-force still
        // returns the correct nearest by cosine: id=1 (cos dist 0).
        let rows = conn
            .prepare("SELECT id FROM v ORDER BY vec_distance_cosine(e, [1.0, 0.0]) ASC LIMIT 1")
            .unwrap()
            .query_with_params(&[])
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 1);
    }

    /// SQLR-28 — a typo in the metric name must error at CREATE INDEX
    /// time. Falling back to L2 silently is the bug we're fixing here,
    /// not the behaviour to preserve.
    #[test]
    fn unknown_metric_name_is_rejected() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE v (id INTEGER PRIMARY KEY, e VECTOR(2));")
            .unwrap();
        let err = conn
            .execute("CREATE INDEX bad ON v USING hnsw (e) WITH (metric = 'cosin');")
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unknown HNSW metric"), "got: {msg}");
    }

    /// SQLR-28 — WITH options on a non-HNSW index must error rather
    /// than be silently ignored. An option that has no effect on the
    /// resulting index is a footgun.
    #[test]
    fn with_metric_on_btree_is_rejected() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT);")
            .unwrap();
        let err = conn
            .execute("CREATE INDEX bad ON t (b) WITH (metric = 'cosine');")
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("doesn't support any options"), "got: {msg}");
    }

    #[test]
    fn prepare_cached_executes_the_same_as_prepare() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT);")
            .unwrap();
        let mut ins = conn
            .prepare_cached("INSERT INTO t (a, b) VALUES (?, ?)")
            .unwrap();
        ins.execute_with_params(&[Value::Integer(1), Value::Text("alpha".into())])
            .unwrap();
        ins.execute_with_params(&[Value::Integer(2), Value::Text("beta".into())])
            .unwrap();

        let stmt = conn.prepare_cached("SELECT b FROM t WHERE a = ?").unwrap();
        let rows = stmt
            .query_with_params(&[Value::Integer(2)])
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<String>(0).unwrap(), "beta");
    }
}
