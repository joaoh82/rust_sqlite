//! Page allocator for `save_database` (SQLR-6).
//!
//! Replaces the bare `next_free_page: u32` counter that the staging code
//! used to thread through every `stage_*_btree` function. The allocator
//! draws from three pools, in order of preference:
//!
//! 1. **Per-table preferred pool** — pages the table previously occupied,
//!    seeded by [`set_preferred`]. An unchanged table's stage produces
//!    byte-identical pages at the same numbers, so the diff pager skips
//!    every write for it.
//! 2. **Global freelist** — pages dropped tables/indexes used to occupy
//!    plus the trunk pages of the previously-persisted freelist.
//! 3. **Extend** — `next_extend++`, monotonic past the current high water.
//!
//! After staging finishes, [`high_water`] is the new `page_count` and
//! [`used`] enumerates every page actually written this save (so the
//! caller can compute the new freelist as `old_live − used`).

use std::collections::{HashSet, VecDeque};

/// Hands out page numbers during a save.
///
/// Lifetime: one allocator per `save_database` call. Not thread-safe; not
/// shared across saves.
pub struct PageAllocator {
    /// Pages available globally. Drained after the per-table pool is empty.
    /// Stored as a VecDeque so callers can append (push_back) and we always
    /// hand them out front-first for ascending-order determinism.
    freelist: VecDeque<u32>,
    /// The current table's preferred pool (its previous-rootpage pages).
    /// Drained before [`freelist`]. Cleared between tables by
    /// [`finish_preferred`].
    preferred: VecDeque<u32>,
    /// Next page number for fresh extension. Page 0 is the header, so
    /// the first alloc always returns ≥ 1.
    next_extend: u32,
    /// Every page handed out this save. Used to compute the newly-freed
    /// set after staging completes.
    used: HashSet<u32>,
}

impl PageAllocator {
    /// `freelist` carries the pages from the previously-persisted
    /// freelist (sorted ascending by the caller). `next_extend` is
    /// typically `1` for a brand-new save.
    pub fn new(freelist: VecDeque<u32>, next_extend: u32) -> Self {
        let mut alloc = Self {
            freelist,
            preferred: VecDeque::new(),
            next_extend,
            used: HashSet::new(),
        };
        // Defensive: a corrupt freelist could push the high-water mark
        // higher than `next_extend` claims. Bump so we never hand out a
        // duplicate page on extend.
        let max_free = alloc.freelist.iter().copied().max().unwrap_or(0);
        if max_free + 1 > alloc.next_extend {
            alloc.next_extend = max_free + 1;
        }
        alloc
    }

    /// Seeds the per-table preferred pool. Drained on subsequent
    /// [`allocate`] calls before any other source.
    pub fn set_preferred(&mut self, mut pool: Vec<u32>) {
        // Sort ascending so the order matches the linear staging order
        // and unchanged tables get byte-identical leaves.
        pool.sort_unstable();
        pool.dedup();
        // Filter out anything the allocator has already handed out
        // (defensive — shouldn't happen but keeps the invariant tidy).
        pool.retain(|p| !self.used.contains(p));
        self.preferred = VecDeque::from(pool);
    }

    /// Empties the per-table preferred pool, returning any leftover
    /// pages to the global freelist (they're now free again).
    pub fn finish_preferred(&mut self) {
        while let Some(p) = self.preferred.pop_front() {
            if !self.used.contains(&p) {
                self.freelist.push_back(p);
            }
        }
    }

    /// Returns the next page to write. Picks from preferred → freelist →
    /// extend. Records the result in `used` and bumps `next_extend` if
    /// the page came from one of the pools and was past the current
    /// high water.
    pub fn allocate(&mut self) -> u32 {
        let page = if let Some(p) = self.preferred.pop_front() {
            p
        } else if let Some(p) = self.freelist.pop_front() {
            p
        } else {
            let p = self.next_extend;
            self.next_extend += 1;
            p
        };
        if page >= self.next_extend {
            self.next_extend = page + 1;
        }
        // A double-allocation is an internal bug; assert in debug.
        debug_assert!(
            !self.used.contains(&page),
            "PageAllocator handed out page {page} twice"
        );
        self.used.insert(page);
        page
    }

