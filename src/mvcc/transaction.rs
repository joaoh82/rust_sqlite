//! [`ConcurrentTx`] — per-`Connection` `BEGIN CONCURRENT`
//! transaction state (Phase 11.4).
//!
//! Per [`docs/concurrent-writes-plan.md`](../../../docs/concurrent-writes-plan.md):
//!
//! > `BEGIN CONCURRENT` doesn't acquire any locks; writes go to the
//! > version chain tagged with the transaction id; reads use
//! > snapshot-isolation visibility.
//!
//! ## How this slice does it
//!
//! Each `Connection` owns at most one [`ConcurrentTx`] at a time.
//! When the user issues `BEGIN CONCURRENT`, the connection deep-
//! clones the database's `tables` map into `ConcurrentTx::tables`
//! and stores a [`TxHandle`] (which advances the
//! [`MvccClock`] to allocate a `begin_ts`). Subsequent `INSERT` /
//! `UPDATE` / `DELETE` statements run against the cloned `tables`
//! (the executor thinks it's writing to the live database —
//! `Connection` swaps the cloned tables in just for the duration
//! of each statement). The live `Database::tables` stays
//! unchanged until commit.
//!
//! At `COMMIT`:
//!
//! 1. Diff `tx.tables_at_begin` (the immutable BEGIN-time clone)
//!    vs `tx.tables` (post-write) to derive a write-set: every
//!    `(RowID, payload)` the transaction changed.
//! 2. For each row in the write-set, walk the
//!    [`super::MvStore`] chain. If any committed version's
//!    `begin > tx.begin_ts`, ABORT with
//!    [`crate::error::SQLRiteError::Busy`] — some other
//!    transaction touched the row after our snapshot.
//! 3. On success, allocate a `commit_ts`, push each write into
//!    the `MvStore` as a committed version (caps the previous
//!    latest version's `end` at `commit_ts`), apply the writes to
//!    `db.tables`, and run the legacy `save_database` so changes
//!    persist via the existing WAL.
//!
//! `ROLLBACK` just drops the `ConcurrentTx` — the cloned tables
//! are released, the `TxHandle` drops (unregistering the
//! transaction from `ActiveTxRegistry`), and `db.tables` is
//! unchanged because we never touched it.
//!
//! ## What this slice doesn't do (yet)
//!
//! - **Snapshot-isolated reads inside the transaction.** Reads
//!   inside `BEGIN CONCURRENT` see the cloned-at-BEGIN state of
//!   the tables (because the executor is dispatched against
//!   `tx.tables`), but they don't consult `MvStore` to filter by
//!   `begin_ts`. Concurrent writes from outside the tx land on
//!   `db.tables`, not on our snapshot — so we don't see them
//!   inside the tx. That's *partial* snapshot isolation: it
//!   isolates correctly under the current "lock the database
//!   per statement" mutex, but doesn't survive once the engine
//!   genuinely supports overlapping in-flight transactions
//!   reading concurrently.
//! - **DDL inside `BEGIN CONCURRENT`.** v0 rejects with a typed
//!   error before the swap, mirroring the plan's stated
//!   non-goal.
//! - **`AUTOINCREMENT`.** Same — rejected with a typed error.
//! - **Persistence of the in-flight write-set across crashes.**
//!   The write-set lives entirely in memory until commit. A
//!   crash mid-transaction loses everything — that's correct
//!   (the transaction never committed), and the legacy WAL
//!   still owns durability of `Database::tables` for committed
//!   data. Phase 11.5 adds the MVCC log-record frame format
//!   that lets writes start landing in the WAL pre-commit.

use std::collections::HashMap;

use crate::sql::db::table::Table;

use super::{ActiveTxRegistry, MvccClock, TxHandle};

/// Per-`Connection` snapshot of `BEGIN CONCURRENT` state.
///
/// Lives on [`Connection`](crate::Connection), not on
/// [`Database`](crate::Database) — multiple sibling connections
/// each carry their own concurrent transaction without stepping
/// on each other's snapshots.
#[derive(Debug)]
pub struct ConcurrentTx {
    /// RAII handle into the `ActiveTxRegistry`. Drops when this
    /// struct drops (commit, rollback, or `Connection` close),
    /// at which point the transaction is unregistered.
    pub handle: TxHandle,

    /// Working snapshot of `Database::tables` taken at `BEGIN
    /// CONCURRENT` via `Table::deep_clone`. Each statement's
    /// executor pass transparently swaps this in for `db.tables`
    /// so writes land here, not on the live database.
    pub tables: HashMap<String, Table>,

    /// Immutable second clone of `Database::tables` taken at
    /// `BEGIN`. Diffing `tables` against **this** at commit
    /// produces the write-set. We can't diff against the live
    /// `Database::tables` directly because between our `BEGIN`
    /// and our `COMMIT`, *other* concurrent transactions may
    /// have committed — their writes show up as differences
    /// against the live state but aren't ours, and treating
    /// them as our DELETEs would silently undo someone else's
    /// commit. The doubled memory cost (two full clones per
    /// transaction) is the price for that correctness in v0;
    /// the obvious follow-up is a per-touched-row begin-state
    /// map that captures only the rows we actually read or
    /// wrote.
    pub tables_at_begin: HashMap<String, Table>,

