//! BM25 relevance scoring — the standard ranking function for keyword
//! retrieval. Pure math; no SQL coupling.
//!
//! Resolves Phase 8 plan Q4 + Q5: no stemming and no stop-list. The
//! caller is responsible for tokenizing both the query and the document
//! (see [`super::tokenizer::tokenize`]); this module just consumes term
//! frequencies + corpus stats and produces a score.
//!
//! ## Formula (Robertson/Spärck Jones BM25)
//!
//! For a document `d` and query `q`:
//!
//! ```text
//! score(d, q) = Σ_{t ∈ q} idf(t) · (tf(t,d) · (k1 + 1)) /
//!                                  (tf(t,d) + k1 · (1 - b + b · |d| / avgdl))
//!
//! idf(t) = ln(1 + (N - n(t) + 0.5) / (n(t) + 0.5))
//! ```
//!
//! - `N`        = total documents in corpus
//! - `n(t)`     = number of documents containing term `t`
//! - `tf(t,d)`  = frequency of `t` in `d`
//! - `|d|`      = length of `d` in tokens
//! - `avgdl`    = average document length across the corpus
//! - `k1`, `b`  = tuning constants (Q4 — fixed at SQLite FTS5 defaults)
//!
//! The `+ 1` inside the IDF log keeps the term non-negative even when
//! `n(t) > N/2`, which would otherwise give the classic BM25 negative
//! IDF and require clipping. This is the "BM25+" / Lucene variant.

use std::collections::HashMap;

/// Tuning parameters for BM25. Per Phase 8 Q4 the public surface still
/// exposes these as a struct so we can grow per-call overrides later
/// without breaking signatures, but the [`Bm25Params::default()`] values
/// (`k1 = 1.5`, `b = 0.75`) are fixed for the MVP and match SQLite FTS5.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bm25Params {
    /// Term-frequency saturation. Higher → less aggressive saturation
    /// (each additional occurrence keeps adding to the score). Typical
    /// range is `[1.2, 2.0]`; SQLite FTS5 ships `1.5`.
    pub k1: f64,
    /// Length-normalization weight. `0.0` → no length normalization,
    /// `1.0` → fully proportional. SQLite FTS5 ships `0.75`.
    pub b: f64,
}

impl Default for Bm25Params {
    fn default() -> Self {
        Self { k1: 1.5, b: 0.75 }
    }
}

