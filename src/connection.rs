//! Public `Connection` / `Statement` / `Rows` / `Row` API (Phase 5a).
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

use std::path::Path;

use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;

use crate::error::{Result, SQLRiteError};
use crate::sql::db::database::Database;
use crate::sql::db::table::Value;
use crate::sql::executor::execute_select_rows;
use crate::sql::pager::{AccessMode, open_database_with_mode, save_database};
use crate::sql::parser::select::SelectQuery;
use crate::sql::process_command;

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
        Ok(Self { db })
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
        Ok(Self { db })
    }

    /// Opens a transient in-memory database. No file is touched and no
    /// locks are taken; state lives for the lifetime of the
    /// `Connection` and is discarded on drop.
    pub fn open_in_memory() -> Result<Self> {
        Ok(Self {
            db: Database::new("memdb".to_string()),
        })
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
        process_command(sql, &mut self.db)
    }

    /// Prepares a statement for repeated execution or row iteration.
    /// The SQL is parsed once and validated; the resulting
    /// [`Statement`] can be executed multiple times. Today this is
    /// primarily useful for SELECT (to reach the typed-row API);
    /// parameter binding and prepared-plan caching are future work.
    pub fn prepare<'c>(&'c mut self, sql: &str) -> Result<Statement<'c>> {
        Statement::new(self, sql)
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
            .finish()
    }
}

/// A prepared statement bound to a specific connection lifetime.
/// Today this is a thin wrapper around the raw SQL; Phase 5's cursor
/// work will grow it into a real prepared-plan cache.
pub struct Statement<'c> {
    conn: &'c mut Connection,
    sql: String,
    kind: StatementKind,
}

enum StatementKind {
    Select(SelectQuery),
    Other,
}

impl std::fmt::Debug for Statement<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Statement")
            .field("sql", &self.sql)
            .field(
                "kind",
                &match self.kind {
                    StatementKind::Select(_) => "Select",
                    StatementKind::Other => "Other",
                },
            )
            .finish()
    }
}

impl<'c> Statement<'c> {
    fn new(conn: &'c mut Connection, sql: &str) -> Result<Self> {
        // Parse once at prepare time so syntax errors surface early.
        let dialect = SQLiteDialect {};
        let mut ast = Parser::parse_sql(&dialect, sql).map_err(SQLRiteError::from)?;
        let Some(stmt) = ast.pop() else {
            return Err(SQLRiteError::General("no statement to prepare".to_string()));
        };
        if !ast.is_empty() {
            return Err(SQLRiteError::General(
                "prepare() accepts a single statement; found more than one".to_string(),
            ));
        }
        let kind = match &stmt {
            sqlparser::ast::Statement::Query(_) => StatementKind::Select(SelectQuery::new(&stmt)?),
            _ => StatementKind::Other,
        };
        Ok(Self {
            conn,
            sql: sql.to_string(),
            kind,
        })
    }

    /// Executes a prepared non-query statement. Equivalent to
    /// [`Connection::execute`] — included for parity with the
    /// typed-row `query()` so callers who want `Statement::run` / `Statement::query`
    /// symmetry get it.
    pub fn run(&mut self) -> Result<String> {
        self.conn.execute(&self.sql)
    }

    /// Runs a SELECT and returns a [`Rows`] iterator over typed rows.
    /// Errors if the prepared statement isn't a SELECT.
    pub fn query(&self) -> Result<Rows> {
        match &self.kind {
            StatementKind::Select(sq) => {
                let result = execute_select_rows(sq.clone(), &self.conn.db)?;
                Ok(Rows {
                    columns: result.columns,
                    rows: result.rows.into_iter(),
                })
            }
            StatementKind::Other => Err(SQLRiteError::General(
                "query() only works on SELECT statements; use run() for DDL/DML".to_string(),
            )),
        }
    }

    /// Column names this statement will produce, in projection order.
    /// `None` for non-SELECT statements.
    pub fn column_names(&self) -> Option<Vec<String>> {
        match &self.kind {
            StatementKind::Select(_) => {
                // We can't know the concrete column list without
                // running the query (it depends on the table schema
                // and the projection). Callers who need it up front
                // should call query() and inspect Rows::columns.
                None
            }
            StatementKind::Other => None,
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
}
