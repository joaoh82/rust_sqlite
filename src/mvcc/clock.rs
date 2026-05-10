//! [`MvccClock`] — the logical-clock primitive that hands out
//! begin- and commit-timestamps for MVCC transactions (Phase 11.2).
//!
//! Per [`docs/concurrent-writes-plan.md`](../../../docs/concurrent-writes-plan.md):
//!
//! > A monotonic `u64` counter, per-`Database`. Hands out `begin_ts`
//! > at `BEGIN CONCURRENT` and `commit_ts` at the start of validation.
//! > Wrapped in `AtomicU64`; no contention because each transaction
//! > calls it twice.
//!
//! The clock is persisted to the WAL header on each checkpoint so
//! reopens resume past the highest committed timestamp — see
//! [`crate::sql::pager::wal::WalHeader::clock_high_water`]. Without
//! persistence, two transactions on either side of a reopen could
//! receive the same timestamp and the snapshot-isolation visibility
//! rule (`begin <= ts < end`) would mis-classify one of them.

use std::sync::atomic::{AtomicU64, Ordering};

/// Process-wide logical clock. Cheap to clone — internally an `Arc`
/// over an [`AtomicU64`] in the [`Database`](crate::Database) wiring
/// (added in Phase 11.3). Standalone today.
#[derive(Debug, Default)]
pub struct MvccClock {
    counter: AtomicU64,
}

impl MvccClock {
    /// Builds a clock seeded at `initial`. The next [`MvccClock::tick`]
    /// returns `initial + 1`.
    ///
    /// Use this with the value persisted in the WAL header at open
    /// time so the clock resumes past the last-checkpointed
    /// high-water mark.
    pub fn new(initial: u64) -> Self {
        Self {
            counter: AtomicU64::new(initial),
        }
    }

    /// Returns the current high-water timestamp without advancing it.
    /// Phase 11.6's GC reads this alongside
    /// [`super::ActiveTxRegistry::min_active_begin_ts`] to decide
    /// which row-version chains are reclaimable.
    pub fn now(&self) -> u64 {
        self.counter.load(Ordering::Acquire)
    }

    /// Bumps the clock by one and returns the new value. Strictly
    /// monotonic: every call observes a distinct `u64`.
    pub fn tick(&self) -> u64 {
        // `fetch_add` returns the *previous* value — adjust to "after"
        // semantics so callers see "the timestamp this call hands out".
        // Wrap-around is impossible in practice (a billion ticks/s for
        // 600 years still fits in `u64`), so saturating-add isn't
        // needed.
        self.counter.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Promotes the clock to at least `value`. No-op if `value` is at
    /// or below the current high-water mark. Used at WAL replay to
    /// bring the in-memory clock up to the persisted high-water
    /// without an extra `tick()`.
    pub fn observe(&self, value: u64) {
        let mut current = self.counter.load(Ordering::Acquire);
        while value > current {
            // CAS rather than `store` — racing observers shouldn't
            // step on each other and shouldn't move the clock
            // backwards if a faster `tick` already overtook them.
            match self.counter.compare_exchange_weak(
                current,
                value,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn new_seeds_the_counter() {
        let c = MvccClock::new(42);
        assert_eq!(c.now(), 42);
        assert_eq!(c.tick(), 43);
        assert_eq!(c.now(), 43);
    }

    #[test]
    fn default_starts_at_zero() {
        let c = MvccClock::default();
        assert_eq!(c.now(), 0);
        assert_eq!(c.tick(), 1);
    }

    #[test]
    fn tick_is_strictly_monotonic_within_a_thread() {
        let c = MvccClock::new(0);
        let mut last = 0;
        for _ in 0..1_000 {
            let t = c.tick();
            assert!(t > last, "tick went backwards: {t} after {last}");
            last = t;
        }
    }

    #[test]
    fn observe_only_moves_forward() {
        let c = MvccClock::new(100);
        c.observe(50); // ignored — below current
        assert_eq!(c.now(), 100);
        c.observe(200);
        assert_eq!(c.now(), 200);
        c.observe(150); // ignored — below current
        assert_eq!(c.now(), 200);
    }

    /// Concurrent ticks across N threads must hand out N × M distinct
    /// values (no duplicates, no skipped values). This is the property
    /// MVCC visibility relies on.
    #[test]
    fn ticks_are_unique_under_contention() {
        const THREADS: usize = 8;
        const PER_THREAD: usize = 250;
        let clock = Arc::new(MvccClock::new(0));

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let c = Arc::clone(&clock);
                thread::spawn(move || {
                    let mut out = Vec::with_capacity(PER_THREAD);
                    for _ in 0..PER_THREAD {
                        out.push(c.tick());
                    }
                    out
                })
            })
            .collect();

        let mut all = Vec::with_capacity(THREADS * PER_THREAD);
        for h in handles {
            all.extend(h.join().unwrap());
        }
        all.sort_unstable();
        // No duplicates.
        for w in all.windows(2) {
            assert_ne!(w[0], w[1], "duplicate timestamp {}", w[0]);
        }
        // Range is contiguous 1..=THREADS*PER_THREAD (clock seeded at 0).
        assert_eq!(all.first().copied(), Some(1));
        assert_eq!(all.last().copied(), Some((THREADS * PER_THREAD) as u64));
    }

    /// Concurrent `observe`s must not move the clock backwards.
    #[test]
    fn observe_under_contention_never_regresses() {
        const THREADS: usize = 8;
        let clock = Arc::new(MvccClock::new(0));
        let handles: Vec<_> = (0..THREADS)
            .map(|tid| {
                let c = Arc::clone(&clock);
                thread::spawn(move || {
                    // Each thread observes a deterministic distinct
                    // value; the clock should end at the max.
                    c.observe((tid as u64 + 1) * 1_000);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(clock.now(), THREADS as u64 * 1_000);
    }
}
