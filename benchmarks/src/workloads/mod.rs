//! Workloads.
//!
//! Each workload is one file. Pattern:
//!
//! 1. Public `WORKLOAD_ID: WorkloadId` constant. Carries the
//!    versioned id (Q8) so the JSON envelope and the criterion bench
//!    group both pick it up consistently.
//! 2. A `setup<D: Driver>(...)` that builds the dataset against the
//!    driver and returns the connection — runs once per criterion
//!    bench, **outside** the timed loop.
//! 3. A `bench_iter<D: Driver>(...)` for the per-iteration body —
//!    what criterion's `b.iter` measures.
//! 4. An `expected_hash(...)` for the correctness gate (Q3 risk
//!    mitigation: catch divergent semantics across engines before the
//!    "winner" measurement is meaningful).

pub mod kv;
