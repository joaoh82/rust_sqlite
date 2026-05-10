//! [`MvStore`] — the in-memory version index sitting in front of
//! the pager (Phase 11.3 skeleton).
//!
//! Per [`docs/concurrent-writes-plan.md`](../../../docs/concurrent-writes-plan.md):
//!
//! > The MVCC store keeps an in-memory map keyed by `RowID
//! > { table_id, row_key }` whose value is a chain of `RowVersion`
//! > records. Each version carries `begin`/`end` timestamps and the
//! > row payload itself. Visibility for a reader transaction with
//! > begin-timestamp `T` is the textbook snapshot-isolation rule:
//! > pick the version whose `begin <= T < end`.
//!
//! Phase 11.3 lands the standalone data structures + visibility
//! logic so 11.4 can plug them into:
//!
//! - the **executor's read path** when the connection is in MVCC
//!   journal mode (the [`super::JournalMode`] enum);
//! - the **commit path**, which mirrors successful writes from the
//!   legacy `Database::tables` map into the MvStore at the assigned
//!   `commit_ts` and ends the previous latest version at the same
//!   timestamp.
//!
//! Today nothing in the executor calls into this module. The
//! `PRAGMA journal_mode = mvcc` switch parses but doesn't change
//! query behaviour. That's intentional — committing to a half-wired
//! read path before the write side exists would force 11.4's
//! commit-validation work into this PR. The two are coupled and
//! ship together.
//!
//! ## Why one big mutex per chain rather than a per-row lock
//!
//! v0 stores each row's version chain inside an
//! `Arc<RwLock<Vec<RowVersion>>>`. The outer map is a
//! `Mutex<HashMap<RowID, _>>`. Two reasons not to over-engineer:
//!
//! 1. The plan-doc explicitly calls this out:
//!    > One chain per row, behind `RwLock` (or `parking_lot::RwLock`).
//!    > The wait-free chain is a known follow-up; it's not on the v0
//!    > critical path.
//! 2. The hot path is `MvStore::read`, which takes the outer lock to
//!    fetch the `Arc<RwLock<…>>`, drops it, then takes the chain's
//!    `RwLock` in read mode for the visibility scan. The outer lock
//!    is held only long enough to clone an `Arc`.
//!
//! When the commit path lands (11.4) and we observe contention, a
//! sharded outer map (e.g. `dashmap`) becomes the obvious upgrade —
//! same `RowID → chain` shape, just multiple shards. None of
//! `MvStore`'s public surface assumes the inner storage shape, so
//! the swap is local.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use crate::sql::db::table::Value;

use super::clock::MvccClock;
use super::registry::{ActiveTxRegistry, TxTimestampOrId};

/// Identifies a row across the MvStore. v0 keys by table name +
/// rowid because the engine doesn't yet have a stable numeric
/// `table_id` (the schema catalog is keyed by name). When 11.5
/// lands a numeric table id (likely as part of the checkpoint
/// integration so the index doesn't carry a `String` per row),
/// flip this to `(u32, i64)` — every consumer of `RowID` only
/// uses it for hashing / equality, so the rename is local.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RowID {
    pub table: String,
    pub rowid: i64,
}

impl RowID {
    pub fn new(table: impl Into<String>, rowid: i64) -> Self {
        Self {
            table: table.into(),
            rowid,
        }
    }
}

/// What a [`RowVersion`] records. `Present` carries the row's
/// column values at the moment of commit; `Tombstone` records that
/// the row was deleted at this version's `begin` timestamp.
///
/// Storing column-value pairs as a `Vec<(String, Value)>` rather
/// than `BTreeMap<String, Value>` because:
/// - The vector preserves declaration order (stable for tests +
///   diagnostics).
/// - Lookups by column are rare on this path — the executor walks
///   the row by projection order.
#[derive(Debug, Clone, PartialEq)]
pub enum VersionPayload {
    /// Row exists with the given column-value pairs.
    Present(Vec<(String, Value)>),
    /// Row was deleted at this version's `begin` timestamp. Visible
    /// readers see "no such row"; readers older than `begin` still
    /// see whatever the previous version held.
    Tombstone,
}

