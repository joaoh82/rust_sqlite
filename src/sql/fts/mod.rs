//! Full-text search (FTS) — inverted-index keyword retrieval with BM25
//! ranking. Pure algorithms; no SQL integration in this module.
//!
//! Phase 8 of the SQLRite roadmap; see `docs/phase-8-plan.md`.
//!
//! - [`tokenizer`] — split text into terms (ASCII MVP per Q3).
//! - [`bm25`] — BM25 relevance scoring (`k1 = 1.5`, `b = 0.75`, fixed per
//!   Q4 + Q5; no stemming, no stop list).
//! - [`posting_list`] — in-memory inverted index keyed by term, holding
//!   per-document term frequencies + lengths. Insert / remove / query.
//!
//! Phase 8a shipped these standalone algorithms; Phase 8b wires them
//! into the SQL surface (`CREATE INDEX … USING fts(<col>)`,
//! `fts_match`, `bm25_score`, the `try_fts_probe` optimizer hook).
//! Persistence of the posting lists themselves arrives with Phase 8c
//! (`KIND_FTS_POSTING` cell encoding).

pub mod bm25;
pub mod posting_list;
pub mod tokenizer;

pub use bm25::{Bm25Params, score as bm25_score};
pub use posting_list::PostingList;
pub use tokenizer::tokenize;
