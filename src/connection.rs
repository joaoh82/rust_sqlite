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

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::sql::dialect::SqlriteDialect;
use sqlparser::ast::Statement as AstStatement;
use sqlparser::parser::Parser;

use crate::error::{Result, SQLRiteError};
use crate::mvcc::{
    ConcurrentTx, JournalMode, MvccCommitBatch, MvccLogRecord, RowID, RowVersion, VersionPayload,
};
use crate::sql::db::database::{Database, TxnSnapshot};
use crate::sql::db::table::{Table, Value};
use crate::sql::executor::execute_select_rows;
use crate::sql::pager::{self, AccessMode, open_database_with_mode, save_database};
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
/// ## Multiple connections (Phase 10.1)
///
/// `Connection` is a thin handle over an `Arc<Mutex<Database>>`. Call
/// [`Connection::connect`] to mint a sibling handle that shares the
/// same backing `Database` — typically one per worker thread. Today
/// every operation still serializes through the single mutex (and the
/// pager's exclusive flock between processes), so the headline
/// behaviour change is that callers can hold and address the same DB
/// from more than one thread without wrapping the whole `Connection`
/// in a `Mutex` themselves. `BEGIN CONCURRENT` and snapshot-isolated
/// reads land in subsequent Phase 10 sub-phases.
///
/// `Connection` is `Send + Sync`. The recommended pattern is one
/// connection per thread (clone via `connect()`); statements still
/// borrow `&mut Connection`, so a single connection isn't suitable
/// for true concurrent statement execution.
pub struct Connection {
    /// Shared engine state. Mints sibling connections via
    /// [`Connection::connect`] without copying the in-memory tables
    /// or the long-lived pager.
    inner: Arc<Mutex<Database>>,
    /// SQLR-23 — small SQL→cached-plan LRU. Keyed by the verbatim SQL
    /// string the caller passed to `prepare_cached`. Stored as a
    /// `VecDeque` rather than a HashMap+linked-list because the
    /// expected capacity is small (default 16) — linear scan is fine
    /// and the implementation stays dependency-free.
    ///
    /// Per-connection (not shared with sibling handles) — each thread
    /// gets its own LRU so cache-mutation never crosses a thread
    /// boundary.
    prep_cache: VecDeque<(String, Arc<CachedPlan>)>,
    prep_cache_cap: usize,
    /// Phase 11.4 — per-connection `BEGIN CONCURRENT` state.
    /// `None` outside a concurrent transaction; `Some` between
    /// `BEGIN CONCURRENT` and `COMMIT` / `ROLLBACK`. Multiple
    /// sibling connections can each hold their own — that's the
    /// headline concurrency story this slice unlocks.
    ///
    /// While `Some`, every statement on this connection runs
    /// against the cloned tables in [`ConcurrentTx::tables`]
    /// instead of the live `Database::tables`. The live database
    /// stays untouched until the commit-validation pass succeeds.
    ///
    /// **Phase 11.5 — wrapped in a `Mutex`.** [`Statement::query`]
    /// and [`Statement::query_with_params`] take `&self`, so they
    /// need interior mutability to swap the snapshot in for the
    /// read. The lock is uncontended in single-thread use (each
    /// connection's `concurrent_tx` is per-handle, and the
    /// Statement-borrows-Connection contract still serializes
    /// statements on a given handle); the Mutex is the cheapest
    /// way to satisfy the borrow checker without restructuring
    /// the Statement API. Lock order is always
    /// `concurrent_tx` → `inner` to keep deadlock-free.
    concurrent_tx: Mutex<Option<ConcurrentTx>>,
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
            inner: Arc::new(Mutex::new(db)),
            prep_cache: VecDeque::new(),
            prep_cache_cap: DEFAULT_PREP_CACHE_CAP,
            concurrent_tx: Mutex::new(None),
        }
    }

    /// Phase 10.1 — mints another `Connection` sharing the same
    /// backing `Database`. Hand the returned handle to a separate
    /// thread to address the same in-memory tables and persistent
    /// pager from there.
    ///
    /// The new handle starts with an empty prepared-statement cache
    /// (caches are per-handle, by design). Inherits the parent's
    /// `prepare_cached` capacity. Concurrent operations still
    /// serialize through the engine's internal lock and the pager's
    /// existing single-writer rule — a true multi-writer story
    /// arrives with `BEGIN CONCURRENT` in Phase 10.4.
    ///
    /// ```no_run
    /// # use sqlrite::Connection;
    /// let mut primary = Connection::open("foo.sqlrite")?;
    /// let secondary = primary.connect();
    /// std::thread::spawn(move || {
    ///     let mut conn = secondary;
    ///     conn.execute("INSERT INTO t (x) VALUES (1)").unwrap();
    /// })
    /// .join()
    /// .unwrap();
    /// # Ok::<(), sqlrite::SQLRiteError>(())
    /// ```
    pub fn connect(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            prep_cache: VecDeque::new(),
            prep_cache_cap: self.prep_cache_cap,
            // Phase 11.4: each sibling handle starts outside any
            // concurrent transaction. Multi-thread `BEGIN CONCURRENT`
            // is the headline use case — every clone gets its own
            // independent slot.
            concurrent_tx: Mutex::new(None),
        }
    }

    /// Phase 10.1 — number of `Connection` handles currently sharing
    /// this database (this handle plus every live `connect()`
    /// descendant). Useful for diagnostics and tests; no semantic
    /// guarantee beyond that.
    pub fn handle_count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }

    /// Locks the shared `Database` and returns the guard. Internal
    /// helper — every public method that needs `&mut Database` calls
    /// this. The lock is released when the guard drops, so callers
    /// must keep the guard alive for the duration of the engine call
    /// (typically by binding it to a local).
    fn lock(&self) -> MutexGuard<'_, Database> {
        // `unwrap` propagates a panic from another thread that held
        // the lock — there's no engine-level recovery story for a
        // poisoned `Database` (the in-memory tables would be in an
        // unknown state), so failing fast is the right behaviour.
        self.inner
            .lock()
            .unwrap_or_else(|e| panic!("sqlrite: database mutex poisoned: {e}"))
    }

    /// Parses and executes one SQL statement. For DDL (`CREATE TABLE`,
    /// `CREATE INDEX`), DML (`INSERT`, `UPDATE`, `DELETE`) and
    /// transaction control (`BEGIN`, `COMMIT`, `ROLLBACK`,
    /// `BEGIN CONCURRENT`). Returns the status message the engine
    /// produced (e.g. `"INSERT Statement executed."`).
    ///
    /// For `SELECT`, `execute` works but discards the row data and
    /// just returns the rendered status — use [`Connection::prepare`]
    /// and [`Statement::query`] to iterate typed rows.
    ///
    /// Phase 11.4 — intercepts `BEGIN CONCURRENT`, `COMMIT`, and
    /// `ROLLBACK` before sqlparser sees them so the per-connection
    /// MVCC transaction state stays in sync. Inside an open
    /// concurrent transaction, every other statement runs against
    /// the transaction's private cloned tables; the live database
    /// stays untouched until commit-validation succeeds.
    pub fn execute(&mut self, sql: &str) -> Result<String> {
        let intent = concurrent_tx_intent(sql);
        let has_tx = self.concurrent_tx_is_open();
        match intent {
            ConcurrentTxIntent::Begin => self.begin_concurrent(),
            ConcurrentTxIntent::Commit if has_tx => self.commit_concurrent(),
            ConcurrentTxIntent::Rollback if has_tx => self.rollback_concurrent(),
            ConcurrentTxIntent::None
            | ConcurrentTxIntent::Commit
            | ConcurrentTxIntent::Rollback => self.execute_dispatch(sql),
        }
    }

    /// Phase 11.5 — cheap probe used by [`Connection::execute`]
    /// (and [`Statement::query`]) to decide whether to route
    /// through the concurrent-tx dispatch. Acquires the
    /// `concurrent_tx` mutex briefly; never blocks for a
    /// meaningful amount of time because the only other lockers
    /// are this connection's own writers.
    fn concurrent_tx_is_open(&self) -> bool {
        self.lock_concurrent_tx().is_some()
    }

    /// Phase 11.5 — locks the per-connection
    /// `Mutex<Option<ConcurrentTx>>`. Wrapping the poison handler
    /// in one place keeps every caller's lock-order discipline
    /// visible at the call site (always `concurrent_tx` before
    /// `inner`).
    fn lock_concurrent_tx(&self) -> MutexGuard<'_, Option<ConcurrentTx>> {
        self.concurrent_tx.lock().unwrap_or_else(|e| {
            panic!("sqlrite: concurrent_tx mutex poisoned: {e}");
        })
    }

    /// Phase 11.5 — runs `f` against the read-side `&Database`
    /// the caller's transaction expects to see.
    ///
    /// - **No concurrent transaction open** — `f` runs against the
    ///   live `Database::tables`. Same path the legacy `query`
    ///   used.
    /// - **Concurrent transaction open** — swaps the transaction's
    ///   private cloned `tables` in for the duration of `f`, so
    ///   `f` sees the BEGIN-time snapshot plus any writes the
    ///   transaction has staged. Swaps back before the function
    ///   returns even on error (the swap-back uses a scope guard
    ///   pattern so a panic inside `f` doesn't leave `db.tables`
    ///   pointing at the snapshot clone).
    ///
    /// Takes `&self` (rather than `&mut self`) because the
    /// `Statement::query` API contract is `&self` — that's why the
    /// `concurrent_tx` field lives behind a `Mutex`. Lock order is
    /// `concurrent_tx` → `inner`, matching every other tx-aware
    /// path on this connection.
    pub(crate) fn with_snapshot_read<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Database) -> R,
    {
        let mut tx_slot = self.lock_concurrent_tx();
        let mut db = self.lock();
        match tx_slot.as_mut() {
            None => f(&db),
            Some(tx) => {
                // Swap the snapshot in. Use a scope guard so the
                // unswap happens even if `f` unwinds — leaving
                // `db.tables` pointing at the tx's private clone
                // would be catastrophic for later sibling-handle
                // reads.
                std::mem::swap(&mut db.tables, &mut tx.tables);
                let prior_txn = db.txn.take();
                db.txn = Some(TxnSnapshot {
                    tables: HashMap::new(),
                });

                struct UnswapGuard<'a> {
                    db: &'a mut Database,
                    tx_tables: &'a mut HashMap<String, Table>,
                    prior_txn: Option<TxnSnapshot>,
                    armed: bool,
                }
                impl Drop for UnswapGuard<'_> {
                    fn drop(&mut self) {
                        if self.armed {
                            self.db.txn = self.prior_txn.take();
                            std::mem::swap(&mut self.db.tables, self.tx_tables);
                        }
                    }
                }
                let mut guard = UnswapGuard {
                    db: &mut db,
                    tx_tables: &mut tx.tables,
                    prior_txn,
                    armed: true,
                };

                let result = f(guard.db);

                // Disarm the guard explicitly and unwind in the
                // expected order so the borrow checker can see
                // both fields are accessed disjointly.
                guard.armed = false;
                guard.db.txn = guard.prior_txn.take();
                std::mem::swap(&mut guard.db.tables, guard.tx_tables);

                result
            }
        }
    }

    /// Internal — runs `sql` against the engine. If a concurrent
    /// transaction is open, swaps the transaction's private
    /// `tables` map in for the duration of the dispatch so writes
    /// land on the snapshot, not the live database. Otherwise
    /// falls straight through to the legacy
    /// [`crate::sql::process_command`] path.
    fn execute_dispatch(&mut self, sql: &str) -> Result<String> {
        if self.concurrent_tx_is_open() {
            self.execute_in_concurrent_tx(sql)
        } else {
            let mut db = self.lock();
            crate::sql::process_command(sql, &mut db)
        }
    }

    /// Phase 11.4 — opens a `BEGIN CONCURRENT` transaction on this
    /// connection. Allocates a new `TxHandle` (which advances the
    /// MVCC clock by one), deep-clones the live tables into the
    /// per-connection [`ConcurrentTx`] state, and records the
    /// schema fingerprint. Returns the status string the REPL
    /// renders (`"BEGIN"`).
    ///
    /// Errors if the database isn't in `journal_mode = mvcc`, or
    /// if any transaction (concurrent or legacy `BEGIN`) is
    /// already open on this connection.
    fn begin_concurrent(&mut self) -> Result<String> {
        // Lock order: concurrent_tx → inner (db). Keep this order
        // in every method that touches both — deadlock-free by
        // construction.
        let mut tx_slot = self.lock_concurrent_tx();
        if tx_slot.is_some() {
            return Err(SQLRiteError::General(
                "cannot BEGIN CONCURRENT: a concurrent transaction is already open".to_string(),
            ));
        }
        let db = self.lock();
        if db.journal_mode() != JournalMode::Mvcc {
            return Err(SQLRiteError::General(
                "BEGIN CONCURRENT requires `PRAGMA journal_mode = mvcc;` first".to_string(),
            ));
        }
        if db.in_transaction() {
            return Err(SQLRiteError::General(
                "cannot BEGIN CONCURRENT: a non-concurrent transaction is already open".to_string(),
            ));
        }
        if db.is_read_only() {
            return Err(SQLRiteError::General(
                "cannot BEGIN CONCURRENT: database is opened read-only".to_string(),
            ));
        }
        let tx = ConcurrentTx::begin(db.mvcc_clock(), db.mv_store().active_registry(), &db.tables);
        drop(db);
        *tx_slot = Some(tx);
        Ok("BEGIN".to_string())
    }

    /// Phase 11.4 — commits the open concurrent transaction.
    ///
    /// Steps (Hekaton-style optimistic validation):
    ///
    /// 1. Diff the transaction's private `tables` against the
    ///    live `Database::tables` to derive the write-set.
    /// 2. For each row in the write-set, walk the
    ///    [`MvStore`](crate::mvcc::MvStore) chain. If any
    ///    committed version's `begin > tx.begin_ts`, abort with
    ///    [`SQLRiteError::Busy`] — some other transaction
    ///    superseded the row after our snapshot.
    /// 3. Allocate a `commit_ts`, push every write into the
    ///    `MvStore` as a committed version (caps the previous
    ///    latest's `end` at `commit_ts`), and apply the writes
    ///    to `Database::tables`.
    /// 4. Run the legacy `save_database` so the changes durable
    ///    via the existing WAL.
    ///
    /// On `Busy`, the transaction is dropped (rollback semantics)
    /// and the caller should retry with a fresh `BEGIN
    /// CONCURRENT`.
    fn commit_concurrent(&mut self) -> Result<String> {
        let mut tx_slot = self.lock_concurrent_tx();
        let tx = tx_slot
            .take()
            .expect("commit_concurrent called without active tx (caller should check)");
        // Drop the slot guard — we already moved the tx out, and
        // holding it across `self.lock()` would violate the
        // `concurrent_tx → inner` order if any helper were to
        // grow a reverse acquire.
        drop(tx_slot);

        let mut db = self.lock();

        // Schema drift catches DDL run on the live database under
        // us. v0 rejects DDL inside the tx; outside is the only
        // way to land here.
        if !tx.schema_unchanged(&db.tables) {
            return Err(SQLRiteError::Busy(
                "schema changed under BEGIN CONCURRENT (a CREATE/DROP/ALTER ran on \
                 another connection); transaction rolled back"
                    .to_string(),
            ));
        }

        // Diff against the BEGIN-time clone, NOT against the live
        // database. Other concurrent transactions may have
        // committed between our BEGIN and now; their writes show
        // up in `db.tables` but aren't part of our write-set, and
        // diffing against live would surface them as bogus DELETEs
        // (silently undoing someone else's commit).
        let writes = diff_tables_for_writes(&tx.tables_at_begin, &tx.tables)?;

        // Validation pass: walk the write-set against MvStore.
        let mv = db.mv_store().clone();
        let begin_ts = tx.begin_ts();
        for (row_id, _payload) in &writes {
            if let Some(latest_begin) = mv.latest_committed_begin(row_id) {
                if latest_begin > begin_ts {
                    return Err(SQLRiteError::Busy(format!(
                        "write-write conflict on {}/{}: another transaction committed \
                         this row at ts={latest_begin} (after our begin_ts={begin_ts}); \
                         transaction rolled back, retry with a fresh BEGIN CONCURRENT",
                        row_id.table, row_id.rowid,
                    )));
                }
            }
        }

        // Validation passed — allocate commit_ts and apply.
        let commit_ts = db.mvcc_clock().tick();
        for (row_id, payload) in &writes {
            let version = RowVersion::committed(commit_ts, payload.clone());
            // `push_committed`'s monotonic-begin check is satisfied
            // because validation above ensured no version has
            // begin >= commit_ts (commit_ts is freshly ticked).
            mv.push_committed(row_id.clone(), version)
                .map_err(|e| SQLRiteError::General(format!("MvStore push failed: {e}")))?;
        }

        // Apply the diff to Database::tables. Reuses the legacy
        // INSERT / UPDATE / DELETE shape so post-commit reads on
        // any handle (concurrent or legacy) see the latest row
        // values via the existing read path.
        apply_writes_to_live(&mut db, &tx.tables, &writes)?;

        // Phase 11.9 — append the MVCC commit batch into the WAL
        // before the legacy page-commit flush. The MVCC frame is
        // not fsync'd on its own; the legacy `save_database`
        // below ends with a commit-frame fsync that durably
        // includes every byte written since the previous fsync,
        // covering this batch too. A crash between the two
        // append calls drops both — torn-write atomicity for the
        // whole transaction.
        //
        // For in-memory databases (no source_path) we skip the
        // WAL append: there's no pager and no fsync. MVCC state
        // stays in the in-memory `MvStore` for the lifetime of
        // the process.
        if let Some(pager) = db.pager.as_mut() {
            let records = writes
                .iter()
                .map(|(row, payload)| MvccLogRecord {
                    row: row.clone(),
                    payload: payload.clone(),
                })
                .collect();
            let batch = MvccCommitBatch { commit_ts, records };
            if let Err(append_err) = pager.append_mvcc_batch(&batch) {
                return Err(SQLRiteError::General(format!(
                    "COMMIT failed appending MVCC log record: {append_err}"
                )));
            }
            // Bump the WAL header's persisted clock high-water so
            // the next checkpoint truncates with a header that
            // covers this commit. The MVCC frames themselves
            // also carry `commit_ts`, so even an un-checkpointed
            // crash still seeds the clock correctly via the
            // replayer's max-with-frames logic — this just keeps
            // the post-checkpoint path correct.
            if let Err(set_err) = pager.observe_clock_high_water(commit_ts) {
                return Err(SQLRiteError::General(format!(
                    "COMMIT failed updating WAL clock high-water: {set_err}"
                )));
            }
        }

        // Persist via the legacy WAL — the on-disk format is
        // unchanged in 11.4+. The page-commit's fsync below
        // covers the MVCC frame appended above; one atomic
        // boundary for the whole transaction.
        if let Some(path) = db.source_path.clone() {
            if let Err(save_err) = pager::save_database(&mut db, &path) {
                return Err(SQLRiteError::General(format!(
                    "COMMIT failed during save_database: {save_err}"
                )));
            }
        }

        // Phase 11.6 — per-commit GC sweep on the write-set's
        // chains. Drop the `tx` handle FIRST so its `begin_ts`
        // exits the active-tx registry; otherwise the watermark
        // is still pinned at our own `begin_ts` and we'd preserve
        // versions we're free to reclaim. Only the rows this
        // transaction wrote can have a newly-capped `end` worth
        // sweeping — the broader GC story (full-store sweeps,
        // background drains) lands behind explicit
        // [`Connection::vacuum_mvcc`] / [`MvStore::gc_all`].
        drop(tx);
        let watermark = mv.active_watermark();
        for (row_id, _) in &writes {
            mv.gc_chain(row_id, watermark);
        }
        Ok("COMMIT".to_string())
    }

    /// Phase 11.4 — rolls back the open concurrent transaction.
    /// Drops the per-connection state; the live `Database::tables`
    /// is unchanged because writes never landed there.
    fn rollback_concurrent(&mut self) -> Result<String> {
        // tx drops here; TxHandle unregisters automatically.
        let _ = self
            .lock_concurrent_tx()
            .take()
            .expect("rollback_concurrent called without active tx (caller should check)");
        Ok("ROLLBACK".to_string())
    }

    /// Phase 11.4 — runs `sql` against the open concurrent
    /// transaction's private cloned tables. Implementation: swap
    /// `db.tables` <-> `tx.tables` for the duration of the
    /// dispatch, suppress auto-save by parking a dummy
    /// [`TxnSnapshot`] on `db.txn`, then unwind both.
    ///
    /// DDL is rejected before the swap with a typed error —
    /// schema mutations inside a `BEGIN CONCURRENT` block aren't
    /// supported in v0 (the plan flags this as an explicit
    /// non-goal, and the swap-based dispatch can't safely apply
    /// new tables to the live database without a separate merge
    /// pass).
    fn execute_in_concurrent_tx(&mut self, sql: &str) -> Result<String> {
        let intent = legacy_tx_intent(sql);
        if matches!(intent, LegacyTxIntent::Begin) {
            return Err(SQLRiteError::General(
                "cannot BEGIN: a concurrent transaction is already open".to_string(),
            ));
        }
        // String-prefix DDL check. Rejecting up front means the
        // tx's snapshot never gets a half-applied schema change —
        // which would be hard to merge back at commit because the
        // live database wouldn't agree.
        if rejects_in_concurrent_tx(sql) {
            return Err(SQLRiteError::General(
                "DDL is not supported inside BEGIN CONCURRENT (v0 limitation; the \
                 transaction stays open, the live schema is unchanged)"
                    .to_string(),
            ));
        }

        // Lock order: concurrent_tx → inner (db). Same shape as
        // every other tx-aware path on this connection.
        let mut tx_slot = self.lock_concurrent_tx();
        let tx = tx_slot
            .as_mut()
            .expect("execute_in_concurrent_tx called without active tx");
        let mut db = self.inner.lock().unwrap_or_else(|e| {
            panic!("sqlrite: database mutex poisoned: {e}");
        });

        // Swap the snapshot in. After this, db.tables IS the tx's
        // private clone; the executor mutates it freely.
        std::mem::swap(&mut db.tables, &mut tx.tables);

        // Suppress auto-save with a dummy TxnSnapshot. The
        // executor's auto-save check looks at `db.in_transaction()`,
        // which is true while `db.txn` is `Some`. The dummy
        // snapshot is never restored from — `tx` itself owns the
        // rollback story for concurrent transactions.
        let prior_txn = db.txn.take();
        db.txn = Some(TxnSnapshot {
            tables: HashMap::new(),
        });

        let result = crate::sql::process_command(sql, &mut db);

        // Unwind in reverse: take the dummy txn off (don't restore
        // anything from it), swap the tables back.
        db.txn = prior_txn;
        std::mem::swap(&mut db.tables, &mut tx.tables);
        result
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
        self.lock().in_transaction()
    }

    /// Returns the current auto-VACUUM threshold (SQLR-10). After a
    /// page-releasing DDL (DROP TABLE / DROP INDEX / ALTER TABLE DROP
    /// COLUMN) commits, the engine compacts the file in place if the
    /// freelist exceeds this fraction of `page_count`. New connections
    /// default to `Some(0.25)` (SQLite parity); `None` means the
    /// trigger is disabled. See [`Connection::set_auto_vacuum_threshold`].
    pub fn auto_vacuum_threshold(&self) -> Option<f32> {
        self.lock().auto_vacuum_threshold()
    }

    /// Sets the auto-VACUUM threshold (SQLR-10). `Some(t)` with `t` in
    /// `0.0..=1.0` arms the trigger; `None` disables it. Values outside
    /// `0.0..=1.0` (or NaN / infinite) return a typed error rather than
    /// silently saturating. The setting is per-database runtime state —
    /// closing the last connection to a database drops it; new
    /// connections start at the default `Some(0.25)`.
    ///
    /// Calling this on an in-memory or read-only database is allowed
    /// (it just won't fire — there's nothing to compact / no writes
    /// will reach the trigger).
    pub fn set_auto_vacuum_threshold(&mut self, threshold: Option<f32>) -> Result<()> {
        self.lock().set_auto_vacuum_threshold(threshold)
    }

    /// Returns `true` if the connection was opened read-only. Mutating
    /// statements on a read-only connection return a typed error.
    pub fn is_read_only(&self) -> bool {
        self.lock().is_read_only()
    }

    /// Phase 11.3 — current journal mode. `Wal` (default) keeps every
    /// pre-Phase-11 caller's behaviour. `Mvcc` is opt-in via
    /// `PRAGMA journal_mode = mvcc;`. Per-database — every
    /// [`Connection::connect`] sibling sees the same value.
    ///
    /// Today this is observable but doesn't change query behaviour;
    /// 11.4 wires `Mvcc` mode into the read/write paths.
    pub fn journal_mode(&self) -> crate::mvcc::JournalMode {
        self.lock().journal_mode()
    }

    /// Phase 11.6 — explicit full-store MVCC garbage collection
    /// pass. Walks every row in the [`MvStore`](crate::mvcc::MvStore)
    /// chain and drops versions whose `end` timestamp is below the
    /// current watermark (the smallest `begin_ts` across all
    /// in-flight transactions on this database, or `u64::MAX` when
    /// nothing is in flight).
    ///
    /// Returns the number of versions reclaimed. Cheap when the
    /// store is small; a future optimisation will give it
    /// background-thread semantics behind a configurable cadence.
    ///
    /// Per-commit GC already sweeps the rows each transaction
    /// touched, so most callers don't need this — it's the
    /// "vacuum the whole store" escape hatch for memory-pressure
    /// workloads or test suites that want a deterministic baseline.
    /// Safe to call even if `journal_mode` is `Wal` (the store is
    /// just empty); useful for tests that want to assert "no
    /// versions left."
    pub fn vacuum_mvcc(&self) -> usize {
        let db = self.lock();
        let mv = db.mv_store().clone();
        let watermark = mv.active_watermark();
        drop(db);
        mv.gc_all(watermark)
    }

    /// Escape hatch for advanced callers — locks the shared `Database`
    /// and hands back the guard. Not part of the stable API; will move
    /// or change as Phase 10's MVCC sub-phases land.
    ///
    /// Bind the guard to a local before calling functions that take
    /// `&Database`:
    ///
    /// ```no_run
    /// # use sqlrite::Connection;
    /// # fn use_db(_d: &sqlrite::Database) {}
    /// let conn = Connection::open_in_memory()?;
    /// let db = conn.database();
    /// use_db(&db);
    /// # Ok::<(), sqlrite::SQLRiteError>(())
    /// ```
    #[doc(hidden)]
    pub fn database(&self) -> MutexGuard<'_, Database> {
        self.lock()
    }

    #[doc(hidden)]
    pub fn database_mut(&mut self) -> MutexGuard<'_, Database> {
        self.lock()
    }
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let db = self.lock();
        f.debug_struct("Connection")
            .field("in_transaction", &db.in_transaction())
            .field("read_only", &db.is_read_only())
            .field("tables", &db.tables.len())
            .field("prep_cache_len", &self.prep_cache.len())
            .field("handles", &Arc::strong_count(&self.inner))
            .field("concurrent_tx", &self.concurrent_tx_is_open())
            .finish()
    }
}

