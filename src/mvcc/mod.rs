//! Multi-version concurrency control primitives (Phase 11).
//!
//! This module is the foundation for SQLRite's `BEGIN CONCURRENT` story
//! — see [`docs/concurrent-writes-plan.md`](../../docs/concurrent-writes-plan.md)
//! for the full sequenced design. As of **Phase 11.2** it carries the
//! standalone primitives that the rest of the work hangs off:
//!
//! - [`MvccClock`] — a process-wide monotonic `u64` counter that hands
//!   out begin- and commit-timestamps. Persisted to the WAL header so
//!   timestamps don't reuse the same value across reopens.
//! - [`ActiveTxRegistry`] — tracks the begin-timestamps of in-flight
//!   transactions. Garbage collection (Phase 11.6) needs
//!   [`ActiveTxRegistry::min_active_begin_ts`] to know which versions
//!   are still possibly visible to a live reader.
//! - [`TxId`] — opaque newtype around a `u64`, allocated by the clock
//!   while a transaction is in flight. After commit the same value is
//!   reused as the row version's `begin` timestamp; the discriminator
//!   between "in-flight transaction id" and "committed timestamp"
//!   lives in [`TxTimestampOrId`].
//!
//! Nothing in the executor reads from these yet — Phase 11.3 wires
//! them into a new `MvStore` in front of the pager. Keeping the
//! plumbing standalone in 11.2 means the Phase 11.4 `BEGIN CONCURRENT`
//! work can pull them in without re-litigating the foundation.

pub mod clock;
pub mod registry;

pub use clock::MvccClock;
pub use registry::{ActiveTxRegistry, TxHandle, TxId, TxTimestampOrId};
