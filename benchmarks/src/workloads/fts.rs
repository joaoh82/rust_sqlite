//! W11 — BM25 top-10.
//!
//! ```sql
//! -- SQLRite shape (Phase 8):
//! CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT);
//! CREATE INDEX docs_fts ON docs USING fts (body);
//! SELECT id FROM docs
//! WHERE  fts_match(body, ?)
//! ORDER BY bm25_score(body, ?) DESC
//! LIMIT 10;
//!
//! -- SQLite FTS5 shape:
//! CREATE VIRTUAL TABLE docs USING fts5(body);
//! SELECT rowid FROM docs WHERE docs MATCH ? ORDER BY rank LIMIT 10;
//! ```
//!
//! ## Methodology — engine-asymmetric setup
//!
//! SQLite FTS5 is a **virtual table** — the index *is* the table. SQLRite
//! attaches an FTS index to a regular table. The two shapes aren't
//! interchangeable, so this workload's `setup` and `bench_iter` branch
//! on `driver.name()`:
//! - `"sqlrite"` → regular `docs` table + `USING fts` index +
//!   `fts_match` / `bm25_score` query.
//! - `"sqlite"` → `CREATE VIRTUAL TABLE docs USING fts5(body)` + `MATCH`
//!   query + `ORDER BY rank` (the FTS5 hidden BM25 ranker).
//!
//! That's faithful to how a perf-conscious user of each engine writes
//! BM25 queries today. The plan flags FTS5 as the comparator;
//! `rusqlite[bundled]` ships FTS5 in its libsqlite3, so no extra
//! installation is needed.
//!
//! ## Plan deviation
//!
//! Plan target is **10k docs**. v1 ships at **1000 docs** because
//! SQLRite's FTS doc-lengths sidecar (a single cell holding
//! per-doc lengths for BM25 normalization) must fit in one 4 KiB
//! page, and the encoding is ~3 bytes per doc — 10k docs blow past
//! the cell limit at COMMIT time. Bumping back to 10k follows
//! Phase 8.1 (overflow chaining) + a `W11.v2` tag. See
//! [`data::FTS_ROW_COUNT`] for the deviation note.

use std::path::Path;

use anyhow::{Context, Result};

use crate::data::{FTS_QUERY_COUNT, FTS_ROW_COUNT, FtsDataset, fts_dataset};
use crate::{Driver, Value, WorkloadId};

pub const W11: WorkloadId = WorkloadId {
    id: "W11",
    name: "bm25-top10",
    version: "v1",
};

pub fn setup<D: Driver>(driver: &D, path: &Path) -> Result<(D::Conn, FtsDataset)> {
    let mut conn = driver.open(path)?;
    create_schema(driver, &mut conn)?;
    let dataset = fts_dataset();
    insert_rows(driver, &mut conn, &dataset)?;
    Ok((conn, dataset))
}

fn create_schema<D: Driver>(driver: &D, conn: &mut D::Conn) -> Result<()> {
    match driver.name() {
        "sqlite" => {
            // FTS5 virtual table — body is the only stored column.
            driver.execute(conn, "CREATE VIRTUAL TABLE docs USING fts5(body)")?;
        }
        _ => {
            driver.execute(
                conn,
                "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)",
            )?;
            driver.execute(conn, "CREATE INDEX docs_fts ON docs USING fts (body)")?;
        }
    }
    Ok(())
}

fn insert_rows<D: Driver>(driver: &D, conn: &mut D::Conn, dataset: &FtsDataset) -> Result<()> {
    driver.execute(conn, "BEGIN").context("W11 BEGIN")?;
    match driver.name() {
        "sqlite" => {
            // FTS5 virtual tables auto-assign rowid; we don't insert
            // an explicit `id` since the body is the searchable column.
            for row in &dataset.rows {
                driver
                    .execute_with_params(
                        conn,
                        "INSERT INTO docs (body) VALUES (?)",
                        &[Value::Text(row.body.clone())],
                    )
                    .with_context(|| format!("W11 SQLite INSERT id={}", row.id))?;
            }
        }
        _ => {
            for row in &dataset.rows {
                driver
                    .execute_with_params(
                        conn,
                        "INSERT INTO docs (id, body) VALUES (?, ?)",
                        &[Value::Integer(row.id), Value::Text(row.body.clone())],
                    )
                    .with_context(|| format!("W11 SQLRite INSERT id={}", row.id))?;
            }
        }
    }
    driver.execute(conn, "COMMIT").context("W11 COMMIT")?;
    debug_assert_eq!(dataset.rows.len(), FTS_ROW_COUNT);
    debug_assert_eq!(dataset.queries.len(), FTS_QUERY_COUNT);
    Ok(())
}

pub fn bench_iter<D: Driver>(driver: &D, conn: &mut D::Conn, query: &str) -> Result<usize> {
    let rows = match driver.name() {
        "sqlite" => {
            // FTS5's default operator between bare tokens is AND, but
            // SQLRite's `fts_match` is any-of (OR per `docs/fts.md`).
            // Join the dataset's space-separated tokens with explicit
            // `OR` so both engines see semantically equivalent
            // queries.
            let or_query = query.split_whitespace().collect::<Vec<_>>().join(" OR ");
            driver.query_all(
                conn,
                "SELECT rowid FROM docs WHERE docs MATCH ? ORDER BY rank LIMIT 10",
                &[Value::Text(or_query)],
            )?
        }
        _ => {
            // SQLRite: fts_match filters, bm25_score ranks. The query
            // string appears twice — once in the WHERE filter, once in
            // the ORDER BY ranker (matches the engine's API; both
            // calls share an internal cache per `docs/fts.md`).
            let sql = format!(
                "SELECT id FROM docs WHERE fts_match(body, '{}') ORDER BY bm25_score(body, '{}') DESC LIMIT 10",
                escape_sql(query),
                escape_sql(query),
            );
            driver.query_all(conn, &sql, &[])?
        }
    };
    Ok(rows.len())
}

pub fn correctness_check<D: Driver>(
    driver: &D,
    conn: &mut D::Conn,
    dataset: &FtsDataset,
) -> Result<()> {
    // The query at index 0 has at least one term in the dictionary, so
    // it must match at least one row in the corpus.
    let n = bench_iter(driver, conn, &dataset.queries[0])?;
    if n == 0 {
        anyhow::bail!(
            "W11 correctness: query {:?} returned 0 hits — corpus generator + dictionary should overlap",
            dataset.queries[0]
        );
    }
    if n > 10 {
        anyhow::bail!("W11 correctness: top-10 returned {n} rows (expected ≤ 10)");
    }
    Ok(())
}

fn escape_sql(s: &str) -> String {
    s.replace('\'', "''")
}
