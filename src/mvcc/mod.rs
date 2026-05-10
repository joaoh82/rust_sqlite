//! Multi-version concurrency control primitives (Phase 11).
//!
//! This module is the foundation for SQLRite's `BEGIN CONCURRENT`
//! story — see [`docs/concurrent-writes-plan.md`](../../docs/concurrent-writes-plan.md)
//! for the full sequenced design.
//!
//! Surface as of Phase 11.3:
//!
//! - [`MvccClock`] — process-wide monotonic `u64` counter that hands
//!   out begin- and commit-timestamps. Persisted to the WAL header
//!   so timestamps don't reuse the same value across reopens.
//! - [`ActiveTxRegistry`] — tracks the begin-timestamps of in-flight
//!   transactions; [`ActiveTxRegistry::min_active_begin_ts`] is the
//!   GC watermark.
//! - [`TxId`] / [`TxTimestampOrId`] — types the version chains
//!   carry.
//! - [`MvStore`] — the in-memory version index. Holds row chains
//!   keyed by [`RowID`]; `read(row, begin_ts)` implements the
//!   snapshot-isolation visibility rule (`begin <= T < end`).
//! - [`JournalMode`] — per-database setting toggled by
//!   `PRAGMA journal_mode = …`. `Wal` (default) keeps every
//!   pre-Phase-11 read path in place; `Mvcc` is the opt-in that
//!   11.4 will wire reads through.
//!
//! The executor doesn't consult `MvStore` yet — that wiring lives
//! in 11.4 alongside `BEGIN CONCURRENT` writes. Decoupling the
//! data structure (this PR) from the read/write integration (next
//! PR) keeps the diffs reviewable.

pub mod clock;
pub mod registry;
pub mod store;

pub use clock::MvccClock;
pub use registry::{ActiveTxRegistry, TxHandle, TxId, TxTimestampOrId};
pub use store::{MvStore, MvStoreError, RowID, RowVersion, RowVersionChain, VersionPayload};

/// Selects the durability + concurrency story a database operates
/// under. Toggled by `PRAGMA journal_mode = …` (see
/// [`crate::sql::pragma::execute_pragma`]).
///
/// - [`JournalMode::Wal`] (default) — every read goes through the
///   legacy table → pager path; every write fsyncs a per-page
///   commit frame. This is the only mode pre-Phase-11 builds knew
///   about, and it's what file-format-v5 + WAL-format-v2 files
///   produce by default.
/// - [`JournalMode::Mvcc`] — opts the database into Phase 11's
///   multi-version concurrency control. Enables snapshot-isolated
///   reads (consult `MvStore` first, fall back to the pager) and
///   `BEGIN CONCURRENT` writes (Phase 11.4). On-disk format is
///   unchanged; the WAL header's `clock_high_water` byte range
///   carries the persisted clock value either way.
///
/// Phase 11.3 ships the parser surface and the per-database
/// setting; the read path doesn't change behaviour yet. The
/// `Mvcc` value is observable via the PRAGMA read form so callers
/// can confirm the toggle landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JournalMode {
    /// Default — legacy WAL-backed pager. Every commit fsyncs a
    /// page-level frame; every read consults `staged → wal_cache
    /// → on_disk`.
    #[default]
    Wal,
    /// Phase 11 MVCC + `BEGIN CONCURRENT`. Same on-disk format as
    /// `Wal`; the in-memory `MvStore` sits in front of the pager
    /// for reads, and writes go through commit-time validation.
    Mvcc,
}

impl JournalMode {
    /// Parses a PRAGMA value (case-insensitive). Returns `None` for
    /// unrecognized inputs so the caller can surface a typed
    /// `unknown journal_mode` error with the bad string.
    pub fn from_str_lossless(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "wal" => Some(Self::Wal),
            "mvcc" => Some(Self::Mvcc),
            _ => None,
        }
    }

    /// The lowercase string form the PRAGMA read renders.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Wal => "wal",
            Self::Mvcc => "mvcc",
        }
    }
}

impl std::fmt::Display for JournalMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_mode_default_is_wal() {
        assert_eq!(JournalMode::default(), JournalMode::Wal);
    }

    #[test]
    fn journal_mode_round_trips_through_str() {
        assert_eq!(
            JournalMode::from_str_lossless("wal"),
            Some(JournalMode::Wal)
        );
        assert_eq!(
            JournalMode::from_str_lossless("WAL"),
            Some(JournalMode::Wal)
        );
        assert_eq!(
            JournalMode::from_str_lossless("Mvcc"),
            Some(JournalMode::Mvcc)
        );
        assert_eq!(JournalMode::from_str_lossless("delete"), None);
        assert_eq!(JournalMode::from_str_lossless(""), None);
    }

    #[test]
    fn journal_mode_displays_lowercase() {
        assert_eq!(format!("{}", JournalMode::Wal), "wal");
        assert_eq!(format!("{}", JournalMode::Mvcc), "mvcc");
    }
}
