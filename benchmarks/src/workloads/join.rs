//! W9 — INNER JOIN, customer ↔ order, probe by customer PK.
//!
//! ```sql
//! CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT);
//! CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER, amount INTEGER);
//! CREATE INDEX idx_orders_customer ON orders(customer_id);
//! -- 100k customers, 100k orders, 1:1 relationship.
//! -- Hot loop:
//! --   SELECT c.id, c.name, o.amount
//! --   FROM customers c INNER JOIN orders o ON c.id = o.customer_id
//! --   WHERE c.id = ?
//! ```
//!
//! Per-PK probe + single-row join shape. Each iter looks up one
//! customer and joins to its single matching order.
//!
//! ## Plan deviation
//!
//! The plan target is **100k-row tables**. v1 ships at **10k rows**
//! because SQLRite's join (see methodology note below) takes >5 min
//! per criterion iteration at the 100k scale on an M-series MBP — too
//! long for `make bench`. The 10k scale still surfaces the gap
//! meaningfully; bumping back to 100k follows a SQLRite join-planner
//! improvement and a `W9.v2` tag. See [`data::JOIN_ROW_COUNT`] for
//! the full deviation note.
//!
//! ## Methodology note
//!
//! SQLRite's join executor is a left-folded nested-loop driver
//! (`src/sql/executor.rs::execute_select_rows_joined`). The outer
//! WHERE narrows `c` to one row via the PK fast-path, but the inner
//! `o` scan is **un-indexed** — the engine doesn't push the
//! `c.id = o.customer_id` predicate down to an index probe on
//! `idx_orders_customer`. So per probe SQLRite walks every
//! `orders` row checking the ON predicate.
//!
//! SQLite's planner *does* push the predicate down and uses
//! `idx_orders_customer` for the inner side. The plan flags this
//! workload as "the most informative number" — the magnitude of the
//! gap is itself a roadmap input toward a real join planner /
//! optimizer pass.

use std::path::Path;

use anyhow::{Context, Result};

use crate::data::{JOIN_ROW_COUNT, JoinDataset, join_dataset};
use crate::{Driver, Value, WorkloadId};

pub const W9: WorkloadId = WorkloadId {
    id: "W9",
    name: "inner-join",
    version: "v2",
};

pub const SELECT_SQL: &str = "SELECT c.id, c.name, o.amount FROM customers AS c INNER JOIN orders AS o ON c.id = o.customer_id WHERE c.id = ?";

pub fn setup<D: Driver>(driver: &D, path: &Path) -> Result<(D::Conn, JoinDataset)> {
    let mut conn = driver.open(path)?;
    driver.execute(
        &mut conn,
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT)",
    )?;
    driver.execute(
        &mut conn,
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER, amount INTEGER)",
    )?;
    driver.execute(
        &mut conn,
        "CREATE INDEX idx_orders_customer ON orders(customer_id)",
    )?;
    let dataset = join_dataset();
    insert_rows(driver, &mut conn, &dataset)?;
    Ok((conn, dataset))
}

pub fn bench_iter<D: Driver>(
    driver: &D,
    conn: &mut D::Conn,
    customer_id: i64,
) -> Result<Vec<Value>> {
    driver.query_one(conn, SELECT_SQL, &[Value::Integer(customer_id)])
}

pub fn correctness_check<D: Driver>(
    driver: &D,
    conn: &mut D::Conn,
    dataset: &JoinDataset,
) -> Result<()> {
    for c in dataset.customers.iter().take(3) {
        let row = bench_iter(driver, conn, c.id)?;
        match (row.first(), row.get(1), row.get(2)) {
            (
                Some(Value::Integer(got_id)),
                Some(Value::Text(got_name)),
                Some(Value::Integer(_amount)),
            ) => {
                if *got_id != c.id {
                    anyhow::bail!("W9 correctness: id round-trip {got_id} != {}", c.id);
                }
                if got_name != &c.name {
                    anyhow::bail!("W9 correctness: name round-trip mismatch");
                }
            }
            other => anyhow::bail!("W9 correctness: unexpected row shape {other:?}"),
        }
    }
    Ok(())
}

fn insert_rows<D: Driver>(driver: &D, conn: &mut D::Conn, dataset: &JoinDataset) -> Result<()> {
    driver.execute(conn, "BEGIN").context("W9 BEGIN")?;
    for c in &dataset.customers {
        driver.execute_with_params(
            conn,
            "INSERT INTO customers (id, name) VALUES (?, ?)",
            &[Value::Integer(c.id), Value::Text(c.name.clone())],
        )?;
    }
    for o in &dataset.orders {
        driver.execute_with_params(
            conn,
            "INSERT INTO orders (id, customer_id, amount) VALUES (?, ?, ?)",
            &[
                Value::Integer(o.id),
                Value::Integer(o.customer_id),
                Value::Integer(o.amount),
            ],
        )?;
    }
    driver.execute(conn, "COMMIT").context("W9 COMMIT")?;
    debug_assert_eq!(dataset.customers.len(), JOIN_ROW_COUNT);
    Ok(())
}
