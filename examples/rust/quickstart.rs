//! Minimal end-to-end usage of the `sqlrite` crate's public API.
//!
//! Run with: `cargo run --example quickstart`
//!
//! Walks through:
//! - Opening an in-memory connection
//! - Creating a table, inserting rows
//! - Preparing a SELECT + iterating typed rows
//! - `get_by_name` + `Option<T>` NULL handling
//! - A BEGIN / INSERT / ROLLBACK block

use sqlrite::{Connection, Result};

fn main() -> Result<()> {
    let mut conn = Connection::open_in_memory()?;

    conn.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER);")?;
    conn.execute("INSERT INTO users (name, age) VALUES ('alice', 30);")?;
    conn.execute("INSERT INTO users (name, age) VALUES ('bob', 25);")?;
    conn.execute("INSERT INTO users (name, age) VALUES ('charlie', 40);")?;

    println!("All users:");
    let stmt = conn.prepare("SELECT id, name, age FROM users;")?;
    let mut rows = stmt.query()?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get_by_name("id")?;
        let name: String = row.get_by_name("name")?;
        // `Option<i64>` wraps NULL cleanly — `age` is declared
        // nullable so the typed accessor surfaces None when absent.
        let age: Option<i64> = row.get_by_name("age")?;
        println!(
            "  {} — {} ({})",
            id,
            name,
            age.map(|a| a.to_string())
                .unwrap_or_else(|| "NULL".to_string())
        );
    }

    // Transactions: BEGIN + INSERT + ROLLBACK leaves the table untouched.
    conn.execute("BEGIN;")?;
    conn.execute("INSERT INTO users (name, age) VALUES ('will_vanish', 99);")?;
    println!("\nMid-transaction row count: {}", count_users(&mut conn)?);
    conn.execute("ROLLBACK;")?;
    println!(
        "Post-rollback row count:   {} (unchanged)",
        count_users(&mut conn)?
    );

    Ok(())
}

fn count_users(conn: &mut Connection) -> Result<usize> {
    let stmt = conn.prepare("SELECT id FROM users;")?;
    let rows = stmt.query()?.collect_all()?;
    Ok(rows.len())
}
