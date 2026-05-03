//! In-memory inverted index for FTS — `term -> { rowid -> term_freq }`,
//! plus per-document length cache. Wraps the [`super::tokenizer`] +
//! [`super::bm25`] primitives into a usable index. Pure data structure;
//! no SQL coupling.
//!
//! Mirrors the role of [`crate::sql::hnsw::HnswIndex`] in 7d.1: this is
//! the in-memory state that 8b will hang off `Table` (via a future
//! `fts_indexes: Vec<FtsIndex>` field) and that 8c will serialize into
//! `KIND_FTS_POSTING` cells.
//!
//! ## Identity choices
//!
//! - Rowids are `i64` (matches HNSW's `node_id` and SQLRite's row-id
//!   convention; see [`crate::sql::hnsw::HnswIndex::insert`]).
//! - The map structure is `BTreeMap<String, BTreeMap<i64, u32>>` rather
//!   than `HashMap` so that (1) persistence (8c) gets a deterministic
//!   on-disk byte order for free — postings are emitted in lexicographic
//!   term order, each posting list in ascending rowid order — and (2)
//!   tests get stable ordering without sorting. `HashMap` is faster on a
//!   per-op basis but the lookups in the FTS hot path are bounded by
//!   query-term count (single digits in practice), so the BTreeMap log-N
//!   factor is negligible.
//!
//! ## What it does NOT do (yet)
//!
//! - **No persistence.** State lives entirely in memory. 8c wires it
//!   into the page format under cell-kind `0x06`.
//! - **No transaction integration.** 8b is responsible for batching
//!   updates inside a `BEGIN; ... COMMIT;` block.
//! - **No phrase / boolean queries.** Single-token any-term match only
//!   for the MVP per the plan's "Out of scope" section. Multi-token
//!   queries OR the per-term hits — no AND, NOT, or positional info.

use std::collections::{BTreeMap, HashMap};

use super::bm25::{Bm25Params, score as bm25_score};
use super::tokenizer::tokenize;

/// In-memory inverted index. See module-level doc.
#[derive(Debug, Default, Clone)]
pub struct PostingList {
    /// Term -> { rowid -> term frequency in that doc }.
    postings: BTreeMap<String, BTreeMap<i64, u32>>,
    /// Rowid -> document length (in tokens, post-tokenization).
    /// Acts as the canonical "set of indexed rowids" — `len()` and
    /// `is_empty()` derive from this.
    doc_lengths: BTreeMap<i64, u32>,
    /// Sum of all `doc_lengths` values; tracked incrementally to make
    /// [`avg_doc_len`] O(1) regardless of corpus size.
    total_tokens: u64,
}

impl PostingList {
    /// Empty index with no postings and no documents.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of indexed documents.
    pub fn len(&self) -> usize {
        self.doc_lengths.len()
    }

    /// True iff no document has been inserted (or all have been removed).
    pub fn is_empty(&self) -> bool {
        self.doc_lengths.is_empty()
    }

    /// Average document length in tokens. Returns `0.0` when the index
    /// is empty so BM25 can guard cleanly without a div-by-zero.
    pub fn avg_doc_len(&self) -> f64 {
        if self.doc_lengths.is_empty() {
            0.0
        } else {
            self.total_tokens as f64 / self.doc_lengths.len() as f64
        }
    }

    /// Phase 8c — emit `(rowid, doc_len)` pairs for every indexed doc,
    /// in ascending rowid order. The pager writes these into the FTS
    /// index's doc-lengths sidecar cell; reload feeds them back to
    /// [`Self::from_persisted_postings`].
    pub fn serialize_doc_lengths(&self) -> Vec<(i64, u32)> {
        self.doc_lengths
            .iter()
            .map(|(id, len)| (*id, *len))
            .collect()
    }

    /// Phase 8c — emit `(term, [(rowid, term_freq)])` triples in
    /// lexicographic term order; per-term entries are in ascending
    /// rowid order (the underlying `BTreeMap` already guarantees this).
    /// One element per unique indexed term; pager writes one cell per
    /// element.
    pub fn serialize_postings(&self) -> Vec<(String, Vec<(i64, u32)>)> {
        self.postings
            .iter()
            .map(|(term, postings)| {
                let entries = postings.iter().map(|(id, freq)| (*id, *freq)).collect();
                (term.clone(), entries)
            })
            .collect()
    }

