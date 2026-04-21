//! On-disk persistence for a `Database`, using fixed-size paged files.
//!
//! Phase 2 strategy: the entire state of each `Table` is `bincode`-serialized
//! and written as a chain of typed pages. The schema catalog — a `Vec` of
//! `(table_name, start_page)` — is serialized the same way and its starting
//! page is recorded in the file header (page 0).
//!
//! This is deliberately simpler than what Phase 3 will ship: there's no B-Tree
//! yet, just opaque bincode blobs inside pages. The page format is forward-
//! compatible, though — the B-Tree work only changes what sits inside the
//! payload area, not the paging or header structure.

pub mod file;
pub mod header;
pub mod page;

use std::fs::{File, OpenOptions};
use std::path::Path;

use bincode::config::standard;
use bincode::serde::{decode_from_slice, encode_to_vec};

use crate::error::{Result, SQLRiteError};
use crate::sql::db::database::Database;
use crate::sql::db::table::Table;
use crate::sql::pager::file::FileStorage;
use crate::sql::pager::header::DbHeader;
use crate::sql::pager::page::{PAYLOAD_PER_PAGE, PageType};

/// Writes `db` to `path`, truncating any existing file. Returns only once
/// the kernel has confirmed the write (fsync).
pub fn save_database(db: &Database, path: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    let mut storage = FileStorage::new(file);

    // Page 0 is reserved for the header; payload pages start at 1.
    let mut next_free_page: u32 = 1;
    let mut catalog_entries: Vec<(String, u32)> = Vec::with_capacity(db.tables.len());

    // Write each table's bincode blob into its own chain.
    for (name, table) in &db.tables {
        let bytes = encode_to_vec(table, standard()).map_err(|e| {
            SQLRiteError::Internal(format!("bincode encode table '{name}': {e}"))
        })?;
        let start_page = next_free_page;
        next_free_page = write_chain(&mut storage, &bytes, PageType::TableData, start_page)?;
        catalog_entries.push((name.clone(), start_page));
    }

    // Write the schema catalog blob into its own chain, after all tables.
    let catalog_bytes = encode_to_vec(&catalog_entries, standard()).map_err(|e| {
        SQLRiteError::Internal(format!("bincode encode schema catalog: {e}"))
    })?;
    let schema_root_page = next_free_page;
    next_free_page =
        write_chain(&mut storage, &catalog_bytes, PageType::SchemaRoot, schema_root_page)?;

    // Commit the header last, so an interrupted write before this point is
    // detectable (page 0 stays zeroed / old, magic won't match).
    storage.write_header(&DbHeader {
        page_count: next_free_page,
        schema_root_page,
    })?;
    storage.flush()?;
    Ok(())
}

/// Reads a database file at `path` and rebuilds the in-memory `Database`.
/// `db_name` is what the returned `Database` will carry internally — by
/// convention the caller passes the file's stem.
pub fn open_database(path: &Path, db_name: String) -> Result<Database> {
    let file = File::open(path)?;
    let mut storage = FileStorage::new(file);
    let header = storage.read_header()?;

    let catalog_bytes = read_chain(&mut storage, header.schema_root_page)?;
    let (catalog_entries, _): (Vec<(String, u32)>, _) =
        decode_from_slice(&catalog_bytes, standard()).map_err(|e| {
            SQLRiteError::Internal(format!("bincode decode schema catalog: {e}"))
        })?;

    let mut db = Database::new(db_name);
    for (name, start_page) in catalog_entries {
        let table_bytes = read_chain(&mut storage, start_page)?;
        let (table, _): (Table, _) =
            decode_from_slice(&table_bytes, standard()).map_err(|e| {
                SQLRiteError::Internal(format!(
                    "bincode decode table '{name}' starting at page {start_page}: {e}"
                ))
            })?;
        db.tables.insert(name, table);
    }
    Ok(db)
}

