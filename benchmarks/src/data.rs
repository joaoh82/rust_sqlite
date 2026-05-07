//! Deterministic dataset generators.
//!
//! Every workload pulls its inputs from this module. The seed is part
//! of the workload's contract — a `W1.v1` run reproduces byte-for-byte
//! across hosts. Bumping a workload's version (`v1` → `v2`) is the
//! explicit gesture for "I changed the dataset shape" (see Q8 in
//! `benchmarks-plan.md`).
//!
//! Why not generate into `benchmarks/data/`? — small datasets like W1's
//! 100k rows regenerate in milliseconds, so we re-build them per-run
//! from the seed. Larger workloads (W7's 1M-row aggregate) might need
//! the on-disk cache; that lands alongside W7 in 9.3.

use rand::Rng;
use rand::SeedableRng;
use rand::seq::SliceRandom;
use rand_chacha::ChaCha8Rng;

/// W1 dataset: 100k rows of `(id INTEGER PRIMARY KEY, name TEXT, payload TEXT)`.
///
/// `name` is `"user_<id>"`. `payload` is a deterministic 64-char string
/// — enough to push each row out of "fits in the i64 PK" territory and
/// exercise the row reassembly path, but not large enough to dominate
/// the comparison with disk I/O.
///
/// The `keys` slice is a shuffled permutation of `1..=100_000` —
/// criterion's hot loop picks `keys[i % keys.len()]` per iteration so
/// every probe is to a present row but lookup order is unpredictable
/// (no monotone B-tree leaf walk gaming the cache).
pub struct W1Dataset {
    pub rows: Vec<W1Row>,
    pub keys: Vec<i64>,
}

pub struct W1Row {
    pub id: i64,
    pub name: String,
    pub payload: String,
}

/// Total rows in the W1 dataset. Public so the workload + the README's
/// table can reference the same number without drift.
pub const W1_ROW_COUNT: usize = 100_000;

/// Per-iteration probe count. The bench loop reads one row per iter,
/// then criterion estimates per-iter latency from many iters; this
/// constant is just the size of the prebuilt random-key slice the
/// loop indexes into — picking it large enough to avoid cache games
/// without bloating memory.
pub const W1_KEY_COUNT: usize = 10_000;

const W1_SEED: u64 = 42;

/// Build the W1 dataset deterministically. Cheap (~30 ms for 100k
/// rows on an M-series MBP); regenerated per bench-process so we
/// don't carry a `benchmarks/data/` cache in v1.
pub fn w1_dataset() -> W1Dataset {
    let mut rng = ChaCha8Rng::seed_from_u64(W1_SEED);

    let mut rows = Vec::with_capacity(W1_ROW_COUNT);
    for i in 1..=W1_ROW_COUNT as i64 {
        rows.push(W1Row {
            id: i,
            name: format!("user_{i}"),
            payload: payload_for(i),
        });
    }

    let mut keys: Vec<i64> = (1..=W1_ROW_COUNT as i64).collect();
    keys.shuffle(&mut rng);
    keys.truncate(W1_KEY_COUNT);

    W1Dataset { rows, keys }
}

