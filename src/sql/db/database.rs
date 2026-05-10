use crate::error::{Result, SQLRiteError};
use crate::mvcc::{JournalMode, MvStore, MvccClock};
use crate::sql::db::table::Table;
use crate::sql::pager::pager::{AccessMode, Pager};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Snapshot of the mutable in-memory state taken at `BEGIN` time so
/// `ROLLBACK` can restore it. See `begin_transaction`, `rollback_transaction`.
/// `tables` is deep-cloned (the `Table::deep_clone` helper reallocates
/// the `Arc<Mutex<_>>` row storage so snapshot and live state don't
/// share a map).
#[derive(Debug)]
pub struct TxnSnapshot {
    pub(crate) tables: HashMap<String, Table>,
}

/// Default fraction of free pages that triggers an auto-VACUUM after
/// a page-releasing DDL (DROP TABLE / DROP INDEX / ALTER TABLE DROP
/// COLUMN). Matches SQLite's classic 25% heuristic. Override per
/// connection with [`Database::set_auto_vacuum_threshold`] (or
/// `Connection::set_auto_vacuum_threshold`); pass `None` to disable.
pub const DEFAULT_AUTO_VACUUM_THRESHOLD: f32 = 0.25;

/// The database is represented by this structure.assert_eq!
#[derive(Debug)]
pub struct Database {
    /// Name of this database. (schema name, not filename)
    pub db_name: String,
    /// HashMap of tables in this database
    pub tables: HashMap<String, Table>,
    /// If `Some`, every committing SQL statement auto-flushes the DB to
    /// this path. `None` → transient in-memory mode (the default; the
    /// REPL only enters persistent mode after `.open FILE`).
    pub source_path: Option<PathBuf>,
    /// Long-lived pager attached when the database is file-backed. Keeps
    /// an in-memory snapshot of every page so auto-saves can diff
    /// against the last-committed state and skip rewriting unchanged
    /// pages. `None` means "in-memory only" or "not yet opened".
    pub pager: Option<Pager>,
    /// Active transaction state (Phase 4f). `Some` between `BEGIN` and
    /// the matching `COMMIT` / `ROLLBACK`. While set:
    /// - auto-save is suppressed (mutations stay in-memory)
    /// - nested `BEGIN` is rejected
    /// - `ROLLBACK` restores `tables` from the snapshot
    pub txn: Option<TxnSnapshot>,
    /// Auto-VACUUM trigger (SQLR-10). After a page-releasing DDL
    /// (DROP TABLE / DROP INDEX / ALTER TABLE DROP COLUMN) commits and
    /// flushes, if the freelist exceeds this fraction of `page_count`
    /// the engine quietly compacts the file. `None` disables the
    /// trigger; defaults to `Some(DEFAULT_AUTO_VACUUM_THRESHOLD)`
    /// (SQLite parity at 25%). Per-connection runtime state — not
    /// persisted across reopens.
    pub auto_vacuum_threshold: Option<f32>,
    /// Phase 11.3 — current journal mode for the database. Default
    /// is [`JournalMode::Wal`] (every pre-Phase-11 caller). Toggled
    /// by `PRAGMA journal_mode = mvcc | wal`. The setting is
    /// per-database (every `Connection` to this `Database` observes
    /// the same value) — see the open question in
    /// [`docs/concurrent-writes-plan.md`](../../../docs/concurrent-writes-plan.md)
    /// §8 for the per-connection vs. per-database trade-off; v0
    /// picked per-database for simplicity.
    pub journal_mode: JournalMode,
    /// Phase 11.3 — process-wide MVCC clock. Shared between every
    /// `Connection` to this `Database` (and 11.4's `MvStore`).
    /// Seeded from the WAL header's `clock_high_water` at open
    /// time so timestamps don't repeat across reopens. Allocated
    /// here even in `JournalMode::Wal` so `PRAGMA journal_mode =
    /// mvcc` doesn't require lazy-creating the clock.
    pub mvcc_clock: Arc<MvccClock>,
    /// Phase 11.3 — in-memory version index. Allocated on every
    /// `Database::new` so the toggle to MVCC mode doesn't require
    /// a re-init step. Empty until 11.4 wires the commit path to
    /// publish row versions; reads still go through the legacy
    /// path until then.
    pub mv_store: MvStore,
}

impl Database {
    /// Creates an empty in-memory `Database`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sqlrite::Database;
    /// let mut db = Database::new("my_db".to_string());
    /// ```
    pub fn new(db_name: String) -> Self {
        let mvcc_clock = Arc::new(MvccClock::new(0));
        let mv_store = MvStore::new(Arc::clone(&mvcc_clock));
        Database {
            db_name,
            tables: HashMap::new(),
            source_path: None,
            pager: None,
            txn: None,
            auto_vacuum_threshold: Some(DEFAULT_AUTO_VACUUM_THRESHOLD),
            journal_mode: JournalMode::default(),
            mvcc_clock,
            mv_store,
        }
    }