    /// Adds pages to the global freelist. Used to drop pages that the
    /// caller traversed but didn't end up restaging (e.g., a dropped
    /// table's leaves; the previous freelist's trunk pages).
    ///
    /// Bumps `next_extend` past any added page so the final page_count
    /// covers freelist trunks even if they live past the highest used
    /// payload page.
    pub fn add_to_freelist(&mut self, pages: impl IntoIterator<Item = u32>) {
        for p in pages {
            // Skip pages already used (we already restaged them) or
            // already on the list.
            if !self.used.contains(&p) && !self.freelist.contains(&p) {
                self.freelist.push_back(p);
                if p + 1 > self.next_extend {
                    self.next_extend = p + 1;
                }
            }
        }
    }

    /// Page-count to publish in the new header. Equal to
    /// `1 + max page handed out` after staging.
    pub fn high_water(&self) -> u32 {
        self.next_extend
    }

    /// Every page handed out this save.
    pub fn used(&self) -> &HashSet<u32> {
        &self.used
    }

    /// Snapshot of pages still on the global freelist (i.e., free pages
    /// that need to be persisted into trunk pages). Sorted ascending so
    /// the encoded freelist trunks are deterministic.
    pub fn drain_freelist(&mut self) -> Vec<u32> {
        let mut v: Vec<u32> = self.freelist.drain(..).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_extends_when_pools_empty() {
        let mut a = PageAllocator::new(VecDeque::new(), 1);
        assert_eq!(a.allocate(), 1);
        assert_eq!(a.allocate(), 2);
        assert_eq!(a.allocate(), 3);
        assert_eq!(a.high_water(), 4);
    }

    #[test]
    fn preferred_pool_drains_first() {
        let mut a = PageAllocator::new(VecDeque::from([8, 9]), 1);
        a.set_preferred(vec![3, 4]);
        assert_eq!(a.allocate(), 3);
        assert_eq!(a.allocate(), 4);
        // After preferred drains, freelist takes over.
        assert_eq!(a.allocate(), 8);
        assert_eq!(a.allocate(), 9);
        // Then extend.
        assert_eq!(a.allocate(), 10);
    }

    #[test]
    fn freelist_drains_after_preferred() {
        let mut a = PageAllocator::new(VecDeque::from([5, 7]), 1);
        assert_eq!(a.allocate(), 5);
        assert_eq!(a.allocate(), 7);
        assert_eq!(a.allocate(), 8); // extend bumped because max free was 7
    }

    #[test]
    fn finish_preferred_returns_leftovers_to_freelist() {
        let mut a = PageAllocator::new(VecDeque::new(), 1);
        a.set_preferred(vec![3, 4, 5]);
        assert_eq!(a.allocate(), 3); // used 3
        a.finish_preferred();
        // Now 4 and 5 should be on the freelist.
        assert_eq!(a.allocate(), 4);
        assert_eq!(a.allocate(), 5);
    }

    #[test]
    fn add_to_freelist_skips_already_used() {
        let mut a = PageAllocator::new(VecDeque::new(), 1);
        let p = a.allocate(); // 1
        a.add_to_freelist([p, 5, 6]);
        let drained = a.drain_freelist();
        assert!(
            !drained.contains(&p),
            "used page should not land on freelist"
        );
        assert_eq!(drained, vec![5, 6]);
    }

    #[test]
    fn next_extend_respects_max_free() {
        // High pages on the freelist should bump next_extend so the
        // allocator never collides with them on extend.
        let mut a = PageAllocator::new(VecDeque::from([100]), 1);
        // First alloc draws from freelist.
        assert_eq!(a.allocate(), 100);
        // Subsequent extend lands at 101, not 1.
        assert_eq!(a.allocate(), 101);
    }
}