/// Dataset for the Group-A "secondary-indexed" workloads: W2 (range
/// scan), W3 (bulk insert), W5 (mixed OLTP), W6 (secondary-index
/// lookup).
///
/// Schema:
///
/// ```sql
/// CREATE TABLE kv2 (
///   id        INTEGER PRIMARY KEY,
///   secondary INTEGER,
///   payload   TEXT
/// );
/// CREATE UNIQUE INDEX idx_kv2_secondary ON kv2(secondary);
/// ```
///
/// `secondary` is a deterministic permutation of `1..=100_000`, so:
/// - **W6** secondary-index probes are unique-row lookups on a non-PK
///   index (every probe hits exactly one row).
/// - **W2** ranges of width N over `secondary` hit exactly N rows (not
///   ±a few, since the values densely cover `1..=100_000`).
///
/// `pk_probes`, `secondary_probes`, and the three `range_probes_*`
/// slices are pre-shuffled in `setup` and reused across the bench's
/// hot loop — same pattern as W1's `keys`.
pub struct GroupADataset {
    pub rows: Vec<GroupARow>,
    /// Random PK probes — used by W5 (mixed OLTP) and indirectly by
    /// any future workload doing PK lookups against this dataset.
    pub pk_probes: Vec<i64>,
    /// Random `secondary`-value probes — used by W6 for secondary-
    /// index lookup.
    pub secondary_probes: Vec<i64>,
    /// Random `(lo, hi)` ranges of width 100 over `secondary` — W2.
    pub range_probes_100: Vec<(i64, i64)>,
    /// Random `(lo, hi)` ranges of width 1k — W2.
    pub range_probes_1k: Vec<(i64, i64)>,
    /// Random `(lo, hi)` ranges of width 10k — W2.
    pub range_probes_10k: Vec<(i64, i64)>,
}

pub struct GroupARow {
    pub id: i64,
    pub secondary: i64,
    pub payload: String,
}

/// Total rows in the Group-A dataset. Same scale as W1 so the
/// SELECT-by-PK numbers between the two workloads are directly
/// comparable.
pub const GROUP_A_ROW_COUNT: usize = 100_000;

/// Hot-loop probe count. Large enough that criterion's iterator
/// pre-walk doesn't repeat the same key for thousands of iterations
/// (which would lean on the OS page cache and skew the gap).
pub const GROUP_A_PROBE_COUNT: usize = 10_000;

/// Number of distinct `(lo, hi)` pairs criterion's hot loop rotates
/// through for each W2 range size. Smaller than `GROUP_A_PROBE_COUNT`
/// because each range scan touches up to 10k rows; we don't need
/// thousands of distinct windows to avoid cache effects.
pub const GROUP_A_RANGE_PROBE_COUNT: usize = 64;

const GROUP_A_SEED: u64 = 43;

/// Build the Group-A dataset deterministically. Reuses the same
/// `payload_for` helper as W1, so identical `id`s produce identical
/// payloads across workloads (cuts down on cross-workload variance
/// when the row reassembly path is the bottleneck).
pub fn group_a_dataset() -> GroupADataset {
    let mut rng = ChaCha8Rng::seed_from_u64(GROUP_A_SEED);

    // Build the secondary-value permutation up front: shuffle
    // `1..=N` so each id maps to a unique non-PK-ordered value.
    let mut secondaries: Vec<i64> = (1..=GROUP_A_ROW_COUNT as i64).collect();
    secondaries.shuffle(&mut rng);

    let mut rows = Vec::with_capacity(GROUP_A_ROW_COUNT);
    for (i, &secondary) in secondaries.iter().enumerate() {
        let id = (i + 1) as i64;
        rows.push(GroupARow {
            id,
            secondary,
            payload: payload_for(id),
        });
    }

    let mut pk_probes: Vec<i64> = (1..=GROUP_A_ROW_COUNT as i64).collect();
    pk_probes.shuffle(&mut rng);
    pk_probes.truncate(GROUP_A_PROBE_COUNT);

    let mut secondary_probes = secondaries.clone();
    secondary_probes.shuffle(&mut rng);
    secondary_probes.truncate(GROUP_A_PROBE_COUNT);

    let range_probes_100 = build_range_probes(&mut rng, 100);
    let range_probes_1k = build_range_probes(&mut rng, 1_000);
    let range_probes_10k = build_range_probes(&mut rng, 10_000);

    GroupADataset {
        rows,
        pk_probes,
        secondary_probes,
        range_probes_100,
        range_probes_1k,
        range_probes_10k,
    }
}

