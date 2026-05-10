//! [`ActiveTxRegistry`] — the live-transaction table that
//! garbage collection consults to know which row versions are still
//! possibly visible (Phase 11.2).
//!
//! Per [`docs/concurrent-writes-plan.md`](../../../docs/concurrent-writes-plan.md):
//!
//! > Versions whose `end` timestamp is older than the oldest active
//! > reader's begin-timestamp are dead and may be reclaimed.
//!
//! The registry is the source of "oldest active reader's
//! begin-timestamp" — [`ActiveTxRegistry::min_active_begin_ts`].
//! Phase 11.6 (GC) reads it on every sweep; Phase 11.4 (commit
//! validation) registers each `BEGIN CONCURRENT` transaction here
//! and unregisters at COMMIT/ROLLBACK.
//!
//! The current shape uses a `Mutex<BTreeMap>` for simplicity. Two
//! reasons not to over-engineer this for v0:
//!
//! 1. The map is only touched twice per transaction (begin +
//!    commit/rollback). Even a thousand concurrent writers hit a
//!    couple-thousand `lock` calls per second — well below mutex
//!    contention thresholds.
//! 2. `min_active_begin_ts` is `O(log N)` on a `BTreeMap` (the
//!    smallest key is at `iter().next()`), which is fine for the
//!    "GC asks once per sweep" use case.
//!
//! When the GC profile shows the registry on the hot path, swap to
//! a sharded skip list or an `RwLock`-protected sorted set. Until
//! then this is sufficient.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::clock::MvccClock;

/// Opaque transaction identifier. Newtype around a `u64` so a stray
/// timestamp doesn't accidentally pass as a `TxId` and vice versa.
/// Allocated by [`ActiveTxRegistry::register`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TxId(pub u64);

impl std::fmt::Display for TxId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tx{}", self.0)
    }
}

/// Tagged-union "this version is in-flight under transaction `id`"
/// vs. "this version was committed at timestamp `ts`". Per the plan,
/// row versions carry a `begin: TxTimestampOrId` so reads can ignore
/// versions belonging to still-open transactions while still seeing
/// the latest committed version.
///
/// Phase 11.4 will be the first consumer; defining it here keeps the
/// type stable across the in-flight sub-phases so callers don't have
/// to chase a moving target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxTimestampOrId {
    /// Committed timestamp — visible to any transaction whose
    /// `begin_ts >= this`.
    Timestamp(u64),
    /// In-flight transaction id — invisible to every other reader
    /// until the producing transaction commits and stamps its
    /// versions with a timestamp.
    Id(TxId),
}

/// Live-transaction table. Cheap to clone (internally `Arc`-wrapped
/// state); pass clones into worker threads.
#[derive(Clone, Debug, Default)]
pub struct ActiveTxRegistry {
    inner: Arc<Mutex<RegistryInner>>,
}

#[derive(Debug, Default)]
struct RegistryInner {
    /// `TxId` → `begin_ts`. Deterministic ordering by `TxId` (which
    /// matches allocation order) — useful for diagnostic output.
    by_id: BTreeMap<TxId, u64>,
    /// Multiset of `begin_ts` values, with a count for each. Lets
    /// `min_active_begin_ts` answer in `O(log N)` regardless of
    /// `by_id`'s size, and `unregister` just decrements the relevant
    /// counter rather than scanning. The shape matters once we have
    /// many concurrent transactions all sharing the same begin_ts
    /// (rare under MvccClock, but possible if a snapshot is taken
    /// without ticking the clock).
    by_ts: BTreeMap<u64, usize>,
}

impl ActiveTxRegistry {
    /// Creates an empty registry. Equivalent to `Default::default()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a new transaction. Allocates a fresh [`TxId`] from
    /// `clock` and snapshots the current clock value as the
    /// transaction's `begin_ts`.
    ///
    /// Returns a [`TxHandle`] — when the handle drops, the
    /// transaction is automatically unregistered. RAII keeps the
    /// "did the transaction's caller forget to clean up?" failure
    /// mode out of the cold-path code.
    pub fn register(&self, clock: &MvccClock) -> TxHandle {
        let begin_ts = clock.tick();
        let id = TxId(begin_ts);
        let mut g = self.lock();
        g.by_id.insert(id, begin_ts);
        *g.by_ts.entry(begin_ts).or_insert(0) += 1;
        drop(g);
        TxHandle {
            id,
            begin_ts,
            registry: self.clone(),
        }
    }

    /// Returns the begin-timestamp of the oldest in-flight
    /// transaction, or `None` when nothing is in flight. Phase 11.6
    /// uses this to set the GC watermark — versions whose `end`
    /// timestamp is strictly less than this value can never be seen
    /// again and may be reclaimed.
    pub fn min_active_begin_ts(&self) -> Option<u64> {
        self.lock().by_ts.keys().next().copied()
    }

    /// Number of in-flight transactions. Cheap diagnostic accessor;
    /// not load-bearing for correctness.
    pub fn active_count(&self) -> usize {
        self.lock().by_id.len()
    }