/// One link in a row's version chain.
///
/// Visibility under snapshot isolation is the textbook rule the
/// Hekaton paper formalises and Turso's MVCC implements:
///
/// - `begin <= T`: the version was committed at or before the
///   reader's begin-timestamp. (For an in-flight version
///   `begin = Id(tx)`, only the producing transaction can see it.)
/// - `end > T` or `end is None`: the version hasn't been superseded
///   yet from the reader's point of view.
///
/// Both conditions must hold. See [`MvStore::visible_at`].
#[derive(Debug, Clone)]
pub struct RowVersion {
    pub begin: TxTimestampOrId,
    pub end: Option<TxTimestampOrId>,
    pub payload: VersionPayload,
}

impl RowVersion {
    /// Builds a freshly-committed version at `commit_ts` with no
    /// `end` (i.e. currently latest). This is the shape the legacy
    /// commit path produces in 11.4 when it mirrors a row write.
    pub fn committed(commit_ts: u64, payload: VersionPayload) -> Self {
        Self {
            begin: TxTimestampOrId::Timestamp(commit_ts),
            end: None,
            payload,
        }
    }

    /// Builds an in-flight version owned by `tx_id`. v0 tests use
    /// this to construct chains by hand; the production write path
    /// (11.4) will own it.
    pub fn in_flight(tx_id: super::TxId, payload: VersionPayload) -> Self {
        Self {
            begin: TxTimestampOrId::Id(tx_id),
            end: None,
            payload,
        }
    }
}

/// A row's version chain. Newest version at the back — easy
/// `push_version` semantics; reads scan from the back since that's
/// where most queries' `begin_ts` lands.
pub type RowVersionChain = Vec<RowVersion>;

/// In-memory MVCC version index. Cheap to clone — the heavy state
/// is behind `Arc`s.
#[derive(Clone, Debug)]
pub struct MvStore {
    inner: Arc<MvStoreInner>,
}

#[derive(Debug)]
struct MvStoreInner {
    /// `RowID → version chain`. Outer `Mutex` guards the map's
    /// shape (insert / lookup); the per-chain `RwLock` guards the
    /// `Vec` (so two readers walking different chains don't fight,
    /// and the writer that ends the latest version doesn't block
    /// readers on other chains).
    versions: Mutex<HashMap<RowID, Arc<RwLock<RowVersionChain>>>>,
    clock: Arc<MvccClock>,
    active: ActiveTxRegistry,
}

impl MvStore {
    /// Builds an empty store wired to a shared clock + registry.
    /// Phase 11.3 wires this into `Database` so every connection
    /// observes the same version index; 11.2's `Wal::clock_high_water`
    /// seeds the clock at open time.
    pub fn new(clock: Arc<MvccClock>) -> Self {
        Self {
            inner: Arc::new(MvStoreInner {
                versions: Mutex::new(HashMap::new()),
                clock,
                active: ActiveTxRegistry::new(),
            }),
        }
    }

    /// Convenience for tests + standalone callers — builds a store
    /// over a freshly-allocated clock seeded at 0. The clock is
    /// returned so the caller can `tick()` it to allocate
    /// timestamps for hand-built versions.
    pub fn fresh() -> (Self, Arc<MvccClock>) {
        let clock = Arc::new(MvccClock::new(0));
        let store = Self::new(Arc::clone(&clock));
        (store, clock)
    }

    /// Returns the shared clock. The same `Arc` every consumer
    /// (commit path, read path, GC) holds.
    pub fn clock(&self) -> &Arc<MvccClock> {
        &self.inner.clock
    }

    /// Returns the active-transaction registry. Phase 11.4 will
    /// register `BEGIN CONCURRENT` transactions here; Phase 11.6
    /// reads `min_active_begin_ts()` to set the GC watermark.
    pub fn active_registry(&self) -> &ActiveTxRegistry {
        &self.inner.active
    }

    /// Number of rows the store holds at least one version for.
    /// Cheap diagnostic — locks only the outer map briefly.
    pub fn tracked_rows(&self) -> usize {
        self.lock_map().len()
    }

    /// Total versions across every chain. Linear in row count;
    /// intended for tests + assertions, not the hot path.
    pub fn total_versions(&self) -> usize {
        let map = self.lock_map();
        map.values()
            .map(|chain| chain.read().expect("chain RwLock poisoned").len())
            .sum()
    }