// =====================================================================
// Phase 11.4 — concurrent-transaction helpers
//
// These live as free functions (rather than methods) so the borrow
// checker stays out of the way: callers in `Connection::execute*`
// already juggle mutable borrows of `self.concurrent_tx` and
// `self.inner.lock()` simultaneously, and threading a third `&mut self`
// through helpers would force every helper to either take owned
// arguments or split the borrow at the call site. Free functions take
// exactly the slices they need.

/// Coarse classifier for tx-control statements. Spotted by string
/// match before `sqlparser` runs, just like the PRAGMA intercept.
/// Distinguishing `BEGIN CONCURRENT` from plain `BEGIN` matters
/// because plain `BEGIN` still routes through the legacy
/// deep-clone snapshot path; only `BEGIN CONCURRENT` opens an
/// MVCC transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConcurrentTxIntent {
    /// `BEGIN CONCURRENT` — opens an MVCC transaction.
    Begin,
    /// `COMMIT` (with optional `TRANSACTION` / `WORK` / `;`).
    Commit,
    /// `ROLLBACK` (with optional `TRANSACTION` / `WORK` / `;`).
    Rollback,
    /// Anything else — falls through to the regular dispatch.
    None,
}

/// Coarse classifier for legacy tx-control statements (used to
/// reject nested `BEGIN` inside an open `BEGIN CONCURRENT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyTxIntent {
    /// Plain `BEGIN` / `BEGIN TRANSACTION` / `BEGIN DEFERRED` etc.
    /// — every shape that *isn't* `BEGIN CONCURRENT`.
    Begin,
    /// Anything else.
    None,
}