    /// Phase 8c — rebuild a `PostingList` directly from the persisted
    /// doc-lengths sidecar + per-term postings. No tokenization runs;
    /// the resulting index is byte-equivalent to what was saved
    /// (assuming the input came from `serialize_*`).
    ///
    /// `doc_lengths` is the full `(rowid, doc_len)` map written into
    /// the sidecar cell. `postings` is one `(term, [(rowid, tf)])`
    /// element per term cell.
    pub fn from_persisted_postings<I, J>(doc_lengths: I, postings: J) -> Self
    where
        I: IntoIterator<Item = (i64, u32)>,
        J: IntoIterator<Item = (String, Vec<(i64, u32)>)>,
    {
        let mut doc_lengths_map: BTreeMap<i64, u32> = BTreeMap::new();
        let mut total_tokens: u64 = 0;
        for (rowid, len) in doc_lengths {
            doc_lengths_map.insert(rowid, len);
            total_tokens += len as u64;
        }

        let mut postings_map: BTreeMap<String, BTreeMap<i64, u32>> = BTreeMap::new();
        for (term, entries) in postings {
            let inner: BTreeMap<i64, u32> = entries.into_iter().collect();
            // An empty posting list shouldn't be persisted, but if it
            // somehow was, drop it on load — `remove()` would have
            // pruned the same way at runtime.
            if !inner.is_empty() {
                postings_map.insert(term, inner);
            }
        }

        Self {
            postings: postings_map,
            doc_lengths: doc_lengths_map,
            total_tokens,
        }
    }

    /// Tokenize `text` and add its postings under `rowid`. If `rowid` is
    /// already indexed, its previous postings are removed first — i.e.
    /// `insert` is idempotent for re-indexing the same row.
    ///
    /// A row whose tokenization yields zero tokens is still recorded
    /// (with `doc_len = 0` and no posting entries). This keeps `len()`
    /// honest for "indexed but empty" rows; BM25 returns 0.0 for them.
    pub fn insert(&mut self, rowid: i64, text: &str) {
        if self.doc_lengths.contains_key(&rowid) {
            self.remove(rowid);
        }

        let tokens = tokenize(text);
        let doc_len = tokens.len() as u32;
        self.total_tokens += doc_len as u64;
        self.doc_lengths.insert(rowid, doc_len);

        // Aggregate per-term frequency for this doc, then push into the
        // global postings map. This avoids bumping the same posting
        // entry repeatedly for a doc with many occurrences of one term.
        let mut tf: HashMap<&str, u32> = HashMap::new();
        for tok in &tokens {
            *tf.entry(tok.as_str()).or_insert(0) += 1;
        }
        for (term, freq) in tf {
            self.postings
                .entry(term.to_string())
                .or_default()
                .insert(rowid, freq);
        }
    }

    /// Remove all postings for `rowid`. No-op if `rowid` was never
    /// inserted. Empty per-term posting lists left behind by the last
    /// referencing row are pruned to keep the BTreeMap tight.
    pub fn remove(&mut self, rowid: i64) {
        let Some(doc_len) = self.doc_lengths.remove(&rowid) else {
            return;
        };
        self.total_tokens -= doc_len as u64;

        // Walk every term — fine because term count grows with vocab,
        // not corpus size, and remove is rare. 8b's incremental DELETE
        // path uses the rebuild-at-save strategy (Q7) anyway.
        let mut empty_terms = Vec::new();
        for (term, postings) in self.postings.iter_mut() {
            if postings.remove(&rowid).is_some() && postings.is_empty() {
                empty_terms.push(term.clone());
            }
        }
        for term in empty_terms {
            self.postings.remove(&term);
        }
    }