/// Compute the BM25 score for a single (document, query) pair.
///
/// - `query_terms` is the pre-tokenized query. Duplicate tokens are
///   summed naturally — if the user typed `"rust rust db"`, the `rust`
///   contribution gets counted twice, matching the standard formulation.
/// - `term_freq` maps each *unique* term in the document to its
///   frequency within that document. The caller can build this from
///   [`super::tokenizer::tokenize`] output.
/// - `n_docs_with` is the corpus statistic — for each term, how many
///   distinct documents contain it. Only entries for query terms are
///   read; extra entries are ignored.
/// - Returns `0.0` for the empty query, the empty corpus
///   (`total_docs == 0`), or a document whose terms don't intersect the
///   query.
pub fn score(
    query_terms: &[String],
    term_freq: &HashMap<String, u32>,
    doc_len: u32,
    avg_doc_len: f64,
    n_docs_with: &HashMap<String, u32>,
    total_docs: u32,
    params: &Bm25Params,
) -> f64 {
    if query_terms.is_empty() || total_docs == 0 {
        return 0.0;
    }

    let n = total_docs as f64;
    let dl = doc_len as f64;
    // avgdl == 0 only if every doc is empty; guard the division.
    let length_norm = if avg_doc_len > 0.0 {
        params.b * (dl / avg_doc_len)
    } else {
        0.0
    };
    let denom_base = params.k1 * (1.0 - params.b + length_norm);

    let mut total = 0.0;
    for term in query_terms {
        let tf = term_freq.get(term).copied().unwrap_or(0) as f64;
        if tf == 0.0 {
            continue;
        }
        let n_t = n_docs_with.get(term).copied().unwrap_or(0) as f64;
        // BM25+ IDF: ln(1 + (N - n_t + 0.5) / (n_t + 0.5))
        let idf = (1.0 + (n - n_t + 0.5) / (n_t + 0.5)).ln();
        let numerator = tf * (params.k1 + 1.0);
        let denominator = tf + denom_base;
        total += idf * (numerator / denominator);
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Bm25Params {
        Bm25Params::default()
    }

    fn tf(pairs: &[(&str, u32)]) -> HashMap<String, u32> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
    }

    #[test]
    fn empty_query_or_corpus_returns_zero() {
        assert_eq!(score(&[], &tf(&[]), 0, 0.0, &tf(&[]), 0, &p()), 0.0);
        let q = vec!["rust".to_string()];
        assert_eq!(
            score(
                &q,
                &tf(&[("rust", 3)]),
                10,
                10.0,
                &tf(&[("rust", 1)]),
                0,
                &p()
            ),
            0.0
        );
    }

    #[test]
    fn zero_term_freq_yields_zero_score() {
        let q = vec!["rust".to_string()];
        let s = score(
            &q,
            &tf(&[("python", 5)]),
            10,
            10.0,
            &tf(&[("rust", 1), ("python", 1)]),
            5,
            &p(),
        );
        assert_eq!(s, 0.0);
    }

    #[test]
    fn higher_tf_strictly_higher_score_at_fixed_length() {
        let q = vec!["rust".to_string()];
        let n_docs_with = tf(&[("rust", 2)]);
        let s_low = score(&q, &tf(&[("rust", 1)]), 10, 10.0, &n_docs_with, 100, &p());
        let s_hi = score(&q, &tf(&[("rust", 5)]), 10, 10.0, &n_docs_with, 100, &p());
        assert!(s_hi > s_low, "tf=5 ({}) should beat tf=1 ({})", s_hi, s_low);
    }

    #[test]
    fn longer_doc_scores_lower_at_same_tf() {
        // Same term-frequency, longer document → length normalization
        // (b > 0) drags the score down.
        let q = vec!["rust".to_string()];
        let n_docs_with = tf(&[("rust", 2)]);
        let s_short = score(&q, &tf(&[("rust", 3)]), 10, 50.0, &n_docs_with, 100, &p());
        let s_long = score(&q, &tf(&[("rust", 3)]), 200, 50.0, &n_docs_with, 100, &p());
        assert!(
            s_short > s_long,
            "short ({}) should beat long ({}) at same tf",
            s_short,
            s_long
        );
    }

    #[test]
    fn rare_term_dominates_common_term() {
        // "the" appears in every doc (n_t == N) → IDF ≈ 0.4 (positive but
        // small, BM25+ doesn't go negative). "quasar" appears in 1 doc →
        // IDF much larger. Same TF + length, the rare term wins.
        let q_common = vec!["the".to_string()];
        let q_rare = vec!["quasar".to_string()];
        let n_docs_with = tf(&[("the", 1000), ("quasar", 1)]);
        let s_common = score(
            &q_common,
            &tf(&[("the", 2)]),
            20,
            20.0,
            &n_docs_with,
            1000,
            &p(),
        );
        let s_rare = score(
            &q_rare,
            &tf(&[("quasar", 2)]),
            20,
            20.0,
            &n_docs_with,
            1000,
            &p(),
        );
        assert!(
            s_rare > s_common * 5.0,
            "rare term ({}) should dominate common term ({})",
            s_rare,
            s_common
        );
    }

    #[test]
    fn hand_computed_reference_three_doc_corpus() {
        // 3-doc corpus, query = ["rust"]:
        //   doc1: "rust rust db"      tf=2, len=3
        //   doc2: "rust db lang"      tf=1, len=3
        //   doc3: "python db tool"    tf=0, len=3
        // n("rust") = 2, N = 3, avgdl = 3.0, k1=1.5, b=0.75
        //
        //   length_norm  = 0.75 * (3 / 3) = 0.75
        //   denom_base   = 1.5 * (1 - 0.75 + 0.75) = 1.5
        //   idf("rust")  = ln(1 + (3 - 2 + 0.5) / (2 + 0.5))
        //                = ln(1 + 1.5/2.5) = ln(1.6) = 0.47000362924...
        //
        //   doc1: 0.47000362924... * (2 * 2.5) / (2 + 1.5)
        //       = 0.47000362924... * 5 / 3.5
        //       = 0.67143375606...
        //   doc2: 0.47000362924... * (1 * 2.5) / (1 + 1.5)
        //       = 0.47000362924... * 2.5 / 2.5
        //       = 0.47000362924...
        //   doc3: 0.0 (no rust)
        let q = vec!["rust".to_string()];
        let n_docs_with = tf(&[
            ("rust", 2),
            ("db", 3),
            ("lang", 1),
            ("python", 1),
            ("tool", 1),
        ]);
        let avgdl = 3.0;
        let s1 = score(
            &q,
            &tf(&[("rust", 2), ("db", 1)]),
            3,
            avgdl,
            &n_docs_with,
            3,
            &p(),
        );
        let s2 = score(
            &q,
            &tf(&[("rust", 1), ("db", 1), ("lang", 1)]),
            3,
            avgdl,
            &n_docs_with,
            3,
            &p(),
        );
        let s3 = score(
            &q,
            &tf(&[("python", 1), ("db", 1), ("tool", 1)]),
            3,
            avgdl,
            &n_docs_with,
            3,
            &p(),
        );

        let idf = (1.0_f64 + (3.0 - 2.0 + 0.5) / (2.0 + 0.5)).ln();
        let expected_s1 = idf * (2.0 * (1.5 + 1.0)) / (2.0 + 1.5);
        let expected_s2 = idf * (1.0 * (1.5 + 1.0)) / (1.0 + 1.5);
        let tol = f64::EPSILON * 16.0;
        assert!(
            (s1 - expected_s1).abs() < tol,
            "doc1 score {} vs expected {}",
            s1,
            expected_s1
        );
        assert!(
            (s2 - expected_s2).abs() < tol,
            "doc2 score {} vs expected {}",
            s2,
            expected_s2
        );
        assert_eq!(s3, 0.0);
        assert!(s1 > s2, "doc1 (tf=2) should outrank doc2 (tf=1)");
    }

    #[test]
    fn duplicate_query_tokens_compound() {
        let q_one = vec!["rust".to_string()];
        let q_two = vec!["rust".to_string(), "rust".to_string()];
        let n_docs_with = tf(&[("rust", 2)]);
        let s1 = score(&q_one, &tf(&[("rust", 1)]), 5, 5.0, &n_docs_with, 10, &p());
        let s2 = score(&q_two, &tf(&[("rust", 1)]), 5, 5.0, &n_docs_with, 10, &p());
        assert!(
            (s2 - 2.0 * s1).abs() < f64::EPSILON * 8.0,
            "duplicated query token should double the score: 2*s1={}, s2={}",
            2.0 * s1,
            s2
        );
    }
}