    /// Returns the version of `row_id` that's visible to a reader
    /// transaction whose begin-timestamp is `begin_ts`, or `None`
    /// if no version satisfies the snapshot-isolation rule.
    ///
    /// Snapshot-isolation visibility:
    /// - the version's `begin` is a committed timestamp `<= begin_ts`,
    ///   and
    /// - the version's `end` is `None` (still latest) or a committed
    ///   timestamp `> begin_ts`.
    ///
    /// In-flight versions (`begin = Id(_)`) are never visible to
    /// other readers — they're a placeholder until the producing
    /// transaction either commits (the version's `begin` is rewritten
    /// to a `Timestamp`) or aborts (the version is dropped). The
    /// producing transaction itself reads its own writes through a
    /// separate path (Phase 11.4); it doesn't go through this
    /// function.
    ///
    /// The chain is scanned **front to back**: in v0 we don't trust
    /// any insertion order, so the loop must not exit early. When
    /// the chain becomes ordered-by-`begin` (a natural property of
    /// the commit path's append-only writes in 11.4), this can
    /// short-circuit on the first visible version.
    pub fn read(&self, row_id: &RowID, begin_ts: u64) -> Option<VersionPayload> {
        let chain = {
            let map = self.lock_map();
            Arc::clone(map.get(row_id)?)
        };
        let chain = chain.read().expect("chain RwLock poisoned");
        for v in chain.iter() {
            if Self::visible_at(v, begin_ts) {
                return Some(v.payload.clone());
            }
        }
        None
    }

    /// Returns true if `version` is visible to a reader whose
    /// begin-timestamp is `begin_ts`. Pure function — exposed for
    /// tests + future GC code.
    pub fn visible_at(version: &RowVersion, begin_ts: u64) -> bool {
        // begin must be a committed timestamp <= begin_ts.
        let begin_ok = match version.begin {
            TxTimestampOrId::Timestamp(t) => t <= begin_ts,
            TxTimestampOrId::Id(_) => false,
        };
        if !begin_ok {
            return false;
        }
        // end must be None (still latest) OR a committed timestamp
        // strictly > begin_ts. An in-flight `Id(_)` cap means some
        // other transaction is in the process of superseding this
        // version but hasn't committed yet — from the reader's
        // perspective the version is still latest.
        match version.end {
            None => true,
            Some(TxTimestampOrId::Timestamp(t)) => t > begin_ts,
            Some(TxTimestampOrId::Id(_)) => true,
        }
    }

    /// Pushes a new version onto the chain for `row_id`. Caps the
    /// chain's previous latest version (if any) at `version.begin`
    /// — the canonical write-side bookkeeping the commit path will
    /// use in 11.4.
    ///
    /// `version.begin` must be a `Timestamp` (committed) — pushing
    /// an in-flight version through this entry point would break
    /// the cap rule. Use [`MvStore::push_in_flight`] for in-flight
    /// versions; commit will rewrite their `begin` later.
    ///
    /// Errors if the new `begin` is `<= the previous latest's
    /// begin` (violates monotonicity — the commit path must always
    /// hand out increasing timestamps via the `MvccClock`).
    pub fn push_committed(&self, row_id: RowID, version: RowVersion) -> Result<(), MvStoreError> {
        let begin_ts = match version.begin {
            TxTimestampOrId::Timestamp(t) => t,
            TxTimestampOrId::Id(_) => return Err(MvStoreError::NotCommitted),
        };
        let chain_arc = self.get_or_create_chain(row_id);
        let mut chain = chain_arc.write().expect("chain RwLock poisoned");
        if let Some(prev) = chain.last() {
            // Validate before mutating — a failed validation must
            // not leave the chain in a half-capped state. (Earlier
            // drafts mutated `prev.end` first, then ran these
            // checks; equal-begin retries then surfaced as
            // `PreviousAlreadyCapped` instead of the
            // `NonMonotonicBegin` callers expect.)
            let prev_begin = match prev.begin {
                TxTimestampOrId::Timestamp(t) => t,
                TxTimestampOrId::Id(_) => 0,
            };
            if begin_ts <= prev_begin {
                return Err(MvStoreError::NonMonotonicBegin {
                    prev: prev_begin,
                    new: begin_ts,
                });
            }
            match prev.end {
                None => {}
                Some(TxTimestampOrId::Timestamp(existing)) if existing == begin_ts => {
                    // Idempotent replay — already capped at exactly
                    // this timestamp (recovery path will hit this).
                }
                Some(TxTimestampOrId::Timestamp(existing)) => {
                    return Err(MvStoreError::PreviousAlreadyCapped { existing });
                }
                Some(TxTimestampOrId::Id(_)) => {
                    // An in-flight cap means another transaction
                    // owns the supersession; the commit path
                    // shouldn't hit this in 11.4 (validation runs
                    // first). v0 returns a typed error rather than
                    // silently overwriting.
                    return Err(MvStoreError::PreviousCappedByInFlight);
                }
            }
        }
        // Validation passed — apply the cap (if any) and push.
        if let Some(prev) = chain.last_mut() {
            if prev.end.is_none() {
                prev.end = Some(TxTimestampOrId::Timestamp(begin_ts));
            }
        }
        chain.push(version);
        Ok(())
    }

