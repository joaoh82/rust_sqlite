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
