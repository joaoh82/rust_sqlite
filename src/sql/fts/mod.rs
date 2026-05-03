//! Full-text search (FTS) — inverted-index keyword retrieval with BM25
//! ranking. Pure algorithms; no SQL integration in this module.
//!
//! Phase 8 of the SQLRite roadmap; see [`docs/phase-8-plan.md`]. This is
//! sub-phase 8a, the standalone trio that the SQL surface (8b) and
//! persistence layer (8c) build on top of:
//!
//! - [`tokenizer`] — split text into terms (ASCII MVP per Q3).
//! - [`bm25`] — BM25 relevance scoring (`k1 = 1.5`, `b = 0.75`, fixed per
//!   Q4 + Q5; no stemming, no stop list).
//! - [`posting_list`] — in-memory inverted index keyed by term, holding
//!   per-document term frequencies + lengths. Insert / remove / query.
//!
//! Mirrors the shape of [`crate::sql::hnsw`] (Phase 7d.1's standalone
//! algorithm module): infallible public API, no project-error coupling,
//! zero crate deps beyond `std`. The integration concerns — `IndexMethod`
//! arms, `fts_match` / `bm25_score` scalar fns, the `try_fts_probe`
//! optimizer hook, `KIND_FTS_POSTING` cell encoding — all land in 8b/8c.

pub mod bm25;
pub mod posting_list;
pub mod tokenizer;

pub use bm25::{Bm25Params, score as bm25_score};
pub use posting_list::PostingList;
pub use tokenizer::tokenize;