    /// Internal — dropped through [`TxHandle::drop`].
    fn unregister(&self, id: TxId, begin_ts: u64) {
        let mut g = self.lock();
        g.by_id.remove(&id);
        if let Some(slot) = g.by_ts.get_mut(&begin_ts) {
            *slot = slot.saturating_sub(1);
            if *slot == 0 {
                g.by_ts.remove(&begin_ts);
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RegistryInner> {
        self.inner
            .lock()
            .unwrap_or_else(|e| panic!("sqlrite: ActiveTxRegistry mutex poisoned: {e}"))
    }
}

/// RAII guard returned by [`ActiveTxRegistry::register`]. Dropping it
/// unregisters the transaction. A typical caller doesn't deal with it
/// explicitly — it lives on the `ConcurrentTx` struct (Phase 11.4)
/// and is dropped when the transaction commits or rolls back.
#[derive(Debug)]
pub struct TxHandle {
    id: TxId,
    begin_ts: u64,
    registry: ActiveTxRegistry,
}

impl TxHandle {
    /// The opaque identifier this transaction was allocated. Stable
    /// for the handle's lifetime.
    pub fn id(&self) -> TxId {
        self.id
    }

    /// The timestamp at which this transaction's snapshot was taken.
    /// Phase 11.3 reads use this as the visibility cutoff: a row
    /// version with `begin <= self.begin_ts() < end` is the visible
    /// one.
    pub fn begin_ts(&self) -> u64 {
        self.begin_ts
    }
}

impl Drop for TxHandle {
    fn drop(&mut self) {
        self.registry.unregister(self.id, self.begin_ts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_has_no_minimum() {
        let r = ActiveTxRegistry::new();
        assert_eq!(r.min_active_begin_ts(), None);
        assert_eq!(r.active_count(), 0);
    }

    #[test]
    fn register_advances_clock_and_updates_minimum() {
        let clock = MvccClock::new(0);
        let r = ActiveTxRegistry::new();

        let h1 = r.register(&clock);
        assert_eq!(h1.begin_ts(), 1);
        assert_eq!(r.min_active_begin_ts(), Some(1));

        let h2 = r.register(&clock);
        assert_eq!(h2.begin_ts(), 2);
        assert_eq!(r.min_active_begin_ts(), Some(1));

        // Closing the older transaction lifts the minimum.
        drop(h1);
        assert_eq!(r.min_active_begin_ts(), Some(2));

        drop(h2);
        assert_eq!(r.min_active_begin_ts(), None);
    }

    #[test]
    fn handles_carry_distinct_ids_and_unique_timestamps() {
        let clock = MvccClock::new(0);
        let r = ActiveTxRegistry::new();
        let h1 = r.register(&clock);
        let h2 = r.register(&clock);
        assert_ne!(h1.id(), h2.id());
        assert_ne!(h1.begin_ts(), h2.begin_ts());
        assert_eq!(r.active_count(), 2);
    }

    #[test]
    fn unregister_in_arbitrary_order_keeps_minimum_correct() {
        let clock = MvccClock::new(0);
        let r = ActiveTxRegistry::new();
        let h1 = r.register(&clock); // begin_ts = 1
        let h2 = r.register(&clock); // begin_ts = 2
        let h3 = r.register(&clock); // begin_ts = 3
        assert_eq!(r.min_active_begin_ts(), Some(1));

        // Drop the middle one — minimum still h1.
        drop(h2);
        assert_eq!(r.min_active_begin_ts(), Some(1));

        // Drop the oldest — minimum jumps to h3.
        drop(h1);
        assert_eq!(r.min_active_begin_ts(), Some(3));

        drop(h3);
        assert_eq!(r.min_active_begin_ts(), None);
    }

    #[test]
    fn registry_is_send_and_sync() {
        // Compile-time check — required so the registry can be cloned
        // into worker threads.
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<ActiveTxRegistry>();
        assert_sync::<ActiveTxRegistry>();
        assert_send::<TxHandle>();
        assert_sync::<TxHandle>();
    }

    /// Many concurrent registrations — every begin_ts must be unique
    /// and the registry's count must match the live handle count.
    #[test]
    fn concurrent_registrations_are_consistent() {
        use std::thread;
        const THREADS: usize = 8;
        const PER_THREAD: usize = 100;

        let clock = Arc::new(MvccClock::new(0));
        let registry = ActiveTxRegistry::new();

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let c = Arc::clone(&clock);
                let r = registry.clone();
                thread::spawn(move || {
                    let mut held: Vec<TxHandle> = Vec::with_capacity(PER_THREAD);
                    for _ in 0..PER_THREAD {
                        held.push(r.register(&c));
                    }
                    // Don't drop here — return the handles so the
                    // outer thread sees them all simultaneously alive.
                    held
                })
            })
            .collect();

        let mut all: Vec<TxHandle> = Vec::with_capacity(THREADS * PER_THREAD);
        for h in handles {
            all.extend(h.join().unwrap());
        }

        assert_eq!(registry.active_count(), THREADS * PER_THREAD);
        let begins: std::collections::BTreeSet<u64> = all.iter().map(|h| h.begin_ts()).collect();
        assert_eq!(
            begins.len(),
            THREADS * PER_THREAD,
            "every concurrent registration must allocate a unique begin_ts"
        );

        // Drop every handle — registry empties out cleanly.
        drop(all);
        assert_eq!(registry.active_count(), 0);
        assert_eq!(registry.min_active_begin_ts(), None);
    }

    #[test]
    fn tx_id_displays_with_prefix() {
        assert_eq!(format!("{}", TxId(7)), "tx7");
    }

    #[test]
    fn tx_timestamp_or_id_round_trips() {
        // Just exercise the Eq/Clone derives so a future refactor
        // that changes the variants surfaces here.
        let a = TxTimestampOrId::Timestamp(42);
        let b = TxTimestampOrId::Id(TxId(42));
        assert_ne!(a, b);
        assert_eq!(a, a);
        assert_eq!(b, b);
    }
}
