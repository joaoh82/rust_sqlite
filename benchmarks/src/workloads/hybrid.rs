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

use crate::data::{
    FTS_ROW_COUNT, FtsDataset, VectorDataset, fts_dataset, vec_to_sql_literal, vector_dataset,
};
use crate::{Driver, Value, WorkloadId};

pub const W12: WorkloadId = WorkloadId {
    id: "W12",
    name: "hybrid",
    version: "v1",
};

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
    let lit = vec_to_sql_literal(vec_query);
    let q = escape_sql(text_query);
    let sql = format!(
        "SELECT id FROM docs \
         WHERE fts_match(body, '{q}') \
         ORDER BY 0.5 * (1.0 - bm25_score(body, '{q}') / 10.0) + 0.5 * vec_distance_cosine(embedding, {lit}) \
         ASC LIMIT 10"
    );
    let rows = driver.query_all(conn, &sql, &[])?;
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
        let lit = vec_to_sql_literal(&v.embedding);
        let sql = format!("INSERT INTO docs (id, body, embedding) VALUES (?, ?, {lit})");
        driver
            .execute_with_params(
                conn,
                &sql,
                &[Value::Integer(f.id), Value::Text(f.body.clone())],
            )
            .with_context(|| format!("W12 INSERT id={}", f.id))?;
    }
    driver.execute(conn, "COMMIT").context("W12 COMMIT")?;
    debug_assert_eq!(fts.rows.len(), FTS_ROW_COUNT);
    Ok(())
}

fn escape_sql(s: &str) -> String {
    s.replace('\'', "''")
}