    /// Phase 11.3 — current journal mode. Toggled by `PRAGMA
    /// journal_mode = mvcc | wal`. `Wal` (the default) keeps every
    /// pre-Phase-11 caller's behaviour; `Mvcc` opts the database
    /// into MVCC + `BEGIN CONCURRENT` (Phase 11.4 wires this end-to-
    /// end; today the toggle is observable but the read/write
    /// paths don't change).
    pub fn journal_mode(&self) -> JournalMode {
        self.journal_mode
    }

    /// Phase 11.3 — switch the database's journal mode. `Wal → Mvcc`
    /// is unconditional in v0 (no in-flight transactions to drain
    /// because nothing publishes versions yet). `Mvcc → Wal` is
    /// rejected if `mv_store` carries any committed versions —
    /// switching back would silently strand them. v0 keeps this
    /// strict; the loosening (and the discard-versions path) lands
    /// when 11.4 starts populating the store.
    pub fn set_journal_mode(&mut self, mode: JournalMode) -> Result<()> {
        if self.journal_mode == mode {
            return Ok(());
        }
        if mode == JournalMode::Wal && self.mv_store.total_versions() > 0 {
            return Err(SQLRiteError::General(
                "PRAGMA journal_mode: cannot switch back to 'wal' while \
                 the MVCC store holds committed versions"
                    .to_string(),
            ));
        }
        self.journal_mode = mode;
        Ok(())
    }

    /// Phase 11.3 — the shared MVCC logical clock. Returned by
    /// reference (not cloned) because callers typically just read
    /// `now()` / `tick()` against the same `Arc` `Database` already
    /// holds.
    pub fn mvcc_clock(&self) -> &Arc<MvccClock> {
        &self.mvcc_clock
    }

    /// Phase 11.3 — the in-memory version index. Read-only access
    /// is enough for 11.3's tests; 11.4 grows the commit-path
    /// helpers into typed methods on `Database` rather than mutating
    /// this directly.
    pub fn mv_store(&self) -> &MvStore {
        &self.mv_store
    }

    /// Returns the current auto-VACUUM threshold, or `None` if disabled.
    /// See [`Database::set_auto_vacuum_threshold`] for semantics.
    pub fn auto_vacuum_threshold(&self) -> Option<f32> {
        self.auto_vacuum_threshold
    }

    /// Sets the auto-VACUUM threshold (SQLR-10). `Some(t)` with `t` in
    /// `0.0..=1.0` arms the trigger: after a page-releasing DDL
    /// commits, if the freelist exceeds `t * page_count` the engine
    /// runs a full-file compact. `None` disables the trigger. Values
    /// outside `0.0..=1.0` (or NaN / infinite) return a typed error
    /// rather than silently saturating.
    pub fn set_auto_vacuum_threshold(&mut self, threshold: Option<f32>) -> Result<()> {
        if let Some(t) = threshold {
            if !t.is_finite() || !(0.0..=1.0).contains(&t) {
                return Err(SQLRiteError::General(format!(
                    "auto_vacuum_threshold must be in 0.0..=1.0, got {t}"
                )));
            }
        }
        self.auto_vacuum_threshold = threshold;
        Ok(())
    }

    /// Returns true if the database contains a table with the specified key as a table name.
    ///
    pub fn contains_table(&self, table_name: String) -> bool {
        self.tables.contains_key(&table_name)
    }

    /// Returns an immutable reference of `sql::db::table::Table` if the database contains a
    /// table with the specified key as a table name.
    ///
    pub fn get_table(&self, table_name: String) -> Result<&Table> {
        if let Some(table) = self.tables.get(&table_name) {
            Ok(table)
        } else {
            Err(SQLRiteError::General(String::from("Table not found.")))
        }
    }

    /// Returns an mutable reference of `sql::db::table::Table` if the database contains a
    /// table with the specified key as a table name.
    ///
    pub fn get_table_mut(&mut self, table_name: String) -> Result<&mut Table> {
        if let Some(table) = self.tables.get_mut(&table_name) {
            Ok(table)
        } else {
            Err(SQLRiteError::General(String::from("Table not found.")))
        }
    }

