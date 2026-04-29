//! Schema introspection — turn an open `Connection` into a textual
//! schema dump the LLM can ground its SQL generation on.
//!
//! ## Why we don't `SELECT FROM sqlrite_master`
//!
//! The `sqlrite_master` catalog persists the original `CREATE TABLE`
//! SQL, so reflecting via SQL would work. But we already have the
//! same information typed in `Database.tables` (declared columns,
//! primary key, NOT NULL / UNIQUE, vector dimensions, …) — going
//! through SQL would mean parsing the round-tripped CREATE statement
//! a second time. Walking the typed structure is cheaper and matches
//! whatever the engine considers authoritative *right now* (a
//! relevant distinction inside an open transaction, where the
//! catalog's persisted state may already be stale).
//!
//! ## Determinism (matters for prompt caching)
//!
//! Tables are dumped in alphabetical order, columns in declaration
//! order. The output is byte-stable for a fixed schema — that's
//! what lets `cache_control: ephemeral` actually hit on repeat
//! calls. Any change to the dump format (adding a clause, changing
//! whitespace) will invalidate the cache once for everyone, but
//! steady-state hits are cheap.

use std::fmt::Write;

use sqlrite::Connection;
use sqlrite::sql::db::database::Database;
use sqlrite::sql::db::table::{DataType, Table};

/// Render the schema of every user-visible table as a sequence of
/// `CREATE TABLE … (…);` statements, sorted alphabetically by name.
///
/// The internal `sqlrite_master` catalog is filtered out — the LLM
/// doesn't need to know about it and including it would confuse the
/// generation.
pub fn dump_schema(conn: &Connection) -> String {
    dump_database(conn.database())
}

/// Same as [`dump_schema`], but takes a `Database` reference directly.
/// Useful for callers that already hold a `Database` (e.g., advanced
/// embedders bypassing the public `Connection` API).
pub fn dump_database(db: &Database) -> String {
    let mut names: Vec<&str> = db
        .tables
        .keys()
        .filter(|k| k.as_str() != "sqlrite_master")
        .map(String::as_str)
        .collect();
    names.sort_unstable();

    let mut out = String::new();
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let table = match db.tables.get(*name) {
            Some(t) => t,
            None => continue,
        };
        format_create_table(table, &mut out);
    }
    out
}

/// Format one `Table` as a single `CREATE TABLE` statement.
///
/// We don't emit the secondary-index DDL here — indexes are an
/// implementation detail of `WHERE col = literal` performance, not
/// something the LLM needs to reason about when choosing what columns
/// to project or filter on. (HNSW indexes are different — they affect
/// what queries are even *possible* efficiently. Phase 7g.x follow-up
/// will add an `[indexed: hnsw, M=16, ef=200]` annotation on indexed
/// vector columns once the prompt budget can absorb it.)
fn format_create_table(table: &Table, out: &mut String) {
    let _ = writeln!(out, "CREATE TABLE {} (", table.tb_name);
    for (i, col) in table.columns.iter().enumerate() {
        let datatype = render_datatype(&col.datatype);
        let mut clauses: Vec<&'static str> = Vec::new();
        if col.is_pk {
            clauses.push("PRIMARY KEY");
        }
        if col.is_unique && !col.is_pk {
            // PRIMARY KEY already implies UNIQUE; SQLite's reflection
            // never double-prints it and neither do we.
            clauses.push("UNIQUE");
        }
        if col.not_null && !col.is_pk {
            clauses.push("NOT NULL");
        }

        let trailing = if i + 1 == table.columns.len() {
            ""
        } else {
            ","
        };
        if clauses.is_empty() {
            let _ = writeln!(out, "  {} {}{}", col.column_name, datatype, trailing);
        } else {
            let _ = writeln!(
                out,
                "  {} {} {}{}",
                col.column_name,
                datatype,
                clauses.join(" "),
                trailing
            );
        }
    }
    out.push_str(");\n");
}