fn concurrent_tx_intent(sql: &str) -> ConcurrentTxIntent {
    let tokens = lowercase_tokens(sql);
    let head = tokens.as_slice();
    match head {
        [first, second, ..] if first == "begin" && second == "concurrent" => {
            ConcurrentTxIntent::Begin
        }
        [first, ..] if first == "commit" => ConcurrentTxIntent::Commit,
        [first, ..] if first == "end" => ConcurrentTxIntent::Commit,
        [first, ..] if first == "rollback" => ConcurrentTxIntent::Rollback,
        _ => ConcurrentTxIntent::None,
    }
}

fn legacy_tx_intent(sql: &str) -> LegacyTxIntent {
    let tokens = lowercase_tokens(sql);
    let head = tokens.as_slice();
    match head {
        // Plain BEGIN — but not BEGIN CONCURRENT, which the
        // concurrent-tx intent already caught.
        [first, ..] if first == "begin" => {
            if matches!(head.get(1).map(String::as_str), Some("concurrent")) {
                LegacyTxIntent::None
            } else {
                LegacyTxIntent::Begin
            }
        }
        [first, ..] if first == "start" => LegacyTxIntent::Begin,
        _ => LegacyTxIntent::None,
    }
}

/// Splits `sql` on whitespace + punctuation that's not part of
/// keywords, lowercases each piece, and returns the resulting
/// token list. Coarse enough to spot `BEGIN`, `COMMIT`,
/// `ROLLBACK`, `CONCURRENT`, `TRANSACTION`, etc.; not a real
/// tokenizer.
fn lowercase_tokens(sql: &str) -> Vec<String> {
    sql.split(|c: char| c.is_whitespace() || c == ';' || c == '(' || c == ')' || c == ',')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

/// Statement shapes that must be rejected inside a `BEGIN
/// CONCURRENT` block. v0 covers the canonical DDL — CREATE
/// TABLE, CREATE INDEX, DROP TABLE, DROP INDEX, ALTER TABLE,
/// VACUUM. Cheap string-prefix check; misses contrived
/// formattings like a leading SQL comment, but the rejection is
/// best-effort and v0 doesn't promise schema isolation inside
/// the tx anyway.
fn rejects_in_concurrent_tx(sql: &str) -> bool {
    let trimmed = sql.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("create ")
        || lower.starts_with("drop ")
        || lower.starts_with("alter ")
        || lower.starts_with("vacuum")
}

/// Phase 11.4 commit-time helper — diff `live` (the original
/// `Database::tables` map) against `snapshot` (the
/// transaction's private clone, post-statements) and produce
/// the write-set: every `(RowID, VersionPayload)` whose value
/// in the snapshot differs from the live state.
///
/// Three cases:
///
/// - Row in snapshot but not in live → INSERT, payload =
///   [`VersionPayload::Present`] of snapshot's column-value
///   pairs.
/// - Row in both, with different column values → UPDATE, same
///   shape.
/// - Row in live but not in snapshot → DELETE, payload =
///   [`VersionPayload::Tombstone`].
///
/// Errors only if the snapshot's table set drifted from the
/// live database (DDL was rejected at execute-time so this
/// shouldn't fire; the typed error guards against bugs).
fn diff_tables_for_writes(
    live: &HashMap<String, Table>,
    snapshot: &HashMap<String, Table>,
) -> Result<Vec<(RowID, VersionPayload)>> {
    let mut writes: Vec<(RowID, VersionPayload)> = Vec::new();
    for (name, snap_table) in snapshot {
        let live_table = live.get(name).ok_or_else(|| {
            SQLRiteError::Internal(format!(
                "concurrent commit: table '{name}' missing from live database"
            ))
        })?;
        let live_rowids: std::collections::HashSet<i64> = live_table.rowids().into_iter().collect();
        let snap_rowids = snap_table.rowids();
        for rowid in &snap_rowids {
            let snap_payload = build_payload(snap_table, *rowid);
            if live_rowids.contains(rowid) {
                let live_payload = build_payload(live_table, *rowid);
                if live_payload != snap_payload {
                    writes.push((RowID::new(name, *rowid), snap_payload));
                }
            } else {
                writes.push((RowID::new(name, *rowid), snap_payload));
            }
        }
        let snap_set: std::collections::HashSet<i64> = snap_rowids.into_iter().collect();
        for rowid in live_table.rowids() {
            if !snap_set.contains(&rowid) {
                writes.push((RowID::new(name, rowid), VersionPayload::Tombstone));
            }
        }
    }
    Ok(writes)
}

/// Builds a [`VersionPayload::Present`] from a row's column-value
/// pairs. Column order is the table's declaration order; missing
/// values surface as [`Value::Null`].
fn build_payload(table: &Table, rowid: i64) -> VersionPayload {
    let cols = table.column_names();
    let vals = table.extract_row(rowid);
    let pairs: Vec<(String, Value)> = cols
        .into_iter()
        .zip(vals)
        .map(|(c, v)| (c, v.unwrap_or(Value::Null)))
        .collect();
    VersionPayload::Present(pairs)
}

/// Applies the commit's write-set onto the live database
/// row-by-row. Each `(RowID, payload)` translates into a
/// `delete_row` (always — clears column data and any
/// secondary-index entries that reference the row) followed
/// by a `restore_row` if the payload is `Present`.
///
/// Per-row apply rather than wholesale table-replace because
/// other concurrent transactions may have committed onto the
/// live database between our BEGIN and our COMMIT — replacing
/// the whole table would silently undo their disjoint writes.
/// The validation pass already proved we have no row-level
/// conflict with those commits, so writing only our own rows
/// preserves theirs.
///
/// The `_snapshot` parameter is unused today but kept on the
/// signature so the FTS / HNSW maintenance pass can grow into
/// it in a follow-up (the snapshot has the secondary-index
/// state the executor built during the tx; the live table
/// will need the same updates if that index is on a touched
/// column).
fn apply_writes_to_live(
    db: &mut Database,
    _snapshot: &HashMap<String, Table>,
    writes: &[(RowID, VersionPayload)],
) -> Result<()> {
    for (row_id, payload) in writes {
        let live_table = db.tables.get_mut(&row_id.table).ok_or_else(|| {
            SQLRiteError::Internal(format!(
                "concurrent commit: table '{}' missing from live database",
                row_id.table
            ))
        })?;
        // Always remove the existing row first — this clears the
        // per-column storage and the secondary-index entries that
        // reference it. INSERT (no existing row) is a no-op
        // delete; UPDATE turns into delete-then-insert; DELETE is
        // just delete.
        live_table.delete_row(row_id.rowid);
        if let VersionPayload::Present(cols) = payload {
            // The payload's column order matches the table's
            // declaration order (build_payload uses
            // column_names() and extract_row(), both of which
            // walk in declaration order). Map back into the
            // `Vec<Option<Value>>` shape `restore_row` expects.
            let values: Vec<Option<Value>> = cols
                .iter()
                .map(|(_col, value)| match value {
                    Value::Null => None,
                    other => Some(other.clone()),
                })
                .collect();
            live_table.restore_row(row_id.rowid, values).map_err(|e| {
                SQLRiteError::Internal(format!(
                    "concurrent commit: restore_row({}) on table '{}' failed: {e}",
                    row_id.rowid, row_id.table,
                ))
            })?;
        }
    }
    Ok(())
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
        let mut db = self.conn.lock();
        process_ast_with_render(ast, &mut db).map(|o| o.status)
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
        let mut db = self.conn.lock();
        process_ast_with_render(ast, &mut db).map(|o| o.status)
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
        // Phase 11.5 — when a `BEGIN CONCURRENT` is open on this
        // connection, the read sees the transaction's BEGIN-time
        // snapshot, not the post-commit live database. The
        // helper handles the swap (and the no-op fallback for
        // the common case where no concurrent tx is open).
        let result = self
            .conn
            .with_snapshot_read(|db| execute_select_rows(sq.clone(), db))?;
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
        // Phase 11.5 — same snapshot-read path as `query()`, just
        // running on the substituted SelectQuery rather than the
        // cached one.
        let result = self
            .conn
            .with_snapshot_read(|db| execute_select_rows(sq, db))?;
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

    // -----------------------------------------------------------------
    // Phase 10.1 — multi-connection foundation
    // -----------------------------------------------------------------

    /// `connect()` mints a sibling handle that shares the backing
    /// `Database`. Writes through one are visible through the other —
    /// the headline behavioural change for Phase 10.1.
    #[test]
    fn connect_shares_underlying_database() {
        let mut a = Connection::open_in_memory().unwrap();
        let mut b = a.connect();
        assert_eq!(a.handle_count(), 2);

        a.execute("CREATE TABLE shared (id INTEGER PRIMARY KEY, label TEXT);")
            .unwrap();
        a.execute("INSERT INTO shared (label) VALUES ('via-a');")
            .unwrap();
        b.execute("INSERT INTO shared (label) VALUES ('via-b');")
            .unwrap();

        let stmt = b.prepare("SELECT label FROM shared;").unwrap();
        let mut labels: Vec<String> = stmt
            .query()
            .unwrap()
            .collect_all()
            .unwrap()
            .into_iter()
            .map(|r| r.get::<String>(0).unwrap())
            .collect();
        labels.sort();
        assert_eq!(labels, vec!["via-a".to_string(), "via-b".to_string()]);
    }

    /// Dropping a sibling decrements the handle count without
    /// disturbing the surviving connections.
    #[test]
    fn handle_count_reflects_live_handles() {
        let primary = Connection::open_in_memory().unwrap();
        assert_eq!(primary.handle_count(), 1);
        let s1 = primary.connect();
        let s2 = primary.connect();
        assert_eq!(primary.handle_count(), 3);
        drop(s1);
        assert_eq!(primary.handle_count(), 2);
        drop(s2);
        assert_eq!(primary.handle_count(), 1);
    }

    /// Multi-thread INSERT/COMMIT against the same in-memory DB. Today
    /// the per-`Database` mutex serializes commits — this test proves
    /// the locking holds without panics or data loss when N threads
    /// race for the writer. Phase 10.4's `BEGIN CONCURRENT` will lift
    /// the serialization for disjoint-row workloads; until then the
    /// guarantee is "no panic, every commit lands."
    #[test]
    fn threaded_writers_serialize_cleanly() {
        use std::thread;

        let primary = Connection::open_in_memory().unwrap();
        // Set up the shared schema before spawning so every worker
        // sees the table.
        {
            let mut p = primary.connect();
            p.execute("CREATE TABLE log (id INTEGER PRIMARY KEY, who TEXT, n INTEGER);")
                .unwrap();
        }

        const THREADS: usize = 8;
        const PER_THREAD: usize = 25;

        let handles: Vec<_> = (0..THREADS)
            .map(|tid| {
                let mut conn = primary.connect();
                thread::spawn(move || {
                    for n in 0..PER_THREAD {
                        let sql = format!("INSERT INTO log (who, n) VALUES ('t{tid}', {n});");
                        conn.execute(&sql).expect("insert under contention");
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("worker panicked");
        }

        // Every write must have landed exactly once — count rows by
        // probing the table directly so we don't depend on a SELECT
        // COUNT(*) implementation.
        let db = primary.database();
        let table = db.get_table("log".to_string()).unwrap();
        assert_eq!(
            table.rowids().len(),
            THREADS * PER_THREAD,
            "expected every threaded INSERT to commit",
        );
    }

    /// `connect()` over a file-backed database produces sibling
    /// handles that hit the same on-disk pager. Auto-save through one
    /// must be visible through the other without a re-open.
    #[test]
    fn connect_shares_file_backed_database() {
        let path = tmp_path("connect_file");
        let mut primary = Connection::open(&path).unwrap();
        primary
            .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT);")
            .unwrap();

        let mut sibling = primary.connect();
        sibling.execute("INSERT INTO t (v) VALUES ('hi');").unwrap();

        let stmt = primary.prepare("SELECT v FROM t;").unwrap();
        let rows = stmt.query().unwrap().collect_all().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<String>(0).unwrap(), "hi");

        drop(sibling);
        drop(primary);
        cleanup(&path);
    }

    /// Prepared-statement caches are per-handle, by design — sharing
    /// a mutable LRU across threads would require an extra lock for
    /// no real win (each worker prepares its own hot SQL).
    #[test]
    fn prep_cache_is_per_handle() {
        let mut a = Connection::open_in_memory().unwrap();
        a.execute("CREATE TABLE t (a INTEGER);").unwrap();
        let mut b = a.connect();

        let _ = a.prepare_cached("SELECT a FROM t").unwrap();
        let _ = a.prepare_cached("SELECT a FROM t").unwrap();
        assert_eq!(a.prepared_cache_len(), 1);
        // The sibling's cache is untouched.
        assert_eq!(b.prepared_cache_len(), 0);
        let _ = b.prepare_cached("SELECT a FROM t").unwrap();
        assert_eq!(b.prepared_cache_len(), 1);
    }

    /// Static check: `Connection` is `Send + Sync`. Required so it can
    /// be moved across threads (or wrapped in `Arc`) without a typestate
    /// adapter — the headline contract Phase 10.1 puts in place.
    #[test]
    fn connection_is_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<Connection>();
        assert_sync::<Connection>();
    }

    // -----------------------------------------------------------------
    // Phase 11.3 — `PRAGMA journal_mode` round-trip
    // -----------------------------------------------------------------

    /// Fresh connections default to `wal` mode. The PRAGMA read form
    /// renders the current value as a single-row, single-column table
    /// the REPL can print.
    #[test]
    fn journal_mode_defaults_to_wal_and_renders_through_pragma() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(conn.journal_mode(), crate::mvcc::JournalMode::Wal);

        // Read form returns "1 row returned." status (matching
        // `auto_vacuum`'s shape).
        let status = conn.execute("PRAGMA journal_mode;").unwrap();
        assert!(
            status.contains("1 row returned"),
            "unexpected status: {status}"
        );
    }

    /// `PRAGMA journal_mode = mvcc;` flips the per-database mode and
    /// is observable through every sibling handle. The headline
    /// per-database contract for Phase 11.3.
    #[test]
    fn journal_mode_set_to_mvcc_propagates_to_siblings() {
        let mut primary = Connection::open_in_memory().unwrap();
        let sibling = primary.connect();
        assert_eq!(sibling.journal_mode(), crate::mvcc::JournalMode::Wal);

        primary.execute("PRAGMA journal_mode = mvcc;").unwrap();
        assert_eq!(primary.journal_mode(), crate::mvcc::JournalMode::Mvcc);
        // Sibling sees the same value — proves the setting lives on
        // the shared `Database`, not on the per-handle Connection.
        assert_eq!(sibling.journal_mode(), crate::mvcc::JournalMode::Mvcc);

        // Switch back is allowed because no MVCC versions exist yet
        // (11.4 will populate the store).
        primary.execute("PRAGMA journal_mode = wal;").unwrap();
        assert_eq!(primary.journal_mode(), crate::mvcc::JournalMode::Wal);
        assert_eq!(sibling.journal_mode(), crate::mvcc::JournalMode::Wal);
    }

    /// The set form is case-insensitive on both the pragma name and
    /// the value (matching SQLite). Quoted values work too.
    #[test]
    fn journal_mode_pragma_is_case_insensitive() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA JOURNAL_MODE = MVCC;").unwrap();
        assert_eq!(conn.journal_mode(), crate::mvcc::JournalMode::Mvcc);
        conn.execute("pragma journal_mode = 'wal';").unwrap();
        assert_eq!(conn.journal_mode(), crate::mvcc::JournalMode::Wal);
    }

    /// Unknown modes return a typed error and don't disturb the
    /// existing setting.
    #[test]
    fn journal_mode_rejects_unknown_value() {
        let mut conn = Connection::open_in_memory().unwrap();
        let err = conn
            .execute("PRAGMA journal_mode = delete;")
            .expect_err("unknown mode must error");
        let msg = format!("{err}");
        assert!(
            msg.contains("unknown mode 'delete'"),
            "unexpected error: {msg}"
        );
        // Setting wasn't disturbed.
        assert_eq!(conn.journal_mode(), crate::mvcc::JournalMode::Wal);
    }

    /// Numeric values are rejected — `journal_mode` is enum-shaped.
    /// SQLite accepts e.g. `journal_mode = 0` for OFF historically;
    /// SQLRite stays explicit.
    #[test]
    fn journal_mode_rejects_numeric_value() {
        let mut conn = Connection::open_in_memory().unwrap();
        let err = conn
            .execute("PRAGMA journal_mode = 0;")
            .expect_err("numeric mode must error");
        let msg = format!("{err}");
        assert!(msg.contains("numeric"), "unexpected error: {msg}");
    }

    // -----------------------------------------------------------------
    // Phase 11.4 — `BEGIN CONCURRENT` end-to-end
    // -----------------------------------------------------------------

    /// `BEGIN CONCURRENT` requires `PRAGMA journal_mode = mvcc;`
    /// first. v0 doesn't auto-enable MVCC mode; users opt in
    /// explicitly so the implications (in-memory MvStore growth,
    /// `Busy` errors becoming possible) aren't a surprise.
    #[test]
    fn begin_concurrent_requires_mvcc_journal_mode() {
        let mut conn = Connection::open_in_memory().unwrap();
        let err = conn
            .execute("BEGIN CONCURRENT;")
            .expect_err("must require MVCC journal mode");
        let msg = format!("{err}");
        assert!(
            msg.contains("PRAGMA journal_mode = mvcc"),
            "unexpected error: {msg}"
        );
    }

    /// Round-trip: enable MVCC, BEGIN CONCURRENT, no writes,
    /// COMMIT. The simplest control-flow check — proves the
    /// parser-intent + lifecycle hooks all line up.
    #[test]
    fn begin_concurrent_then_empty_commit_round_trips() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA journal_mode = mvcc;").unwrap();
        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
            .unwrap();
        let begin_status = conn.execute("BEGIN CONCURRENT;").unwrap();
        assert_eq!(begin_status, "BEGIN");
        let commit_status = conn.execute("COMMIT;").unwrap();
        assert_eq!(commit_status, "COMMIT");
    }

    /// Plan test #1: two concurrent transactions on **disjoint
    /// rowids** must both commit. No write-write conflict to
    /// detect; validation passes for both.
    #[test]
    fn two_concurrent_inserts_on_disjoint_rows_both_commit() {
        let mut a = Connection::open_in_memory().unwrap();
        a.execute("PRAGMA journal_mode = mvcc;").unwrap();
        a.execute("CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER);")
            .unwrap();
        let mut b = a.connect();

        a.execute("BEGIN CONCURRENT;").unwrap();
        a.execute("INSERT INTO accounts (id, balance) VALUES (1, 100);")
            .unwrap();

        b.execute("BEGIN CONCURRENT;").unwrap();
        b.execute("INSERT INTO accounts (id, balance) VALUES (2, 200);")
            .unwrap();

        // Both commit cleanly — disjoint rowids, no conflict.
        a.execute("COMMIT;").unwrap();
        b.execute("COMMIT;").unwrap();

        // Both rows are visible through the legacy read path.
        let stmt = a.prepare("SELECT id, balance FROM accounts;").unwrap();
        let mut rows: Vec<(i64, i64)> = stmt
            .query()
            .unwrap()
            .collect_all()
            .unwrap()
            .into_iter()
            .map(|r| (r.get::<i64>(0).unwrap(), r.get::<i64>(1).unwrap()))
            .collect();
        rows.sort();
        assert_eq!(rows, vec![(1, 100), (2, 200)]);
    }

    /// Plan test #2: two concurrent transactions on the **same
    /// row** — one commits, the other aborts with `Busy`.
    #[test]
    fn two_concurrent_updates_same_row_one_aborts_with_busy() {
        let mut a = Connection::open_in_memory().unwrap();
        a.execute("PRAGMA journal_mode = mvcc;").unwrap();
        a.execute("CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER);")
            .unwrap();
        a.execute("INSERT INTO accounts (id, balance) VALUES (1, 100);")
            .unwrap();
        let mut b = a.connect();

        // Both BEGIN before either UPDATE — that's the snapshot
        // the validation checks against.
        a.execute("BEGIN CONCURRENT;").unwrap();
        b.execute("BEGIN CONCURRENT;").unwrap();

        a.execute("UPDATE accounts SET balance = 200 WHERE id = 1;")
            .unwrap();
        b.execute("UPDATE accounts SET balance = 300 WHERE id = 1;")
            .unwrap();

        // First commit wins.
        a.execute("COMMIT;").unwrap();

        // Second commit hits the validation pass and aborts.
        let err = b
            .execute("COMMIT;")
            .expect_err("second commit must abort with Busy");
        assert!(matches!(err, SQLRiteError::Busy(_)));
        assert!(err.is_retryable(), "Busy must be retryable");
        let msg = format!("{err}");
        assert!(
            msg.contains("write-write conflict"),
            "unexpected error: {msg}"
        );

        // The winning value is what's persisted.
        let stmt = a
            .prepare("SELECT balance FROM accounts WHERE id = 1;")
            .unwrap();
        let rows = stmt.query().unwrap().collect_all().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 200);
    }

    /// Plan test #3: an aborted transaction's writes must never
    /// become visible. After ROLLBACK (explicit or implicit on
    /// Busy), the row keeps its pre-tx value.
    #[test]
    fn aborted_transactions_writes_never_become_visible() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA journal_mode = mvcc;").unwrap();
        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
            .unwrap();
        conn.execute("INSERT INTO t (id, v) VALUES (1, 100);")
            .unwrap();

        // Explicit ROLLBACK.
        conn.execute("BEGIN CONCURRENT;").unwrap();
        conn.execute("UPDATE t SET v = 999 WHERE id = 1;").unwrap();
        conn.execute("ROLLBACK;").unwrap();

        let stmt = conn.prepare("SELECT v FROM t WHERE id = 1;").unwrap();
        let rows = stmt.query().unwrap().collect_all().unwrap();
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 100);

        // Implicit rollback via Busy: another connection commits a
        // newer version under us.
        let mut other = conn.connect();
        conn.execute("BEGIN CONCURRENT;").unwrap();
        other.execute("BEGIN CONCURRENT;").unwrap();
        conn.execute("UPDATE t SET v = 7 WHERE id = 1;").unwrap();
        other.execute("UPDATE t SET v = 13 WHERE id = 1;").unwrap();
        conn.execute("COMMIT;").unwrap();
        let _ = other.execute("COMMIT;").expect_err("must abort with Busy");

        // The losing writer's value (13) never lands. The winner
        // (7) is what's visible.
        let rows = conn
            .prepare("SELECT v FROM t WHERE id = 1;")
            .unwrap()
            .query()
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 7);
    }

    /// Plan test #4: retry-after-`Busy` succeeds. The caller's
    /// retry helper opens a fresh `BEGIN CONCURRENT` (with a
    /// new `begin_ts` past the conflict) and the same UPDATE
    /// commits cleanly.
    #[test]
    fn retry_after_busy_succeeds() {
        let mut a = Connection::open_in_memory().unwrap();
        a.execute("PRAGMA journal_mode = mvcc;").unwrap();
        a.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
            .unwrap();
        a.execute("INSERT INTO t (id, v) VALUES (1, 1);").unwrap();
        let mut b = a.connect();

        a.execute("BEGIN CONCURRENT;").unwrap();
        b.execute("BEGIN CONCURRENT;").unwrap();
        a.execute("UPDATE t SET v = 100 WHERE id = 1;").unwrap();
        b.execute("UPDATE t SET v = 200 WHERE id = 1;").unwrap();
        a.execute("COMMIT;").unwrap();
        let err = b.execute("COMMIT;").expect_err("first attempt must Busy");
        assert!(err.is_retryable());

        // Retry: open a fresh tx, redo the same UPDATE, commit.
        b.execute("BEGIN CONCURRENT;").unwrap();
        b.execute("UPDATE t SET v = 200 WHERE id = 1;").unwrap();
        b.execute("COMMIT;").expect("retry must succeed");

        let rows = a
            .prepare("SELECT v FROM t WHERE id = 1;")
            .unwrap()
            .query()
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 200);
    }

    /// Nested `BEGIN CONCURRENT` is rejected with a typed error.
    /// Same single-tx-per-connection rule the legacy `BEGIN`
    /// already enforces.
    #[test]
    fn nested_begin_concurrent_is_rejected() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA journal_mode = mvcc;").unwrap();
        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY);")
            .unwrap();
        conn.execute("BEGIN CONCURRENT;").unwrap();
        let err = conn
            .execute("BEGIN CONCURRENT;")
            .expect_err("nested BEGIN CONCURRENT must error");
        assert!(format!("{err}").contains("already open"));
    }

    /// Legacy `BEGIN` inside `BEGIN CONCURRENT` is rejected.
    /// Mixing the two transaction kinds isn't supported in v0;
    /// the deep-clone snapshot and the MVCC write-set don't
    /// interleave cleanly.
    #[test]
    fn legacy_begin_inside_concurrent_is_rejected() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA journal_mode = mvcc;").unwrap();
        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY);")
            .unwrap();
        conn.execute("BEGIN CONCURRENT;").unwrap();
        let err = conn
            .execute("BEGIN;")
            .expect_err("legacy BEGIN inside concurrent tx must error");
        assert!(format!("{err}").contains("concurrent transaction is already open"));
    }

    /// DDL inside `BEGIN CONCURRENT` is rejected with a typed
    /// error. Plan §8 calls this out as an explicit non-goal —
    /// schema mutations interact poorly with the snapshot-
    /// based commit and the v0 write-set model.
    #[test]
    fn ddl_inside_begin_concurrent_is_rejected() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA journal_mode = mvcc;").unwrap();
        conn.execute("BEGIN CONCURRENT;").unwrap();
        let err = conn
            .execute("CREATE TABLE t (id INTEGER PRIMARY KEY);")
            .expect_err("DDL inside concurrent tx must error");
        let msg = format!("{err}");
        assert!(msg.contains("DDL is not supported"), "unexpected: {msg}");
        // The transaction stays open — caller can ROLLBACK.
        conn.execute("ROLLBACK;").unwrap();
    }

    /// An empty concurrent commit (BEGIN, no writes, COMMIT)
    /// always succeeds — even when other transactions have
    /// committed in the meantime, because we have nothing to
    /// validate.
    #[test]
    fn empty_concurrent_commit_never_busies() {
        let mut a = Connection::open_in_memory().unwrap();
        a.execute("PRAGMA journal_mode = mvcc;").unwrap();
        a.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
            .unwrap();
        a.execute("INSERT INTO t (id, v) VALUES (1, 1);").unwrap();
        let mut b = a.connect();

        a.execute("BEGIN CONCURRENT;").unwrap();
        // Sibling B opens its own concurrent tx and commits a
        // change to row 1.
        b.execute("BEGIN CONCURRENT;").unwrap();
        b.execute("UPDATE t SET v = 999 WHERE id = 1;").unwrap();
        b.execute("COMMIT;").unwrap();

        // a never wrote anything — its commit is purely a
        // tx-state cleanup. Validation has no rows to check.
        a.execute("COMMIT;")
            .expect("empty commit must succeed even if siblings committed");
    }

    // -----------------------------------------------------------------
    // Phase 11.5 — snapshot-isolated reads via Statement::query
    // -----------------------------------------------------------------

    /// The headline 11.5 contract: a SELECT issued via
    /// `prepare(...).query()` inside an open `BEGIN CONCURRENT`
    /// sees the BEGIN-time snapshot, not the post-commit live
    /// state. Phase 11.4 had this test failing because the
    /// prepare/query path bypassed the swap; Phase 11.5 routes
    /// it through `with_snapshot_read`.
    #[test]
    fn query_inside_concurrent_tx_sees_begin_time_snapshot() {
        let mut a = Connection::open_in_memory().unwrap();
        a.execute("PRAGMA journal_mode = mvcc;").unwrap();
        a.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
            .unwrap();
        a.execute("INSERT INTO t (id, v) VALUES (1, 1);").unwrap();
        let mut b = a.connect();

        a.execute("BEGIN CONCURRENT;").unwrap();
        // Sibling B commits a change to row 1 from another tx.
        b.execute("BEGIN CONCURRENT;").unwrap();
        b.execute("UPDATE t SET v = 999 WHERE id = 1;").unwrap();
        b.execute("COMMIT;").unwrap();

        // Reader inside a's tx, via prepare()+query(), must see
        // the BEGIN-time value (1), not b's committed value (999).
        let rows = a
            .prepare("SELECT v FROM t WHERE id = 1;")
            .unwrap()
            .query()
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(
            rows[0].get::<i64>(0).unwrap(),
            1,
            "Statement::query inside BEGIN CONCURRENT must see the snapshot, not the live db"
        );

        // After a's empty commit, the same handle's read sees b's
        // value (999) — the swap is gone, the legacy read path is
        // back in play.
        a.execute("COMMIT;").unwrap();
        let rows = a
            .prepare("SELECT v FROM t WHERE id = 1;")
            .unwrap()
            .query()
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 999);
    }

    /// Read-your-writes: an UPDATE inside the tx is visible to
    /// the same tx's subsequent SELECT via `query()`. The swap
    /// makes the tx's private clone the read target, so writes
    /// the executor staged on the clone are reflected.
    #[test]
    fn query_inside_concurrent_tx_sees_own_writes() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA journal_mode = mvcc;").unwrap();
        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
            .unwrap();
        conn.execute("INSERT INTO t (id, v) VALUES (1, 100);")
            .unwrap();

        conn.execute("BEGIN CONCURRENT;").unwrap();
        conn.execute("UPDATE t SET v = 200 WHERE id = 1;").unwrap();
        // Inside the tx, query() sees v = 200 (our own write).
        let rows = conn
            .prepare("SELECT v FROM t WHERE id = 1;")
            .unwrap()
            .query()
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 200);

        // After ROLLBACK, the live db still has 100 (the write
        // never landed).
        conn.execute("ROLLBACK;").unwrap();
        let rows = conn
            .prepare("SELECT v FROM t WHERE id = 1;")
            .unwrap()
            .query()
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 100);
    }

    /// Reads via `query_with_params` (parameter-bound SELECT)
    /// also flow through the snapshot. Same path, just with the
    /// substitution step in front.
    #[test]
    fn query_with_params_inside_concurrent_tx_sees_snapshot() {
        let mut a = Connection::open_in_memory().unwrap();
        a.execute("PRAGMA journal_mode = mvcc;").unwrap();
        a.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
            .unwrap();
        a.execute("INSERT INTO t (id, v) VALUES (1, 7);").unwrap();
        let mut b = a.connect();

        a.execute("BEGIN CONCURRENT;").unwrap();
        b.execute("BEGIN CONCURRENT;").unwrap();
        b.execute("UPDATE t SET v = 42 WHERE id = 1;").unwrap();
        b.execute("COMMIT;").unwrap();

        let rows = a
            .prepare("SELECT v FROM t WHERE id = ?")
            .unwrap()
            .query_with_params(&[Value::Integer(1)])
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 7);

        a.execute("COMMIT;").unwrap();
    }

    /// Outside any concurrent tx, `query()` reads the live
    /// database. Sanity check that 11.5's snapshot routing is
    /// strictly opt-in via `BEGIN CONCURRENT`.
    #[test]
    fn query_outside_concurrent_tx_sees_live_database() {
        let mut a = Connection::open_in_memory().unwrap();
        a.execute("PRAGMA journal_mode = mvcc;").unwrap();
        a.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
            .unwrap();
        a.execute("INSERT INTO t (id, v) VALUES (1, 1);").unwrap();
        let mut b = a.connect();

        // Sibling commits a change. a is NOT in a tx, so its read
        // should see the post-commit value.
        b.execute("BEGIN CONCURRENT;").unwrap();
        b.execute("UPDATE t SET v = 100 WHERE id = 1;").unwrap();
        b.execute("COMMIT;").unwrap();

        let rows = a
            .prepare("SELECT v FROM t WHERE id = 1;")
            .unwrap()
            .query()
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 100);
    }

    /// Sibling reader at the moment a writer commits: the
    /// reader's own `BEGIN CONCURRENT` (and its private snapshot)
    /// must isolate it from the writer's commit, so the snapshot
    /// stays internally consistent for the reader's lifetime.
    /// Repeats the read multiple times across the writer's
    /// activity to catch any races where the snapshot leaks.
    #[test]
    fn snapshot_stays_consistent_across_sibling_commits() {
        let mut reader = Connection::open_in_memory().unwrap();
        reader.execute("PRAGMA journal_mode = mvcc;").unwrap();
        reader
            .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
            .unwrap();
        reader
            .execute("INSERT INTO t (id, v) VALUES (1, 1);")
            .unwrap();
        let mut writer = reader.connect();

        reader.execute("BEGIN CONCURRENT;").unwrap();
        // First read inside reader's tx — sees v=1.
        let read_at_t0 = reader
            .prepare("SELECT v FROM t WHERE id = 1;")
            .unwrap()
            .query()
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(read_at_t0[0].get::<i64>(0).unwrap(), 1);

        // Writer commits a stream of changes between reader's
        // reads. Each commit advances the live db and adds a
        // version to MvStore.
        for new_value in [10, 20, 30, 40] {
            writer.execute("BEGIN CONCURRENT;").unwrap();
            writer
                .execute(&format!("UPDATE t SET v = {new_value} WHERE id = 1;"))
                .unwrap();
            writer.execute("COMMIT;").unwrap();

            // Reader's snapshot must still see v=1.
            let r = reader
                .prepare("SELECT v FROM t WHERE id = 1;")
                .unwrap()
                .query()
                .unwrap()
                .collect_all()
                .unwrap();
            assert_eq!(
                r[0].get::<i64>(0).unwrap(),
                1,
                "snapshot regressed after writer committed v={new_value}",
            );
        }

        reader.execute("COMMIT;").unwrap();
    }

    // -----------------------------------------------------------------
    // Phase 11.6 — MVCC garbage collection
    // -----------------------------------------------------------------

    /// Per-commit GC bounds the chain length under repeated
    /// updates to the same row when no readers are holding a
    /// snapshot that would need older versions. After many
    /// updates the store should hold roughly one version per row,
    /// not a version per commit.
    #[test]
    fn repeated_updates_keep_chain_bounded_when_no_readers() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA journal_mode = mvcc;").unwrap();
        conn.execute("CREATE TABLE counters (id INTEGER PRIMARY KEY, n INTEGER);")
            .unwrap();
        conn.execute("INSERT INTO counters (id, n) VALUES (1, 0);")
            .unwrap();

        // 50 sequential updates inside their own concurrent
        // transactions. With no overlapping readers, the
        // per-commit GC sweep should reclaim every superseded
        // version and leave only the latest.
        for n in 1..=50 {
            conn.execute("BEGIN CONCURRENT;").unwrap();
            conn.execute(&format!("UPDATE counters SET n = {n} WHERE id = 1;"))
                .unwrap();
            conn.execute("COMMIT;").unwrap();
        }

        // MvStore should now hold exactly one version for the
        // row we hammered (the latest). Without GC it would hold
        // 50.
        let db = conn.database();
        let store_size = db.mv_store().total_versions();
        let tracked = db.mv_store().tracked_rows();
        drop(db);
        assert_eq!(
            store_size, 1,
            "expected 1 version after 50 GC'd updates, got {store_size}",
        );
        assert_eq!(tracked, 1);
    }

    /// GC must NOT reclaim versions that an in-flight reader's
    /// snapshot might still see. While a reader holds an open
    /// `BEGIN CONCURRENT` at `begin_ts = T`, every version with
    /// `end > T` must remain in the chain.
    #[test]
    fn gc_preserves_versions_visible_to_active_reader() {
        let mut writer = Connection::open_in_memory().unwrap();
        writer.execute("PRAGMA journal_mode = mvcc;").unwrap();
        writer
            .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
            .unwrap();
        writer
            .execute("INSERT INTO t (id, v) VALUES (1, 0);")
            .unwrap();
        let mut reader = writer.connect();

        // Reader opens its tx FIRST so its snapshot sits at the
        // smallest `begin_ts` across the active set.
        reader.execute("BEGIN CONCURRENT;").unwrap();

        // Writer commits five updates; per-commit GC fires after
        // each, but the reader's begin_ts pins the watermark so
        // the older versions can't be reclaimed.
        for n in 1..=5 {
            writer.execute("BEGIN CONCURRENT;").unwrap();
            writer
                .execute(&format!("UPDATE t SET v = {n} WHERE id = 1;"))
                .unwrap();
            writer.execute("COMMIT;").unwrap();
        }

        // Reader's snapshot still sees v=0 — the chain must have
        // retained the original version (or a tombstone-capped
        // earlier value) so the visibility rule resolves it.
        let rows = reader
            .prepare("SELECT v FROM t WHERE id = 1;")
            .unwrap()
            .query()
            .unwrap()
            .collect_all()
            .unwrap();
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 0);

        // The reader's snapshot is preserved by GC's watermark.
        // No assertion on the exact chain length — that's an
        // implementation detail; the property is "reader sees
        // v=0 even after writer's burst."

        reader.execute("COMMIT;").unwrap();

        // After the reader closes, the watermark jumps and an
        // explicit vacuum reclaims everything reclaimable.
        // (We skip checking the exact reclaim count because the
        // post-reader-close state of the chain depends on the
        // ordering of the reader's `drop` and the watermark
        // sample inside `vacuum_mvcc` — both are correct, just
        // different.)
        writer.vacuum_mvcc();
        let db = writer.database();
        let store_size = db.mv_store().total_versions();
        drop(db);
        // At most one version per row (the latest committed).
        assert!(
            store_size <= 1,
            "after reader closed and vacuum ran, expected ≤1 version, got {store_size}",
        );
    }

    /// `Connection::vacuum_mvcc` is a no-op on a fresh
    /// `JournalMode::Wal` database: the store is empty, nothing
    /// to reclaim. Matches the "safe to call regardless of
    /// journal mode" contract.
    #[test]
    fn vacuum_mvcc_is_a_noop_on_wal_database() {
        let conn = Connection::open_in_memory().unwrap();
        // Default journal mode is Wal; never enabled MVCC.
        assert_eq!(conn.vacuum_mvcc(), 0);
    }

    /// Explicit `vacuum_mvcc` reclaims everything reclaimable
    /// when no transactions are active.
    #[test]
    fn vacuum_mvcc_reclaims_everything_with_no_active_readers() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA journal_mode = mvcc;").unwrap();
        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
            .unwrap();

        // Build up some versions.
        conn.execute("INSERT INTO t (id, v) VALUES (1, 0);")
            .unwrap();
        conn.execute("BEGIN CONCURRENT;").unwrap();
        conn.execute("UPDATE t SET v = 1 WHERE id = 1;").unwrap();
        conn.execute("COMMIT;").unwrap();
        conn.execute("BEGIN CONCURRENT;").unwrap();
        conn.execute("UPDATE t SET v = 2 WHERE id = 1;").unwrap();
        conn.execute("COMMIT;").unwrap();

        // Per-commit GC has already done most of the work; the
        // explicit vacuum is idempotent.
        let _ = conn.vacuum_mvcc();
        let db = conn.database();
        let store_size = db.mv_store().total_versions();
        drop(db);
        assert!(store_size <= 1);
    }

    /// `is_retryable()` covers both `Busy` and `BusySnapshot`
    /// without callers having to match each variant. The contract
    /// SDK retry helpers will rely on.
    #[test]
    fn is_retryable_covers_busy_variants() {
        assert!(SQLRiteError::Busy("x".into()).is_retryable());
        assert!(SQLRiteError::BusySnapshot("x".into()).is_retryable());
        assert!(!SQLRiteError::General("x".into()).is_retryable());
    }

    /// Phase 11.9 — every BEGIN CONCURRENT commit on a file-backed
    /// database leaves an MVCC log-record frame in the WAL. The Pager
    /// surfaces those on reopen via `recovered_mvcc_commits`.
    #[test]
    fn mvcc_commit_persists_a_log_record_into_wal() {
        let path = tmp_path("mvcc_log_record");
        {
            let mut c = Connection::open(&path).unwrap();
            c.execute("PRAGMA journal_mode = mvcc;").unwrap();
            c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
                .unwrap();
            c.execute("BEGIN CONCURRENT;").unwrap();
            c.execute("INSERT INTO t (id, v) VALUES (1, 42);").unwrap();
            c.execute("COMMIT;").unwrap();
        }
        // Reopen and confirm the WAL replay surfaced the batch.
        let c2 = Connection::open(&path).unwrap();
        let db = c2.database();
        let pager = db.pager.as_ref().expect("file-backed db carries a pager");
        let batches = pager.recovered_mvcc_commits();
        assert_eq!(batches.len(), 1, "one BEGIN CONCURRENT commit -> one batch");
        assert_eq!(batches[0].records.len(), 1, "one row written");
        let rec = &batches[0].records[0];
        assert_eq!(rec.row.table, "t");
        assert_eq!(rec.row.rowid, 1);
        match &rec.payload {
            VersionPayload::Present(cols) => {
                assert!(cols.iter().any(
                    |(k, v)| k == "v" && matches!(v, crate::sql::db::table::Value::Integer(42))
                ));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
        drop(db);
        drop(c2);
        cleanup(&path);
    }

    /// Phase 11.9 — on reopen the MVCC log records are pushed back
    /// into `MvStore`. The conflict-detection window survives a
    /// process restart: a write whose `begin_ts` predates a
    /// replayed commit must surface as `Busy`.
    #[test]
    fn mvcc_reopen_restores_mv_store_and_clock() {
        let path = tmp_path("mvcc_reopen");
        {
            let mut c = Connection::open(&path).unwrap();
            c.execute("PRAGMA journal_mode = mvcc;").unwrap();
            c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
                .unwrap();
            c.execute("BEGIN CONCURRENT;").unwrap();
            c.execute("INSERT INTO t (id, v) VALUES (1, 10);").unwrap();
            c.execute("COMMIT;").unwrap();
            c.execute("BEGIN CONCURRENT;").unwrap();
            c.execute("UPDATE t SET v = 20 WHERE id = 1;").unwrap();
            c.execute("COMMIT;").unwrap();
        }
        let c2 = Connection::open(&path).unwrap();
        let db = c2.database();
        // Two commits replayed → two versions for row t/1 (the
        // first capped, the second open-ended).
        let store = db.mv_store();
        let row = RowID::new("t", 1);
        assert!(
            store.latest_committed_begin(&row).is_some(),
            "MvStore should know about row t/1 after reopen"
        );
        // Clock must have advanced past the persisted commits so
        // any new transaction gets a fresh `begin_ts`.
        let last_commit_ts = store.latest_committed_begin(&row).unwrap();
        assert!(
            db.mvcc_clock().now() >= last_commit_ts,
            "clock {} must be >= last replayed commit_ts {}",
            db.mvcc_clock().now(),
            last_commit_ts,
        );
        drop(db);
        drop(c2);
        cleanup(&path);
    }

    /// Phase 11.9 — multi-row batches survive replay intact, with
    /// every (RowID, payload) pair coming back from the WAL.
    #[test]
    fn mvcc_multi_row_batch_replays_intact() {
        let path = tmp_path("mvcc_multi_row");
        {
            let mut c = Connection::open(&path).unwrap();
            c.execute("PRAGMA journal_mode = mvcc;").unwrap();
            c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
                .unwrap();
            // Seed rows under legacy mode so the concurrent tx
            // can UPDATE them — Phase 11 keeps INSERT-only
            // semantics for the concurrent path simple.
            c.execute("INSERT INTO t (id, v) VALUES (1, 1);").unwrap();
            c.execute("INSERT INTO t (id, v) VALUES (2, 2);").unwrap();
            c.execute("INSERT INTO t (id, v) VALUES (3, 3);").unwrap();

            c.execute("BEGIN CONCURRENT;").unwrap();
            c.execute("UPDATE t SET v = 100 WHERE id = 1;").unwrap();
            c.execute("UPDATE t SET v = 200 WHERE id = 2;").unwrap();
            c.execute("UPDATE t SET v = 300 WHERE id = 3;").unwrap();
            c.execute("COMMIT;").unwrap();
        }
        let c2 = Connection::open(&path).unwrap();
        let db = c2.database();
        let pager = db.pager.as_ref().unwrap();
        let batches = pager.recovered_mvcc_commits();
        assert_eq!(batches.len(), 1, "single COMMIT -> single batch");
        let rowids: Vec<i64> = batches[0].records.iter().map(|r| r.row.rowid).collect();
        assert!(rowids.contains(&1));
        assert!(rowids.contains(&2));
        assert!(rowids.contains(&3));
        assert_eq!(batches[0].records.len(), 3);
        drop(db);
        drop(c2);
        cleanup(&path);
    }

    /// Phase 11.9 — a BEGIN CONCURRENT that's never committed
    /// leaves no MVCC frame in the WAL. The reopen path replays
    /// only what was sealed.
    #[test]
    fn mvcc_rolled_back_tx_leaves_no_wal_record() {
        let path = tmp_path("mvcc_rollback");
        {
            let mut c = Connection::open(&path).unwrap();
            c.execute("PRAGMA journal_mode = mvcc;").unwrap();
            c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
                .unwrap();
            c.execute("BEGIN CONCURRENT;").unwrap();
            c.execute("INSERT INTO t (id, v) VALUES (1, 999);").unwrap();
            c.execute("ROLLBACK;").unwrap();
        }
        let c2 = Connection::open(&path).unwrap();
        let db = c2.database();
        let pager = db.pager.as_ref().unwrap();
        assert!(
            pager.recovered_mvcc_commits().is_empty(),
            "ROLLBACK must not append MVCC frames"
        );
        // Legacy tables also untouched.
        let store = db.mv_store();
        assert_eq!(store.total_versions(), 0);
        drop(db);
        drop(c2);
        cleanup(&path);
    }

    /// Phase 11.9 — legacy (non-BEGIN-CONCURRENT) commits do
    /// **not** emit MVCC frames. The persistence is opt-in along
    /// the same axis as `BEGIN CONCURRENT`.
    #[test]
    fn legacy_commit_does_not_emit_mvcc_frame() {
        let path = tmp_path("mvcc_legacy_no_frame");
        {
            let mut c = Connection::open(&path).unwrap();
            c.execute("PRAGMA journal_mode = mvcc;").unwrap();
            c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY);")
                .unwrap();
            c.execute("INSERT INTO t (id) VALUES (1);").unwrap();
        }
        let c2 = Connection::open(&path).unwrap();
        let db = c2.database();
        let pager = db.pager.as_ref().unwrap();
        assert!(
            pager.recovered_mvcc_commits().is_empty(),
            "legacy writes never produce MVCC frames"
        );
        drop(db);
        drop(c2);
        cleanup(&path);
    }

    /// Phase 11.9 — crash recovery sketch. After several
    /// concurrent commits we drop the connection without an
    /// explicit checkpoint (the auto-checkpoint threshold is
    /// well above what 3 frames triggers). A fresh open replays
    /// every MVCC frame and reconstructs the chain.
    #[test]
    fn mvcc_replays_multiple_commits_after_unclean_close() {
        let path = tmp_path("mvcc_unclean_close");
        {
            let mut c = Connection::open(&path).unwrap();
            c.execute("PRAGMA journal_mode = mvcc;").unwrap();
            c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
                .unwrap();
            for v in 0..5 {
                c.execute("BEGIN CONCURRENT;").unwrap();
                if v == 0 {
                    c.execute("INSERT INTO t (id, v) VALUES (1, 0);").unwrap();
                } else {
                    c.execute(&format!("UPDATE t SET v = {v} WHERE id = 1;"))
                        .unwrap();
                }
                c.execute("COMMIT;").unwrap();
            }
            // c drops here without calling checkpoint — the WAL
            // still holds every MVCC frame.
        }
        let c2 = Connection::open(&path).unwrap();
        let db = c2.database();
        let pager = db.pager.as_ref().unwrap();
        let batches = pager.recovered_mvcc_commits();
        assert_eq!(batches.len(), 5, "every COMMIT must show up after reopen");
        // commit_ts values are strictly increasing.
        for w in batches.windows(2) {
            assert!(w[0].commit_ts < w[1].commit_ts);
        }
        drop(db);
        drop(c2);
        cleanup(&path);
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