    /// True iff `rowid` is indexed and at least one of its terms is in
    /// the (tokenized) `query`. Powers `fts_match(col, 'q')` in 8b
    /// without going through scoring.
    pub fn matches(&self, rowid: i64, query: &str) -> bool {
        if !self.doc_lengths.contains_key(&rowid) {
            return false;
        }
        for term in tokenize(query) {
            if let Some(postings) = self.postings.get(&term) {
                if postings.contains_key(&rowid) {
                    return true;
                }
            }
        }
        false
    }

    /// BM25 score for a single (rowid, query) pair. Returns `0.0` if
    /// `rowid` is unknown or no query terms hit.
    pub fn score(&self, rowid: i64, query: &str, params: &Bm25Params) -> f64 {
        let Some(&doc_len) = self.doc_lengths.get(&rowid) else {
            return 0.0;
        };
        let query_terms = tokenize(query);
        if query_terms.is_empty() {
            return 0.0;
        }

        let term_freq = self.term_freq_for_doc(rowid, &query_terms);
        let n_docs_with = self.n_docs_with_for_terms(&query_terms);
        bm25_score(
            &query_terms,
            &term_freq,
            doc_len,
            self.avg_doc_len(),
            &n_docs_with,
            self.doc_lengths.len() as u32,
            params,
        )
    }

    /// Score every doc that contains at least one query term and return
    /// `(rowid, score)` sorted by score descending, ties broken by
    /// rowid ascending. Powers the bulk path used by 8b's
    /// `try_fts_probe` optimizer hook.
    ///
    /// Empty query → empty result. Empty index → empty result. Rows
    /// that don't match any query term are not scored at all (they
    /// would score 0.0 — including them just bloats the result).
    pub fn query(&self, query: &str, params: &Bm25Params) -> Vec<(i64, f64)> {
        let query_terms = tokenize(query);
        if query_terms.is_empty() || self.doc_lengths.is_empty() {
            return Vec::new();
        }

        // Collect candidate rowids: every doc that has at least one
        // query term in its postings. BTreeMap iteration is sorted, so
        // the candidate set comes out in ascending rowid order — handy
        // for the tie-break below.
        let mut candidates: BTreeMap<i64, u32> = BTreeMap::new();
        for term in &query_terms {
            if let Some(postings) = self.postings.get(term) {
                for &rowid in postings.keys() {
                    candidates.entry(rowid).or_insert(0);
                }
            }
        }
        if candidates.is_empty() {
            return Vec::new();
        }

        let n_docs_with = self.n_docs_with_for_terms(&query_terms);
        let avg = self.avg_doc_len();
        let total_docs = self.doc_lengths.len() as u32;

        let mut scored: Vec<(i64, f64)> = candidates
            .into_keys()
            .map(|rowid| {
                let doc_len = self.doc_lengths[&rowid];
                let tf = self.term_freq_for_doc(rowid, &query_terms);
                let s = bm25_score(
                    &query_terms,
                    &tf,
                    doc_len,
                    avg,
                    &n_docs_with,
                    total_docs,
                    params,
                );
                (rowid, s)
            })
            .collect();

        // Score desc, then rowid asc on ties. f64::partial_cmp + the
        // candidate set already being sorted ascending means we only
        // need a stable sort_by on score.
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored
    }

    fn term_freq_for_doc(&self, rowid: i64, query_terms: &[String]) -> HashMap<String, u32> {
        let mut tf = HashMap::with_capacity(query_terms.len());
        for term in query_terms {
            if tf.contains_key(term) {
                continue;
            }
            let freq = self
                .postings
                .get(term)
                .and_then(|p| p.get(&rowid).copied())
                .unwrap_or(0);
            tf.insert(term.clone(), freq);
        }
        tf
    }

