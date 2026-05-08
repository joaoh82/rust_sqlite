//! W12 — hybrid retrieval (BM25 + cosine fusion). SQLRite-only.
//!
//! ```sql
//! CREATE TABLE docs (
//!   id        INTEGER PRIMARY KEY,
//!   body      TEXT,
//!   embedding VECTOR(384)
//! );
//! CREATE INDEX docs_fts  ON docs USING fts (body);
//!
//! -- Hot loop — 50/50 BM25 + cosine fusion, raw arithmetic per
//! -- examples/hybrid-retrieval/:
//! SELECT id
//! FROM   docs
//! WHERE  fts_match(body, ?)
//! ORDER BY 0.5 * (1.0 - bm25_score(body, ?) / 10.0)
//!        + 0.5 *        vec_distance_cosine(embedding, [...])
//! ASC
//! LIMIT 10;
//! ```
//!
//! Mirrors [`examples/hybrid-retrieval/hybrid_retrieval.rs`](../../examples/hybrid-retrieval/hybrid_retrieval.rs).
//! No off-the-shelf comparator exists in a single embedded engine — the
//! number stands on its own; that's the plan's stated stance.
//!
//! The 50/50 weighting + the BM25-rescaling factor (`bm25_score / 10`,
//! a rough normalization to put BM25 and cosine on the same scale) are
//! the same as the example's. Adjusting the weights is a `W12.v2` bump.
//!
//! ## Plan deviation
//!
//! v1 ships at **1000 docs** (and 1000 paired vectors) because of the
//! FTS doc-lengths sidecar limit; see W11's plan-deviation section
//! for the engine constraint. The vector half of the hybrid query
//! is similarly capped — `data::vector_dataset()` still produces
//! 10k vectors but only the first 1000 are inserted via `zip`. A
//! `W12.v2` lifts the cap once Phase 8.1 ships overflow chaining.

use std::path::Path;

use anyhow::{Context, Result};

use crate::data::{FTS_ROW_COUNT, FtsDataset, VectorDataset, fts_dataset, vector_dataset};
use crate::{Driver, Value, WorkloadId};

/// SQLR-23 — bumped to v2 because the hybrid SQL is now fully
/// parameterized (FTS query string twice + query vector once) instead
/// of formatted into the SQL string per call. The static template
/// hits the prepared-plan cache.
pub const W12: WorkloadId = WorkloadId {
    id: "W12",
    name: "hybrid",
    version: "v2",
};

/// Hot-loop SQL for the hybrid query — three `?` placeholders:
/// `?1` and `?2` are the FTS query string (used for both filter and
/// ranker), `?3` is the cosine query vector. Static across iterations.
pub const SELECT_SQL: &str = "SELECT id FROM docs \
     WHERE fts_match(body, ?) \
     ORDER BY 0.5 * (1.0 - bm25_score(body, ?) / 10.0) + 0.5 * vec_distance_cosine(embedding, ?) \
     ASC LIMIT 10";

/// Insert SQL for the seed pass — id, body, embedding all bound.
pub const INSERT_SQL: &str = "INSERT INTO docs (id, body, embedding) VALUES (?, ?, ?)";

pub struct HybridDataset {
    pub fts: FtsDataset,
    pub vec: VectorDataset,
}

pub fn setup<D: Driver>(driver: &D, path: &Path) -> Result<(D::Conn, HybridDataset)> {
    let mut conn = driver.open(path)?;
    driver.execute(
        &mut conn,
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(384))",
    )?;
    let fts = fts_dataset();
    let vec = vector_dataset();
    insert_rows(driver, &mut conn, &fts, &vec)?;
    driver.execute(&mut conn, "CREATE INDEX docs_fts ON docs USING fts (body)")?;
    Ok((conn, HybridDataset { fts, vec }))
}

pub fn bench_iter<D: Driver>(
    driver: &D,
    conn: &mut D::Conn,
    text_query: &str,
    vec_query: &[f32],
) -> Result<usize> {
    // SQLR-23 — every component bound through `?`. The SQL template is
    // identical across the 64 random query pairs, so the
    // prepare_cached LRU keeps a single plan hot for the entire bench
    // loop.
    let rows = driver.query_all(
        conn,
        SELECT_SQL,
        &[
            Value::Text(text_query.to_string()),
            Value::Text(text_query.to_string()),
            Value::Vector(vec_query.to_vec()),
        ],
    )?;
    Ok(rows.len())
}

pub fn correctness_check<D: Driver>(
    driver: &D,
    conn: &mut D::Conn,
    dataset: &HybridDataset,
) -> Result<()> {
    let n = bench_iter(
        driver,
        conn,
        &dataset.fts.queries[0],
        &dataset.vec.queries[0],
    )?;
    if n == 0 {
        anyhow::bail!("W12 correctness: hybrid query returned 0 rows");
    }
    if n > 10 {
        anyhow::bail!("W12 correctness: top-10 returned {n} rows (expected ≤ 10)");
    }
    Ok(())
}

/// SQLite's FTS5 virtual-table doesn't compose with VECTOR columns, so
/// W12 is SQLRite-only by design (per plan: "no off-the-shelf
/// comparator exists in a single embedded engine"). The driver-side
/// check lets the bench register fn skip non-SQLRite drivers cleanly.
pub fn driver_supports(driver_name: &str) -> bool {
    driver_name == "sqlrite"
}

fn insert_rows<D: Driver>(
    driver: &D,
    conn: &mut D::Conn,
    fts: &FtsDataset,
    vec: &VectorDataset,
) -> Result<()> {
    debug_assert_eq!(fts.rows.len(), vec.rows.len());
    driver.execute(conn, "BEGIN").context("W12 BEGIN")?;
    for (f, v) in fts.rows.iter().zip(vec.rows.iter()) {
        debug_assert_eq!(f.id, v.id);
        // SQLR-23 — id / body / embedding all bound. Same plan is
        // cached and reused across every row of the seed pass.
        driver
            .execute_with_params(
                conn,
                INSERT_SQL,
                &[
                    Value::Integer(f.id),
                    Value::Text(f.body.clone()),
                    Value::Vector(v.embedding.clone()),
                ],
            )
            .with_context(|| format!("W12 INSERT id={}", f.id))?;
    }
    driver.execute(conn, "COMMIT").context("W12 COMMIT")?;
    debug_assert_eq!(fts.rows.len(), FTS_ROW_COUNT);
    Ok(())
}
