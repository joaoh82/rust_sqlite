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