/// Pick `GROUP_A_RANGE_PROBE_COUNT` non-overlapping random `(lo, hi)`
/// pairs of `width` rows each. `lo` is uniform in
/// `1..=GROUP_A_ROW_COUNT - width + 1` so every range stays in-bounds.
fn build_range_probes(rng: &mut ChaCha8Rng, width: i64) -> Vec<(i64, i64)> {
    let max_lo = GROUP_A_ROW_COUNT as i64 - width + 1;
    let mut out = Vec::with_capacity(GROUP_A_RANGE_PROBE_COUNT);
    for _ in 0..GROUP_A_RANGE_PROBE_COUNT {
        let lo = rng.gen_range(1..=max_lo);
        out.push((lo, lo + width - 1));
    }
    out
}

/// Dataset for the Group-B "SQL-feature scaling" workloads — W7
/// (SUM aggregate) and W8 (GROUP BY at three cardinalities).
///
/// Schema:
///
/// ```sql
/// CREATE TABLE big (
///   id     INTEGER PRIMARY KEY,
///   v      INTEGER,
///   k_10   INTEGER,
///   k_1k   INTEGER,
///   k_100k INTEGER
/// );
/// ```
///
/// - `v = (id * 7) mod 1000` — varied non-monotone integer for SUM.
/// - `k_10 = id mod 10` — 10 distinct groups (W8 low-cardinality).
/// - `k_1k = id mod 1000` — 1k distinct groups.
/// - `k_100k = id mod 100_000` — 100k distinct groups (essentially
///   one group per ~10 rows on a 1M-row table; the high-cardinality
///   stress-test for the hash aggregator).
///
/// 1M rows is the plan's W7 target — the largest single-table dataset
/// in the suite. 1M × ~40 bytes/row ≈ 40 MB on disk; well within
/// SQLRite's whole-DB-in-RAM model on a 32 GiB host.
pub struct GroupBDataset {
    pub rows: Vec<GroupBRow>,
    /// Pre-computed `SUM(v)` so the W7 correctness gate doesn't have
    /// to re-derive it on every probe.
    pub sum_v: i64,
}

pub struct GroupBRow {
    pub id: i64,
    pub v: i64,
    pub k_10: i64,
    pub k_1k: i64,
    pub k_100k: i64,
}

/// 1M rows. Plan target for W7. W8 reuses the same dataset.
pub const GROUP_B_ROW_COUNT: usize = 1_000_000;

const GROUP_B_SEED: u64 = 44;

pub fn group_b_dataset() -> GroupBDataset {
    // Seed kept for forward-compat — the dataset is currently fully
    // deterministic from `id`, but a future variant may shuffle `v`
    // independently and we want the seed surface ready.
    let _rng = ChaCha8Rng::seed_from_u64(GROUP_B_SEED);

    let mut rows = Vec::with_capacity(GROUP_B_ROW_COUNT);
    let mut sum_v: i64 = 0;
    for i in 1..=GROUP_B_ROW_COUNT as i64 {
        let v = (i.wrapping_mul(7)).rem_euclid(1_000);
        sum_v += v;
        rows.push(GroupBRow {
            id: i,
            v,
            k_10: i.rem_euclid(10),
            k_1k: i.rem_euclid(1_000),
            k_100k: i.rem_euclid(100_000),
        });
    }
    GroupBDataset { rows, sum_v }
}

/// W9 INNER JOIN dataset. Two 100k-row tables with a 1:1 PK/FK
/// relationship — every customer has exactly one order. The hot loop
/// probes by customer PK and joins to the matching order.
///
/// Schema:
///
/// ```sql
/// CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT);
/// CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER, amount INTEGER);
/// CREATE INDEX idx_orders_customer ON orders(customer_id);
/// ```
///
/// SQLRite's join is a nested-loop driver without an inner-side index
/// probe (see [`docs/supported-sql.md`](../../docs/supported-sql.md)
/// "JOIN semantics" / `executor.rs::execute_select_rows_joined`). So
/// per-PK-probe joins should show a meaningful gap vs SQLite's
/// indexed-nested-loop join. That gap is informational — the plan
/// flags it as the most useful "what does the join planner cost us?"
/// number.
pub struct JoinDataset {
    pub customers: Vec<JoinCustomer>,
    pub orders: Vec<JoinOrder>,
    /// Random customer-PK probes for the bench hot loop.
    pub probes: Vec<i64>,
}