    fn n_docs_with_for_terms(&self, query_terms: &[String]) -> HashMap<String, u32> {
        let mut n = HashMap::with_capacity(query_terms.len());
        for term in query_terms {
            if n.contains_key(term) {
                continue;
            }
            let count = self.postings.get(term).map(|p| p.len() as u32).unwrap_or(0);
            n.insert(term.clone(), count);
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list_is_empty() {
        let pl = PostingList::new();
        assert!(pl.is_empty());
        assert_eq!(pl.len(), 0);
        assert_eq!(pl.avg_doc_len(), 0.0);
        assert!(pl.query("anything", &Bm25Params::default()).is_empty());
        assert_eq!(pl.score(1, "anything", &Bm25Params::default()), 0.0);
        assert!(!pl.matches(1, "anything"));
    }

    #[test]
    fn empty_query_returns_empty_results() {
        let mut pl = PostingList::new();
        pl.insert(1, "rust embedded database");
        assert!(pl.query("", &Bm25Params::default()).is_empty());
        assert!(pl.query("!!!", &Bm25Params::default()).is_empty());
        assert_eq!(pl.score(1, "", &Bm25Params::default()), 0.0);
    }

    #[test]
    fn insert_and_query_two_docs_ranks_correctly() {
        let mut pl = PostingList::new();
        pl.insert(1, "rust rust embedded database");
        pl.insert(2, "rust language");
        let res = pl.query("rust", &Bm25Params::default());
        assert_eq!(res.len(), 2);
        // doc1 has tf=2 in a longer doc; doc2 has tf=1 in a shorter doc.
        // Length normalization makes the call non-obvious — just check
        // that the result set contains both rows in some order, with
        // both scores positive.
        let (id_a, s_a) = res[0];
        let (id_b, s_b) = res[1];
        assert!(s_a > 0.0 && s_b > 0.0);
        assert!(s_a >= s_b);
        assert!(
            (id_a == 1 || id_a == 2) && (id_b == 1 || id_b == 2) && id_a != id_b,
            "result rowids should be {{1,2}}, got ({}, {})",
            id_a,
            id_b
        );

        // matches() agrees on which rows hit.
        assert!(pl.matches(1, "rust"));
        assert!(pl.matches(2, "rust"));
        assert!(!pl.matches(1, "python"));
    }

    #[test]
    fn score_method_matches_bulk_query() {
        let mut pl = PostingList::new();
        pl.insert(10, "rust embedded database");
        pl.insert(20, "go embedded database");
        pl.insert(30, "python web framework");

        let params = Bm25Params::default();
        let bulk = pl.query("embedded", &params);
        for (rowid, score) in &bulk {
            let direct = pl.score(*rowid, "embedded", &params);
            assert!(
                (direct - score).abs() < f64::EPSILON * 16.0,
                "score({}, ...) = {} vs query() reported {}",
                rowid,
                direct,
                score
            );
        }
        assert_eq!(pl.score(30, "embedded", &params), 0.0);
    }

    #[test]
    fn remove_clears_doc_and_prunes_empty_terms() {
        let mut pl = PostingList::new();
        pl.insert(1, "rust");
        pl.insert(2, "rust embedded");
        assert_eq!(pl.len(), 2);
        assert_eq!(pl.total_tokens, 3);
        assert!(pl.postings.contains_key("rust"));
        assert!(pl.postings.contains_key("embedded"));

        pl.remove(2);
        assert_eq!(pl.len(), 1);
        assert_eq!(pl.total_tokens, 1);
        // "embedded" only existed in doc 2; should be gone now.
        assert!(!pl.postings.contains_key("embedded"));
        assert!(pl.postings.contains_key("rust"));

        pl.remove(1);
        assert!(pl.is_empty());
        assert!(pl.postings.is_empty());
        assert_eq!(pl.total_tokens, 0);

        // Idempotent remove.
        pl.remove(1);
        pl.remove(99);
        assert!(pl.is_empty());
    }

    #[test]
    fn reinsert_replaces_prior_postings() {
        let mut pl = PostingList::new();
        pl.insert(1, "rust rust rust");
        assert_eq!(pl.postings["rust"][&1], 3);
        assert_eq!(pl.total_tokens, 3);

        pl.insert(1, "go");
        assert_eq!(pl.len(), 1);
        assert_eq!(pl.total_tokens, 1);
        assert!(!pl.postings.contains_key("rust"));
        assert_eq!(pl.postings["go"][&1], 1);
    }

    #[test]
    fn tie_break_orders_by_rowid_ascending() {
        // Two identical docs → identical scores → rowid ASC.
        let mut pl = PostingList::new();
        pl.insert(7, "alpha beta");
        pl.insert(3, "alpha beta");
        pl.insert(5, "alpha beta");
        let res = pl.query("alpha", &Bm25Params::default());
        let ids: Vec<i64> = res.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![3, 5, 7]);
        // All three scores should be exactly equal.
        let s = res[0].1;
        for (_, score) in &res {
            assert_eq!(*score, s);
        }
    }

    #[test]
    fn multi_term_query_unions_candidates_any_term() {
        let mut pl = PostingList::new();
        pl.insert(1, "rust embedded");
        pl.insert(2, "rust web");
        pl.insert(3, "go embedded");
        pl.insert(4, "python web");
        let res = pl.query("rust embedded", &Bm25Params::default());
        let ids: std::collections::BTreeSet<i64> = res.iter().map(|(id, _)| *id).collect();
        // Per the MVP "any-term" semantic — rowid 4 is the only one with
        // neither term, so it must NOT appear; the other three must.
        assert_eq!(ids, [1, 2, 3].iter().copied().collect());
        // Doc 1 has both terms → should outrank singletons.
        assert_eq!(res[0].0, 1);
    }

    #[test]
    fn serialize_round_trips_through_from_persisted() {
        // Phase 8c — the (de)serialize pair must reproduce the exact
        // in-memory state that was saved. Emptiness, multi-term, and
        // re-insert idempotence all need to round-trip.
        let mut pl = PostingList::new();
        pl.insert(1, "rust embedded database");
        pl.insert(2, "rust web framework");
        pl.insert(3, ""); // zero-token doc — exercises the sidecar
        pl.insert(4, "rust rust rust embedded power");

        let docs = pl.serialize_doc_lengths();
        let postings = pl.serialize_postings();
        let roundtripped = PostingList::from_persisted_postings(docs, postings);

        assert_eq!(roundtripped.len(), pl.len(), "doc count");
        assert_eq!(roundtripped.avg_doc_len(), pl.avg_doc_len(), "avg_doc_len");
        // Every query result + score must match.
        let q = pl.query("rust", &Bm25Params::default());
        let q2 = roundtripped.query("rust", &Bm25Params::default());
        assert_eq!(q, q2, "query results must match after round-trip");
        // Zero-token doc 3 stays in the corpus stats so total_docs is
        // honest, even though it'll never match a query.
        assert!(roundtripped.matches(1, "rust"));
        assert!(!roundtripped.matches(3, "rust"));
    }

    #[test]
    fn synthetic_thousand_doc_corpus_top_ten_is_stable() {
        // 1000 deterministic docs. Most are noise; only 5 contain the
        // rare "quasar" term. Top-10 query must surface those 5 (the
        // remaining slots score 0.0 and aren't returned at all because
        // we filter to candidates with at least one matching term).
        let mut pl = PostingList::new();
        let rare_rows: [i64; 5] = [137, 248, 391, 642, 873];
        for i in 0..1000_i64 {
            // Pseudo-random body deterministic in `i`.
            let words = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"];
            let pick_a = words[((i as usize) * 7) % words.len()];
            let pick_b = words[((i as usize) * 13 + 1) % words.len()];
            let body = if rare_rows.contains(&i) {
                format!("quasar {} {}", pick_a, pick_b)
            } else {
                format!("{} {}", pick_a, pick_b)
            };
            pl.insert(i, &body);
        }
        assert_eq!(pl.len(), 1000);

        let res = pl.query("quasar", &Bm25Params::default());
        assert_eq!(res.len(), 5, "exactly five docs should contain 'quasar'");
        let returned: std::collections::BTreeSet<i64> = res.iter().map(|(id, _)| *id).collect();
        let expected: std::collections::BTreeSet<i64> = rare_rows.iter().copied().collect();
        assert_eq!(returned, expected);

        // Stability: re-running the query yields identical output (no
        // hidden HashMap order leaking through).
        let res2 = pl.query("quasar", &Bm25Params::default());
        assert_eq!(res, res2);
    }
}