    /// Pushes an in-flight version onto the chain. Used by the
    /// 11.4 write path while a `BEGIN CONCURRENT` transaction is
    /// open; the version's `begin` is rewritten from `Id(tx)` to
    /// `Timestamp(commit_ts)` on commit, and the previous latest
    /// gets capped at the same timestamp (via [`Self::push_committed`]
    /// at commit time, after the in-flight version is removed).
    ///
    /// 11.3 ships this as standalone API for tests; 11.4 wires it
    /// into the executor.
    pub fn push_in_flight(&self, row_id: RowID, version: RowVersion) {
        let chain_arc = self.get_or_create_chain(row_id);
        let mut chain = chain_arc.write().expect("chain RwLock poisoned");
        chain.push(version);
    }

    fn get_or_create_chain(&self, row_id: RowID) -> Arc<RwLock<RowVersionChain>> {
        let mut map = self.lock_map();
        Arc::clone(
            map.entry(row_id)
                .or_insert_with(|| Arc::new(RwLock::new(Vec::new()))),
        )
    }

    fn lock_map(&self) -> std::sync::MutexGuard<'_, HashMap<RowID, Arc<RwLock<RowVersionChain>>>> {
        self.inner
            .versions
            .lock()
            .unwrap_or_else(|e| panic!("sqlrite: MvStore versions mutex poisoned: {e}"))
    }
}