    /// Returns `true` if this database is attached to a file and that
    /// file was opened in [`AccessMode::ReadOnly`]. In-memory databases
    /// (no pager) and read-write file-backed databases both return
    /// `false`. Callers use this to reject mutating SQL at the
    /// dispatcher level so the in-memory tables don't drift away from
    /// disk on a would-be INSERT / UPDATE / DELETE.
    pub fn is_read_only(&self) -> bool {
        self.pager
            .as_ref()
            .is_some_and(|p| p.access_mode() == AccessMode::ReadOnly)
    }

    /// Returns `true` while a `BEGIN … COMMIT`/`ROLLBACK` block is open.
    pub fn in_transaction(&self) -> bool {
        self.txn.is_some()
    }

    /// Starts a transaction: snapshots every table deep-cloned so that
    /// a later `rollback_transaction` can restore the pre-BEGIN state.
    /// Nested transactions are rejected — explicit savepoints are not
    /// on this phase's roadmap. Errors on a read-only database.
    pub fn begin_transaction(&mut self) -> Result<()> {
        if self.in_transaction() {
            return Err(SQLRiteError::General(
                "cannot BEGIN: a transaction is already open".to_string(),
            ));
        }
        if self.is_read_only() {
            return Err(SQLRiteError::General(
                "cannot BEGIN: database is opened read-only".to_string(),
            ));
        }
        let snapshot = TxnSnapshot {
            tables: self
                .tables
                .iter()
                .map(|(k, v)| (k.clone(), v.deep_clone()))
                .collect(),
        };
        self.txn = Some(snapshot);
        Ok(())
    }

    /// Drops the transaction snapshot and returns it for the caller to
    /// discard. The in-memory `tables` state is the new committed state;
    /// the caller is responsible for flushing to disk via the pager.
    /// Errors if no transaction is open.
    pub fn commit_transaction(&mut self) -> Result<()> {
        if self.txn.is_none() {
            return Err(SQLRiteError::General(
                "cannot COMMIT: no transaction is open".to_string(),
            ));
        }
        self.txn = None;
        Ok(())
    }

    /// Restores `tables` from the transaction snapshot and clears it.
    /// Errors if no transaction is open.
    pub fn rollback_transaction(&mut self) -> Result<()> {
        let Some(snapshot) = self.txn.take() else {
            return Err(SQLRiteError::General(
                "cannot ROLLBACK: no transaction is open".to_string(),
            ));
        };
        self.tables = snapshot.tables;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::dialect::SqlriteDialect;
    use crate::sql::parser::create::CreateQuery;
    use sqlparser::parser::Parser;

    #[test]
    fn new_database_create_test() {
        let db_name = String::from("my_db");
        let db = Database::new(db_name.to_string());
        assert_eq!(db.db_name, db_name);
    }

    #[test]
    fn contains_table_test() {
        let db_name = String::from("my_db");
        let mut db = Database::new(db_name.to_string());

        let query_statement = "CREATE TABLE contacts (
            id INTEGER PRIMARY KEY,
            first_name TEXT NOT NULL,
            last_name TEXT NOT NULl,
            email TEXT NOT NULL UNIQUE
        );";
        let dialect = SqlriteDialect::new();
        let mut ast = Parser::parse_sql(&dialect, query_statement).unwrap();
        if ast.len() > 1 {
            panic!("Expected a single query statement, but there are more then 1.")
        }
        let query = ast.pop().unwrap();

        let create_query = CreateQuery::new(&query).unwrap();
        let table_name = &create_query.table_name;
        db.tables
            .insert(table_name.to_string(), Table::new(create_query));

        assert!(db.contains_table("contacts".to_string()));
    }

    #[test]
    fn get_table_test() {
        let db_name = String::from("my_db");
        let mut db = Database::new(db_name.to_string());

        let query_statement = "CREATE TABLE contacts (
            id INTEGER PRIMARY KEY,
            first_name TEXT NOT NULL,
            last_name TEXT NOT NULl,
            email TEXT NOT NULL UNIQUE
        );";
        let dialect = SqlriteDialect::new();
        let mut ast = Parser::parse_sql(&dialect, query_statement).unwrap();
        if ast.len() > 1 {
            panic!("Expected a single query statement, but there are more then 1.")
        }
        let query = ast.pop().unwrap();

        let create_query = CreateQuery::new(&query).unwrap();
        let table_name = &create_query.table_name;
        db.tables
            .insert(table_name.to_string(), Table::new(create_query));

        let table = db.get_table(String::from("contacts")).unwrap();
        assert_eq!(table.columns.len(), 4);

        let table = db.get_table_mut(String::from("contacts")).unwrap();
        table.last_rowid += 1;
        assert_eq!(table.columns.len(), 4);
        assert_eq!(table.last_rowid, 1);
    }
}
