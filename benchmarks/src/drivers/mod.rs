//! Driver implementations.
//!
//! One file per engine; each implements [`crate::Driver`]. See
//! `docs/benchmarks-plan.md` "Driver bias" risk: every implementation
//! is reviewed against the question "is this how a perf-conscious user
//! of <engine> would write it?" — `prepare_cached`, transaction
//! batching, etc.
//!
//! Adding a comparator is one file here + one register-call in
//! `benches/suite.rs`.

pub mod sqlite;
pub mod sqlrite;

// DuckDB driver — Group B only. Feature-gated to keep the heavy
// `libduckdb` dep out of the default `make bench` build.
#[cfg(feature = "duckdb")]
pub mod duckdb;