/// Errors returned by mutating MvStore operations. Read-side calls
/// (`read`, `visible_at`) don't error.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum MvStoreError {
    /// `push_committed` got a version whose `begin` is an in-flight
    /// `TxId` rather than a committed `Timestamp`.
    #[error("push_committed expects a committed Timestamp, not an in-flight TxId")]
    NotCommitted,

    /// The previous latest version is already capped at a different
    /// timestamp. Either the caller is double-committing, or the
    /// commit path is racing with itself (which 11.4's commit-validation
    /// loop is supposed to prevent).
    #[error("previous latest version already capped at end_ts={existing}")]
    PreviousAlreadyCapped { existing: u64 },

    /// The previous latest's `end` is set to an in-flight cap. v0
    /// rejects rather than silently overwriting; 11.4's commit
    /// validation runs first so this shouldn't fire in production.
    #[error("previous latest version is being capped by an in-flight transaction")]
    PreviousCappedByInFlight,

    /// New version's `begin` is not strictly greater than the
    /// previous latest's `begin`. The clock should always hand out
    /// monotonically increasing timestamps; this is a corruption /
    /// bug indicator.
    #[error("non-monotonic begin: previous={prev}, new={new}")]
    NonMonotonicBegin { prev: u64, new: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(value: i64) -> VersionPayload {
        VersionPayload::Present(vec![("v".to_string(), Value::Integer(value))])
    }

    #[test]
    fn empty_store_returns_none() {
        let (store, _clock) = MvStore::fresh();
        assert!(store.read(&RowID::new("t", 1), 100).is_none());
        assert_eq!(store.tracked_rows(), 0);
        assert_eq!(store.total_versions(), 0);
    }

    /// Snapshot isolation visibility — the headline rule. One row
    /// gets two committed versions at different timestamps; readers
    /// at varying `begin_ts` see exactly the version that satisfies
    /// `begin <= T < end`.
    #[test]
    fn visibility_picks_the_right_version_for_each_begin_ts() {
        let (store, clock) = MvStore::fresh();
        let row = RowID::new("accounts", 1);

        // V1 committed at ts=5, V2 committed at ts=10.
        clock.observe(5);
        store
            .push_committed(row.clone(), RowVersion::committed(5, payload(100)))
            .unwrap();
        clock.observe(10);
        store
            .push_committed(row.clone(), RowVersion::committed(10, payload(200)))
            .unwrap();

        // Reader before V1 — nothing visible.
        assert_eq!(store.read(&row, 4), None);

        // Reader at exactly V1's begin — sees V1.
        assert_eq!(store.read(&row, 5), Some(payload(100)));

        // Reader between V1 and V2 — still sees V1 (V2's begin > T).
        assert_eq!(store.read(&row, 9), Some(payload(100)));

        // Reader at exactly V2's begin — sees V2.
        assert_eq!(store.read(&row, 10), Some(payload(200)));

        // Reader past V2 — sees V2.
        assert_eq!(store.read(&row, 1_000), Some(payload(200)));
    }

    /// `push_committed` caps the previous latest version's `end` at
    /// the new version's `begin`. Without this, every version's
    /// `end` would stay None and the visibility rule would return
    /// the oldest committed version for every reader.
    #[test]
    fn push_committed_caps_previous_latest() {
        let (store, _clock) = MvStore::fresh();
        let row = RowID::new("t", 7);
        store
            .push_committed(row.clone(), RowVersion::committed(2, payload(1)))
            .unwrap();
        store
            .push_committed(row.clone(), RowVersion::committed(5, payload(2)))
            .unwrap();
        // Inspect the chain through the public API. A reader at
        // exactly ts=4 should see V1 — that's only correct if V1's
        // end was set to Some(Timestamp(5)).
        assert_eq!(store.read(&row, 4), Some(payload(1)));
    }

    /// The visibility helper is pure; test it independently of
    /// the chain to lock down the rule.
    #[test]
    fn visible_at_handles_each_combination() {
        // Committed begin, no end — visible iff T >= begin.
        let v = RowVersion {
            begin: TxTimestampOrId::Timestamp(10),
            end: None,
            payload: payload(0),
        };
        assert!(!MvStore::visible_at(&v, 9));
        assert!(MvStore::visible_at(&v, 10));
        assert!(MvStore::visible_at(&v, 1_000));

        // Committed begin + committed end — visible iff begin <= T < end.
        let v = RowVersion {
            begin: TxTimestampOrId::Timestamp(10),
            end: Some(TxTimestampOrId::Timestamp(20)),
            payload: payload(0),
        };
        assert!(!MvStore::visible_at(&v, 9));
        assert!(MvStore::visible_at(&v, 10));
        assert!(MvStore::visible_at(&v, 19));
        assert!(!MvStore::visible_at(&v, 20));

        // In-flight begin — invisible to outside readers regardless
        // of `end`.
        let v = RowVersion {
            begin: TxTimestampOrId::Id(super::super::TxId(42)),
            end: None,
            payload: payload(0),
        };
        assert!(!MvStore::visible_at(&v, 0));
        assert!(!MvStore::visible_at(&v, 1_000));

        // In-flight cap on an otherwise-visible version — still
        // visible (the supersession isn't committed yet).
        let v = RowVersion {
            begin: TxTimestampOrId::Timestamp(5),
            end: Some(TxTimestampOrId::Id(super::super::TxId(42))),
            payload: payload(0),
        };
        assert!(MvStore::visible_at(&v, 10));
        assert!(!MvStore::visible_at(&v, 4)); // begin > T
    }

    /// Tombstone semantics: deleting the row creates a Tombstone
    /// version. Readers older than the delete still see the value
    /// from the previous version; readers at or after the delete
    /// see "no row" (the tombstone payload).
    #[test]
    fn tombstone_versions_capture_the_delete() {
        let (store, _clock) = MvStore::fresh();
        let row = RowID::new("t", 1);
        store
            .push_committed(row.clone(), RowVersion::committed(1, payload(42)))
            .unwrap();
        store
            .push_committed(
                row.clone(),
                RowVersion::committed(5, VersionPayload::Tombstone),
            )
            .unwrap();

        assert_eq!(store.read(&row, 1), Some(payload(42)));
        assert_eq!(store.read(&row, 4), Some(payload(42)));
        assert_eq!(store.read(&row, 5), Some(VersionPayload::Tombstone));
        assert_eq!(store.read(&row, 100), Some(VersionPayload::Tombstone));
    }

    #[test]
    fn push_committed_rejects_in_flight_begin() {
        let (store, _clock) = MvStore::fresh();
        let v = RowVersion::in_flight(super::super::TxId(7), payload(0));
        let err = store
            .push_committed(RowID::new("t", 1), v)
            .expect_err("in-flight begin must be rejected");
        assert_eq!(err, MvStoreError::NotCommitted);
    }

    #[test]
    fn push_committed_rejects_non_monotonic_begin() {
        let (store, _clock) = MvStore::fresh();
        let row = RowID::new("t", 1);
        store
            .push_committed(row.clone(), RowVersion::committed(10, payload(1)))
            .unwrap();
        let err = store
            .push_committed(row.clone(), RowVersion::committed(10, payload(2)))
            .expect_err("equal begin should be rejected");
        assert!(matches!(err, MvStoreError::NonMonotonicBegin { .. }));
        let err = store
            .push_committed(row.clone(), RowVersion::committed(5, payload(2)))
            .expect_err("backward begin should be rejected");
        assert!(matches!(err, MvStoreError::NonMonotonicBegin { .. }));
    }

    /// In-flight versions don't appear to other readers — the
    /// snapshot-isolation contract Phase 11.4 relies on. Other
    /// readers see the previously-committed version (or None if
    /// the chain is empty otherwise).
    #[test]
    fn in_flight_versions_are_invisible_to_other_readers() {
        let (store, _clock) = MvStore::fresh();
        let row = RowID::new("t", 1);
        store
            .push_committed(row.clone(), RowVersion::committed(5, payload(100)))
            .unwrap();
        // Simulate an in-flight write at a higher (uncommitted)
        // timestamp via a fresh TxId. Reader at any begin_ts must
        // still see V1.
        store.push_in_flight(
            row.clone(),
            RowVersion::in_flight(super::super::TxId(99), payload(200)),
        );
        assert_eq!(store.read(&row, 5), Some(payload(100)));
        assert_eq!(store.read(&row, 1_000), Some(payload(100)));
    }

    /// Tracked-row + version counters reflect the chain shape.
    /// Cheap sanity test that 11.6's GC will rely on once it lands.
    #[test]
    fn tracked_rows_and_total_versions_are_accurate() {
        let (store, _clock) = MvStore::fresh();
        store
            .push_committed(RowID::new("a", 1), RowVersion::committed(1, payload(0)))
            .unwrap();
        store
            .push_committed(RowID::new("a", 1), RowVersion::committed(2, payload(0)))
            .unwrap();
        store
            .push_committed(RowID::new("a", 2), RowVersion::committed(1, payload(0)))
            .unwrap();
        store
            .push_committed(RowID::new("b", 1), RowVersion::committed(1, payload(0)))
            .unwrap();
        assert_eq!(store.tracked_rows(), 3);
        assert_eq!(store.total_versions(), 4);
    }

    #[test]
    fn store_is_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<MvStore>();
        assert_sync::<MvStore>();
    }

    /// Concurrent readers walking different chains must not block
    /// each other — that's the reason for the per-chain `RwLock`
    /// rather than one big `Mutex<HashMap>`. Smoke test: many
    /// threads read concurrently and must all see the right
    /// versions.
    #[test]
    fn concurrent_reads_see_consistent_snapshots() {
        use std::thread;

        let (store, _clock) = MvStore::fresh();
        for rid in 0..32 {
            let row = RowID::new("t", rid);
            store
                .push_committed(row.clone(), RowVersion::committed(1, payload(rid)))
                .unwrap();
            store
                .push_committed(row, RowVersion::committed(10, payload(rid * 100)))
                .unwrap();
        }

        let store_arc = Arc::new(store);
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let s = Arc::clone(&store_arc);
                thread::spawn(move || {
                    for _ in 0..500 {
                        for rid in 0..32 {
                            let row = RowID::new("t", rid);
                            // Pre-supersession: V1 visible.
                            assert_eq!(s.read(&row, 5), Some(payload(rid)));
                            // Post-supersession: V2 visible.
                            assert_eq!(s.read(&row, 100), Some(payload(rid * 100)));
                        }
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    }

    /// The store's clock is the same `Arc` callers handed in — a
    /// later 11.3 wiring change in `Database` relies on this.
    #[test]
    fn store_shares_caller_clock() {
        let clock = Arc::new(MvccClock::new(42));
        let store = MvStore::new(Arc::clone(&clock));
        assert_eq!(store.clock().now(), 42);
        clock.tick(); // clock.tick now == 43
        assert_eq!(store.clock().now(), 43);
    }
}
