//! End-to-end `BEGIN CONCURRENT` demo with two sibling handles.
//!
//! Run with: `cargo run --example concurrent_writers`
//!
//! Phase 11 (SQLR-22) opt-in MVCC. The example:
//!
//! 1. Opens a connection, opts the database into `journal_mode = mvcc`.
//! 2. Mints a sibling handle via `Connection::connect` so two writers
//!    share the same backing database.
//! 3. Runs two concurrent transactions:
//!       - A and B touch *disjoint* rows → both commit.
//!       - A and B touch the *same* row → the second commit fails
//!         with `SQLRiteError::Busy`; the retry takes a fresh
//!         `begin_ts`, observes the post-commit state, and lands.
//!
//! The retry loop is the canonical shape every SDK reuses; see
//! [`docs/concurrent-writes.md`](../../docs/concurrent-writes.md).

use sqlrite::{Connection, Result};

fn main() -> Result<()> {
    let mut a = Connection::open_in_memory()?;
    a.execute("PRAGMA journal_mode = mvcc")?;
    a.execute(
        "CREATE TABLE accounts (
             id      INTEGER PRIMARY KEY,
             holder  TEXT NOT NULL,
             balance INTEGER NOT NULL
         )",
    )?;
    a.execute("INSERT INTO accounts (id, holder, balance) VALUES (1, 'alice', 100)")?;
    a.execute("INSERT INTO accounts (id, holder, balance) VALUES (2, 'bob',   100)")?;

    // Sibling handle on the same Arc<Mutex<Database>>. In real apps
    // you'd hand this to a worker thread; we keep it on the main
    // thread to keep the demo readable.
    let mut b = a.connect();

    println!("=== Disjoint-row commits both succeed ===");
    a.execute("BEGIN CONCURRENT")?;
    b.execute("BEGIN CONCURRENT")?;
    a.execute("UPDATE accounts SET balance = balance + 10 WHERE id = 1")?;
    b.execute("UPDATE accounts SET balance = balance + 20 WHERE id = 2")?;
    a.execute("COMMIT")?;
    b.execute("COMMIT")?; // write-sets don't intersect — no conflict.
    print_balances(&mut a)?;

    println!("\n=== Same-row commits: A wins, B retries ===");
    // Interleave BEGINs so A.begin_ts < B.begin_ts and both see the
    // same pre-update value.
    a.execute("BEGIN CONCURRENT")?;
    b.execute("BEGIN CONCURRENT")?;
    a.execute("UPDATE accounts SET balance = balance + 5 WHERE id = 1")?;
    b.execute("UPDATE accounts SET balance = balance + 50 WHERE id = 1")?;
    a.execute("COMMIT")?;
    // B's commit sees a version newer than its own `begin_ts` → Busy.
    // The transaction is already dropped on the failed COMMIT;
    // there's no ROLLBACK to run. Start a fresh BEGIN CONCURRENT.
    match b.execute("COMMIT") {
        Err(e) if e.is_retryable() => {
            eprintln!("  B lost the race: {e}");
            b.execute("BEGIN CONCURRENT")?;
            b.execute("UPDATE accounts SET balance = balance + 50 WHERE id = 1")?;
            b.execute("COMMIT")?;
        }
        other => {
            other?;
        }
    }
    print_balances(&mut a)?;

    Ok(())
}

fn print_balances(conn: &mut Connection) -> Result<()> {
    let stmt = conn.prepare("SELECT id, holder, balance FROM accounts ORDER BY id")?;
    let mut rows = stmt.query()?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get_by_name("id")?;
        let holder: String = row.get_by_name("holder")?;
        let balance: i64 = row.get_by_name("balance")?;
        println!("  account {id} ({holder}): {balance}");
    }
    Ok(())
}