    /// Sorted table-name fingerprint of `Database::tables` at
    /// `BEGIN`. Used at commit to detect that DDL ran on the live
    /// database under us — v0 rejects DDL inside the tx, but
    /// nothing prevents another connection from running it
    /// outside.
    pub schema_at_begin: Vec<String>,
}

impl ConcurrentTx {
    /// Allocates a new transaction. Advances the clock by one
    /// (the `TxHandle::begin_ts`), records the table-name
    /// fingerprint, and deep-clones every table.
    ///
    /// Caller is expected to have already verified
    /// `journal_mode == Mvcc` and that no transaction is open.
    pub fn begin(
        clock: &MvccClock,
        registry: &ActiveTxRegistry,
        live_tables: &HashMap<String, Table>,
    ) -> Self {
        let handle = registry.register(clock);
        let tables: HashMap<String, Table> = live_tables
            .iter()
            .map(|(k, v)| (k.clone(), v.deep_clone()))
            .collect();
        let tables_at_begin: HashMap<String, Table> = live_tables
            .iter()
            .map(|(k, v)| (k.clone(), v.deep_clone()))
            .collect();
        let mut schema_at_begin: Vec<String> = live_tables.keys().cloned().collect();
        schema_at_begin.sort();
        Self {
            handle,
            tables,
            tables_at_begin,
            schema_at_begin,
        }
    }

    /// Convenience — the `begin_ts` snapshot timestamp this
    /// transaction took at BEGIN. Used at commit to validate
    /// against `MvStore` versions that committed after this
    /// snapshot.
    pub fn begin_ts(&self) -> u64 {
        self.handle.begin_ts()
    }

    /// True if `live_tables` has the same table-name set this
    /// transaction recorded at BEGIN. Used at commit to surface a
    /// typed error rather than silently committing onto a
    /// schema that drifted under us.
    pub fn schema_unchanged(&self, live_tables: &HashMap<String, Table>) -> bool {
        let mut current: Vec<&String> = live_tables.keys().collect();
        current.sort();
        if current.len() != self.schema_at_begin.len() {
            return false;
        }
        current
            .iter()
            .zip(self.schema_at_begin.iter())
            .all(|(a, b)| **a == *b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::db::table::Table;
    use crate::sql::parser::create::CreateQuery;
    use std::collections::HashMap;

    fn empty_table(name: &str) -> Table {
        let _ = name;
        // Build a minimal create-table to materialise a Table —
        // mirror the existing test helpers that construct via the
        // CREATE pipeline rather than poking the struct directly.
        use crate::sql::dialect::SqlriteDialect;
        use sqlparser::parser::Parser;
        let sql = format!(
            "CREATE TABLE {name} (id INTEGER PRIMARY KEY, v TEXT);",
            name = name,
        );
        let dialect = SqlriteDialect::new();
        let mut ast = Parser::parse_sql(&dialect, &sql).unwrap();
        let stmt = ast.pop().unwrap();
        let q = CreateQuery::new(&stmt).unwrap();
        Table::new(q)
    }

    fn live_with_one_table(name: &str) -> HashMap<String, Table> {
        let mut m = HashMap::new();
        m.insert(name.to_string(), empty_table(name));
        m
    }

    #[test]
    fn begin_clones_tables_and_advances_clock() {
        let clock = MvccClock::new(0);
        let registry = ActiveTxRegistry::new();
        let live = live_with_one_table("t");

        let tx = ConcurrentTx::begin(&clock, &registry, &live);
        // Clock advanced by one (begin_ts).
        assert_eq!(clock.now(), 1);
        assert_eq!(tx.begin_ts(), 1);
        // Every table cloned.
        assert!(tx.tables.contains_key("t"));
        // Schema fingerprint matches.
        assert_eq!(tx.schema_at_begin, vec!["t".to_string()]);
        // Registered with the registry.
        assert_eq!(registry.active_count(), 1);
    }

    #[test]
    fn dropping_tx_unregisters() {
        let clock = MvccClock::new(0);
        let registry = ActiveTxRegistry::new();
        let live = live_with_one_table("t");
        let tx = ConcurrentTx::begin(&clock, &registry, &live);
        assert_eq!(registry.active_count(), 1);
        drop(tx);
        assert_eq!(registry.active_count(), 0);
    }

    /// Clones really are deep — mutating the live map after
    /// `begin` doesn't show up in `tx.tables`. The contract every
    /// COMMIT-time diff relies on.
    #[test]
    fn clone_is_independent_of_live_tables() {
        let clock = MvccClock::new(0);
        let registry = ActiveTxRegistry::new();
        let mut live = live_with_one_table("t");

        let tx = ConcurrentTx::begin(&clock, &registry, &live);
        // Add a new table to live — tx's snapshot must be unchanged.
        live.insert("u".to_string(), empty_table("u"));
        assert_eq!(tx.tables.len(), 1);
        assert!(tx.tables.contains_key("t"));
        assert!(!tx.tables.contains_key("u"));
        // schema_unchanged catches the drift.
        assert!(!tx.schema_unchanged(&live));
    }

    #[test]
    fn schema_unchanged_recognises_identical_set() {
        let clock = MvccClock::new(0);
        let registry = ActiveTxRegistry::new();
        let live = live_with_one_table("t");

        let tx = ConcurrentTx::begin(&clock, &registry, &live);
        // No drift — same single table.
        assert!(tx.schema_unchanged(&live));
    }
}