/// Writes `payload` as a chain of pages starting at `start_page`. The first
/// page carries `head_type`; all continuations are `PageType::Overflow`.
/// Returns the first page number *after* the written chain (i.e., the next
/// free page to hand out).
fn write_chain(
    storage: &mut FileStorage,
    payload: &[u8],
    head_type: PageType,
    start_page: u32,
) -> Result<u32> {
    // Special-case empty payload: we still want a single (empty) head page so
    // the catalog can round-trip an empty `Vec`.
    if payload.is_empty() {
        storage.write_page(start_page, head_type, 0, &[])?;
        return Ok(start_page + 1);
    }
    let mut remaining = payload;
    let mut current_page = start_page;
    let mut first = true;
    while !remaining.is_empty() {
        let chunk_len = remaining.len().min(PAYLOAD_PER_PAGE);
        let (chunk, rest) = remaining.split_at(chunk_len);
        let next = if rest.is_empty() { 0 } else { current_page + 1 };
        let page_type = if first { head_type } else { PageType::Overflow };
        storage.write_page(current_page, page_type, next, chunk)?;
        current_page += 1;
        first = false;
        remaining = rest;
    }
    Ok(current_page)
}

/// Reassembles a chained payload by following `next`-page pointers from
/// `start_page` until one of them is 0.
fn read_chain(storage: &mut FileStorage, start_page: u32) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    let mut current = start_page;
    loop {
        let (_ty, next, payload) = storage.read_page(current)?;
        out.extend_from_slice(&payload);
        if next == 0 {
            break;
        }
        current = next;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::process_command;

    fn seed_db() -> Database {
        let mut db = Database::new("test".to_string());
        process_command(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, age INTEGER);",
            &mut db,
        )
        .unwrap();
        process_command("INSERT INTO users (name, age) VALUES ('alice', 30);", &mut db).unwrap();
        process_command("INSERT INTO users (name, age) VALUES ('bob', 25);", &mut db).unwrap();
        process_command(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);",
            &mut db,
        )
        .unwrap();
        process_command("INSERT INTO notes (body) VALUES ('hello');", &mut db).unwrap();
        db
    }

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        // Stir in ns-precision time so parallel tests don't collide.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("sqlrite-{pid}-{nanos}-{name}.sqlrite"));
        p
    }

    #[test]
    fn round_trip_preserves_schema_and_data() {
        use crate::sql::db::table::Value;

        let path = tmp_path("roundtrip");
        let db = seed_db();
        save_database(&db, &path).expect("save");

        let loaded = open_database(&path, "test".to_string()).expect("open");
        assert_eq!(loaded.tables.len(), 2);

        let users = loaded.get_table("users".to_string()).expect("users table");
        assert_eq!(users.columns.len(), 3);
        let rowids = users.rowids();
        assert_eq!(rowids.len(), 2);
        // alice and bob should both be present with their ages.
        let names: Vec<String> = rowids
            .iter()
            .filter_map(|r| match users.get_value("name", *r) {
                Some(Value::Text(s)) => Some(s),
                _ => None,
            })
            .collect();
        assert!(names.contains(&"alice".to_string()));
        assert!(names.contains(&"bob".to_string()));

        let notes = loaded.get_table("notes".to_string()).expect("notes table");
        assert_eq!(notes.rowids().len(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn round_trip_survives_writes_after_load() {
        let path = tmp_path("after_load");
        save_database(&seed_db(), &path).unwrap();

        // Load, mutate, re-save, reload.
        let mut db = open_database(&path, "test".to_string()).unwrap();
        process_command("INSERT INTO users (name, age) VALUES ('carol', 40);", &mut db).unwrap();
        save_database(&db, &path).unwrap();

        let db2 = open_database(&path, "test".to_string()).unwrap();
        let users = db2.get_table("users".to_string()).unwrap();
        assert_eq!(users.rowids().len(), 3);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_rejects_garbage_file() {
        let path = tmp_path("bad");
        std::fs::write(&path, b"not a sqlrite database, just bytes").unwrap();
        let result = open_database(&path, "x".to_string());
        assert!(result.is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn multi_page_table_round_trips() {
        // Force the table bincode blob to exceed PAYLOAD_PER_PAGE so we exercise
        // the overflow-chain writer + reader.
        let path = tmp_path("multi_page");
        let mut db = Database::new("big".to_string());
        process_command(
            "CREATE TABLE things (id INTEGER PRIMARY KEY, data TEXT);",
            &mut db,
        )
        .unwrap();
        for i in 0..200 {
            let body = "x".repeat(200);
            let q = format!("INSERT INTO things (data) VALUES ('row-{i}-{body}');");
            process_command(&q, &mut db).unwrap();
        }
        save_database(&db, &path).unwrap();
        let loaded = open_database(&path, "big".to_string()).unwrap();
        let things = loaded.get_table("things".to_string()).unwrap();
        assert_eq!(things.rowids().len(), 200);
        let _ = std::fs::remove_file(&path);
    }
}
