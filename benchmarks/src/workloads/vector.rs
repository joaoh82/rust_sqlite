//! W10 — vector top-10 (cosine), brute-force vs HNSW.
//!
//! ```sql
//! CREATE TABLE vecs (id INTEGER PRIMARY KEY, embedding VECTOR(384));
//! -- 10k 384-dim vectors, deterministic per-id.
//! -- HNSW variant adds:
//! CREATE INDEX vecs_hnsw ON vecs USING hnsw (embedding);
//!
//! -- Hot loop:
//! SELECT id FROM vecs
//! ORDER BY vec_distance_cosine(embedding, [...]) ASC
//! LIMIT 10;
//! ```
//!
//! Two criterion groups land per driver: `W10.v1/brute-force` (no HNSW
//! index — every probe full-scans + bounded-heap top-k) and
//! `W10.v1/hnsw` (with the HNSW index, optimizer probes the graph
//! per [`docs/supported-sql.md`](../../docs/supported-sql.md) "HNSW
//! indexes"). The gap between the two is the headline number for
//! "did Phase 7d's ANN actually deliver?"
//!
//! ## Comparator
//!
//! Plan target was `sqlite-vec` if installable, else SQLRite-only.
//! [`sqlite-vec`](https://github.com/asg017/sqlite-vec) is a SQLite
//! extension — not part of `rusqlite[bundled]`, requires loading a
//! pre-compiled `.dylib` / `.so` at runtime. Wiring it up is a follow-
//! up; v1 ships **SQLRite-only** for both variants. The headline value
//! is the absolute SQLRite latency + the brute-force-vs-HNSW gap.

use std::path::Path;

use anyhow::{Context, Result};

use crate::data::{VECTOR_QUERY_COUNT, VECTOR_ROW_COUNT, VectorDataset, vector_dataset};
use crate::{Driver, Value, WorkloadId};

/// SQLR-23 — bumped to v2 because the bench-driver methodology changed:
/// the query vector is now bound through `Value::Vector` instead of
/// inlined as a 4 KB bracket-array literal in the SQL string. The
/// brute-force-vs-HNSW gap should widen materially because the
/// per-iter parser cost no longer dominates.
pub const W10: WorkloadId = WorkloadId {
    id: "W10",
    name: "vector-top10",
    version: "v2",
};

/// `(label, with_hnsw_index)` — two variants per driver.
pub const VARIANTS: [(&str, bool); 2] = [("brute-force", false), ("hnsw", true)];

/// Hot-loop SQL — fully parameterized: the embedding column gets
/// bound to a `Value::Vector(query)`. Static across iterations so
/// `prepare_cached` returns the same plan every call.
pub const SELECT_SQL: &str =
    "SELECT id FROM vecs ORDER BY vec_distance_cosine(embedding, ?) ASC LIMIT 10";

/// Insert SQL for the seed pass — id and embedding both bound.
pub const INSERT_SQL: &str = "INSERT INTO vecs (id, embedding) VALUES (?, ?)";

pub fn setup<D: Driver>(
    driver: &D,
    path: &Path,
    with_hnsw: bool,
) -> Result<(D::Conn, VectorDataset)> {
    let mut conn = driver.open(path)?;
    driver.execute(
        &mut conn,
        "CREATE TABLE vecs (id INTEGER PRIMARY KEY, embedding VECTOR(384))",
    )?;
    let dataset = vector_dataset();
    insert_rows(driver, &mut conn, &dataset)?;
    if with_hnsw {
        driver.execute(
            &mut conn,
            "CREATE INDEX vecs_hnsw ON vecs USING hnsw (embedding)",
        )?;
    }
    Ok((conn, dataset))
}

/// One iteration: top-10 cosine-nearest probes for `query`. Returns
/// the row count so criterion's black_box has a stable fingerprint.
///
/// SQLR-23: `query` binds through `Value::Vector` instead of being
/// formatted into the SQL string. With the vector out of the lexer's
/// hot path, the HNSW probe optimizer becomes visible vs brute-force.
pub fn bench_iter<D: Driver>(driver: &D, conn: &mut D::Conn, query: &[f32]) -> Result<usize> {
    let rows = driver.query_all(conn, SELECT_SQL, &[Value::Vector(query.to_vec())])?;
    Ok(rows.len())
}

pub fn correctness_check<D: Driver>(
    driver: &D,
    conn: &mut D::Conn,
    dataset: &VectorDataset,
) -> Result<()> {
    // Top-10 must return exactly 10 rows on a 10k-row corpus.
    let rows = bench_iter(driver, conn, &dataset.queries[0])?;
    if rows != 10 {
        anyhow::bail!("W10 correctness: top-10 returned {rows} rows, expected 10");
    }
    debug_assert_eq!(dataset.rows.len(), VECTOR_ROW_COUNT);
    debug_assert_eq!(dataset.queries.len(), VECTOR_QUERY_COUNT);
    Ok(())
}

/// SQLite doesn't speak `VECTOR(N)` columns / `vec_distance_cosine` /
/// HNSW indexes natively. The driver-side check lets the bench
/// register fn skip W10 for non-SQLRite drivers cleanly.
pub fn driver_supports(driver_name: &str) -> bool {
    driver_name == "sqlrite"
}

fn insert_rows<D: Driver>(driver: &D, conn: &mut D::Conn, dataset: &VectorDataset) -> Result<()> {
    driver.execute(conn, "BEGIN").context("W10 BEGIN")?;
    for row in &dataset.rows {
        // SQLR-23 — both id and embedding are now bound. Same
        // `prepare_cached` plan reused for every row.
        driver
            .execute_with_params(
                conn,
                INSERT_SQL,
                &[Value::Integer(row.id), Value::Vector(row.embedding.clone())],
            )
            .with_context(|| format!("W10 INSERT id={}", row.id))?;
    }
    driver.execute(conn, "COMMIT").context("W10 COMMIT")?;
    Ok(())
}