pub struct JoinCustomer {
    pub id: i64,
    pub name: String,
}

pub struct JoinOrder {
    pub id: i64,
    pub customer_id: i64,
    pub amount: i64,
}

/// Plan-deviation note (W9): the plan spec calls for "two 100k-row
/// tables." A first smoke at that scale measured SQLRite at >5
/// minutes per iteration on the criterion hot loop — the engine's
/// join executor is a left-folded nested-loop driver that doesn't
/// push the ON predicate down to an index probe on the inner side
/// (`src/sql/executor.rs::execute_select_rows_joined`), so each
/// per-PK probe scans the full 100k-row inner table. That scale is
/// untenable for `make bench`.
///
/// v1 ships at **10k rows** instead. SQLRite still scans the whole
/// inner side per probe — the per-probe cost is ~10× lower at this
/// scale, the gap vs SQLite's indexed nested-loop join stays
/// meaningful, and the bench finishes in seconds. Bumping back to
/// 100k follows a SQLRite join-planner / indexed-inner-side
/// improvement (tracked separately) and a `W9.v2` tag.
pub const JOIN_ROW_COUNT: usize = 10_000;
pub const JOIN_PROBE_COUNT: usize = 1_000;
const JOIN_SEED: u64 = 45;

pub fn join_dataset() -> JoinDataset {
    let mut rng = ChaCha8Rng::seed_from_u64(JOIN_SEED);
    let mut customers = Vec::with_capacity(JOIN_ROW_COUNT);
    let mut orders = Vec::with_capacity(JOIN_ROW_COUNT);
    for i in 1..=JOIN_ROW_COUNT as i64 {
        customers.push(JoinCustomer {
            id: i,
            name: format!("customer_{i}"),
        });
        orders.push(JoinOrder {
            id: i,
            customer_id: i,
            amount: i.rem_euclid(10_000),
        });
    }
    let mut probes: Vec<i64> = (1..=JOIN_ROW_COUNT as i64).collect();
    probes.shuffle(&mut rng);
    probes.truncate(JOIN_PROBE_COUNT);
    JoinDataset {
        customers,
        orders,
        probes,
    }
}

/// W4 single-row-insert dataset. The bench loop generates rows on the
/// fly with monotonically-increasing PKs starting at this base, so the
/// preload (rows `1..=BASE-1`) sets a stable table size before timing
/// begins. Without that, the first iter measures an empty-table insert
/// while later iters measure a `iters_so_far`-sized table — a steep
/// O(N) ramp on SQLRite's bottom-up commit path that would dominate
/// the median.
pub const W4_PRELOAD_ROWS: i64 = 1_000;

/// Stable payload for W4 inserts. Same length as W1 / Group-A so the
/// row-write path exercises the same cell-encoding cost.
pub const W4_PAYLOAD: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Reuses [`payload_for`] for any workload that wants the same
/// per-id-stable 64-char string the Group-A / W1 datasets carry.
pub fn payload_str(id: i64) -> String {
    payload_for(id)
}

fn payload_for(id: i64) -> String {
    // 64 chars, content stable per id. Hex-encoded mix-of-bits so two
    // consecutive ids don't share a prefix (matters for any future
    // index-prefix-compressed engine — not SQLite or SQLRite today,
    // but a cheap correctness gesture).
    let mut s = String::with_capacity(64);
    let mut x = (id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for _ in 0..8 {
        s.push_str(&format!("{x:016x}"));
        x = x.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
        if s.len() >= 64 {
            s.truncate(64);
            break;
        }
    }
    s
}