fn render_datatype(dt: &DataType) -> String {
    // Match the canonical SQL the parser accepts on the way in (so
    // round-trip is `dump → CREATE TABLE` → identical schema). The
    // engine's internal `Display` impl is debug-leaning ("Boolean",
    // "Vector(384)") — different enough from the on-the-wire form
    // that we render explicitly here.
    match dt {
        DataType::Integer => "INTEGER".to_string(),
        DataType::Text => "TEXT".to_string(),
        DataType::Real => "REAL".to_string(),
        DataType::Bool => "BOOLEAN".to_string(),
        DataType::Vector(dim) => format!("VECTOR({dim})"),
        DataType::Json => "JSON".to_string(),
        DataType::None => "TEXT".to_string(),
        // Invalid columns shouldn't reach a schema dump in practice
        // (the parser rejects them at CREATE time), but if one slips
        // through we render it as TEXT rather than panicking — the
        // LLM will at worst suggest a TEXT-shaped query.
        DataType::Invalid => "TEXT".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlrite::Connection;

    fn open() -> Connection {
        Connection::open_in_memory().expect("open in-memory db")
    }

    #[test]
    fn empty_schema_returns_empty_string() {
        let conn = open();
        assert_eq!(dump_schema(&conn), "");
    }

    #[test]
    fn single_table_round_trips() {
        let mut conn = open();
        conn.execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT UNIQUE)",
        )
        .unwrap();
        let dump = dump_schema(&conn);
        assert!(dump.contains("CREATE TABLE users ("), "got: {dump}");
        assert!(dump.contains("id INTEGER PRIMARY KEY"));
        assert!(dump.contains("name TEXT NOT NULL"));
        assert!(dump.contains("email TEXT UNIQUE"));
    }

    #[test]
    fn vector_and_json_columns_render_with_canonical_keywords() {
        let mut conn = open();
        conn.execute(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, embedding VECTOR(384), payload JSON)",
        )
        .unwrap();
        let dump = dump_schema(&conn);
        assert!(dump.contains("embedding VECTOR(384)"), "got: {dump}");
        assert!(dump.contains("payload JSON"), "got: {dump}");
    }

    #[test]
    fn tables_emitted_in_alphabetical_order() {
        let mut conn = open();
        conn.execute("CREATE TABLE zebra (id INTEGER PRIMARY KEY)")
            .unwrap();
        conn.execute("CREATE TABLE alpha (id INTEGER PRIMARY KEY)")
            .unwrap();
        conn.execute("CREATE TABLE mango (id INTEGER PRIMARY KEY)")
            .unwrap();
        let dump = dump_schema(&conn);
        let alpha = dump.find("CREATE TABLE alpha").unwrap();
        let mango = dump.find("CREATE TABLE mango").unwrap();
        let zebra = dump.find("CREATE TABLE zebra").unwrap();
        assert!(
            alpha < mango && mango < zebra,
            "non-deterministic order: {dump}"
        );
    }

    #[test]
    fn sqlrite_master_is_excluded() {
        let mut conn = open();
        conn.execute("CREATE TABLE users (id INTEGER PRIMARY KEY)")
            .unwrap();
        let dump = dump_schema(&conn);
        assert!(
            !dump.contains("sqlrite_master"),
            "internal catalog leaked: {dump}"
        );
    }

    #[test]
    fn dump_is_byte_stable_across_calls() {
        // The whole point of putting the schema dump behind a
        // `cache_control: ephemeral` breakpoint is that this is true.
        // If sort order ever becomes non-deterministic (e.g. by walking
        // the HashMap directly), prompt caching silently degrades.
        let mut conn = open();
        conn.execute("CREATE TABLE a (id INTEGER PRIMARY KEY, x TEXT)")
            .unwrap();
        conn.execute("CREATE TABLE b (id INTEGER PRIMARY KEY, y REAL)")
            .unwrap();
        conn.execute("CREATE TABLE c (id INTEGER PRIMARY KEY, z BOOLEAN)")
            .unwrap();
        let first = dump_schema(&conn);
        for _ in 0..20 {
            assert_eq!(dump_schema(&conn), first);
        }
    }
}
