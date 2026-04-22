//! On-disk persistence for a `Database`, using fixed-size paged files.
//!
//! The file is a sequence of 4 KiB pages. Page 0 holds the header
//! (magic, version, page count, schema-root pointer). Every other page carries
//! a small per-page header (type tag + next-page pointer + payload length)
//! followed by a payload of up to 4089 bytes.
//!
//! **Storage strategy (Phase 2/3a/3b).** Each `Table` is `bincode`-serialized
//! to bytes and laid out in a chain of typed pages. The schema catalog — a
//! `Vec<(table_name, start_page)>` — is serialized the same way and its
//! starting page is recorded in the header. There is no B-Tree yet; Phase 3c
//! introduces cell-based rows and Phase 3d replaces the chains with a
//! real on-disk B-Tree. Until then, every logical record lives inside an
//! opaque bincode blob.
//!
//! **Pager.** Reads and writes go through [`Pager`], which keeps a long-lived
//! in-memory snapshot of every page currently on disk plus a staging area
//! for the next commit. On commit only pages whose bytes actually changed
//! are written, so unchanged tables stay out of the auto-save path after the
//! first write.

// Phase 3c.1/3c.2 modules — standalone data layer; not yet wired into
// save/open. The allow attributes go away in Phase 3c.4 when the higher
// layers actually call into these.
#[allow(dead_code)]
pub mod cell;
pub mod file;
pub mod header;
pub mod page;
pub mod pager;
#[allow(dead_code)]
pub mod table_page;
#[allow(dead_code)]
pub mod varint;

use std::path::Path;

use bincode::config::standard;
use bincode::serde::{decode_from_slice, encode_to_vec};

use crate::error::{Result, SQLRiteError};
use crate::sql::db::database::Database;
use crate::sql::db::table::Table;
use crate::sql::pager::header::DbHeader;
use crate::sql::pager::page::{PAGE_HEADER_SIZE, PAGE_SIZE, PAYLOAD_PER_PAGE, PageType};
use crate::sql::pager::pager::Pager;

/// Reads a database file at `path` and reconstructs the in-memory `Database`,
/// leaving the long-lived `Pager` attached to it so subsequent writes can
/// auto-commit via `save_database`.
///
/// `db_name` is the logical database name; by convention the caller passes
/// the file's stem.
pub fn open_database(path: &Path, db_name: String) -> Result<Database> {
    let pager = Pager::open(path)?;

    let catalog_bytes = read_chain(&pager, pager.header().schema_root_page)?;
    let (catalog_entries, _): (Vec<(String, u32)>, _) =
        decode_from_slice(&catalog_bytes, standard()).map_err(|e| {
            SQLRiteError::Internal(format!("bincode decode schema catalog: {e}"))
        })?;

    let mut db = Database::new(db_name);
    for (name, start_page) in catalog_entries {
        let table_bytes = read_chain(&pager, start_page)?;
        let (table, _): (Table, _) =
            decode_from_slice(&table_bytes, standard()).map_err(|e| {
                SQLRiteError::Internal(format!(
                    "bincode decode table '{name}' starting at page {start_page}: {e}"
                ))
            })?;
        db.tables.insert(name, table);
    }

    // Seed the pager's staged-page set with a no-op of the initial state so
    // the first subsequent commit behaves the same whether or not anything
    // changed. (Without this, we'd need the caller to handle "fresh pager vs
    // populated pager" separately.)
    let _ = pager.header();

    db.source_path = Some(path.to_path_buf());
    db.pager = Some(pager);
    Ok(db)
}

/// Persists `db` to disk. If `db.pager` is `Some` and was opened from `path`,
/// reuse the long-lived pager so only pages with changed bytes are written.
/// Otherwise open (or create) a fresh pager — used by `.save FILE` when the
/// target is different from the currently-open file, and by tests.
pub fn save_database(db: &mut Database, path: &Path) -> Result<()> {
    let same_path = db.source_path.as_deref() == Some(path);
    let mut pager = if same_path {
        match db.pager.take() {
            Some(p) => p,
            None if path.exists() => Pager::open(path)?,
            None => Pager::create(path)?,
        }
    } else if path.exists() {
        Pager::open(path)?
    } else {
        Pager::create(path)?
    };

    pager.clear_staged();

    // Page 0 is the header; payload pages start at 1.
    let mut next_free_page: u32 = 1;
    let mut catalog_entries: Vec<(String, u32)> = Vec::with_capacity(db.tables.len());

    // Iterate tables in a deterministic order so the on-disk page numbers are
    // stable across saves — required for diffing commits to skip unchanged pages.
    let mut table_names: Vec<&String> = db.tables.keys().collect();
    table_names.sort();
    for name in table_names {
        let table = &db.tables[name];
        let bytes = encode_to_vec(table, standard()).map_err(|e| {
            SQLRiteError::Internal(format!("bincode encode table '{name}': {e}"))
        })?;
        let start_page = next_free_page;
        next_free_page = stage_chain(&mut pager, &bytes, PageType::TableData, start_page)?;
        catalog_entries.push((name.clone(), start_page));
    }

    let catalog_bytes = encode_to_vec(&catalog_entries, standard()).map_err(|e| {
        SQLRiteError::Internal(format!("bincode encode schema catalog: {e}"))
    })?;
    let schema_root_page = next_free_page;
    next_free_page =
        stage_chain(&mut pager, &catalog_bytes, PageType::SchemaRoot, schema_root_page)?;

    pager.commit(DbHeader {
        page_count: next_free_page,
        schema_root_page,
    })?;

    if same_path {
        db.pager = Some(pager);
    }
    Ok(())
}

/// Stages `payload` as a chain of pages starting at `start_page`. The first
/// page carries `head_type`; continuations are `PageType::Overflow`. Returns
/// the first page number *after* the written chain.
fn stage_chain(
    pager: &mut Pager,
    payload: &[u8],
    head_type: PageType,
    start_page: u32,
) -> Result<u32> {
    if payload.is_empty() {
        // Still emit a single empty head page so the catalog can round-trip
        // an empty Vec.
        pager.stage_page(start_page, encode_payload_page(head_type, 0, &[])?);
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
        pager.stage_page(current_page, encode_payload_page(page_type, next, chunk)?);
        current_page += 1;
        first = false;
        remaining = rest;
    }
    Ok(current_page)
}

/// Reassembles a chained payload by following `next` pointers from
/// `start_page` until one is 0.
fn read_chain(pager: &Pager, start_page: u32) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    let mut current = start_page;
    loop {
        let raw = pager.read_page(current).ok_or_else(|| {
            SQLRiteError::Internal(format!("page {current} is missing from pager cache"))
        })?;
        let (_ty, next, payload_len) = decode_page_header(raw)?;
        out.extend_from_slice(&raw[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + payload_len]);
        if next == 0 {
            break;
        }
        current = next;
    }
    Ok(out)
}

/// Builds a `PAGE_SIZE`-byte buffer ready to hand to the pager.
fn encode_payload_page(ty: PageType, next: u32, payload: &[u8]) -> Result<[u8; PAGE_SIZE]> {
    if payload.len() > PAYLOAD_PER_PAGE {
        return Err(SQLRiteError::Internal(format!(
            "page payload {} bytes exceeds max {PAYLOAD_PER_PAGE}",
            payload.len()
        )));
    }
    let mut buf = [0u8; PAGE_SIZE];
    buf[0] = ty as u8;
    buf[1..5].copy_from_slice(&next.to_le_bytes());
    buf[5..7].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    buf[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + payload.len()].copy_from_slice(payload);
    Ok(buf)
}

fn decode_page_header(buf: &[u8; PAGE_SIZE]) -> Result<(PageType, u32, usize)> {
    let ty = PageType::from_u8(buf[0])?;
    let next = u32::from_le_bytes(buf[1..5].try_into().unwrap());
    let payload_len = u16::from_le_bytes(buf[5..7].try_into().unwrap()) as usize;
    if payload_len > PAYLOAD_PER_PAGE {
        return Err(SQLRiteError::Internal(format!(
            "corrupt page: payload length {payload_len} exceeds max"
        )));
    }
    Ok((ty, next, payload_len))
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
        let mut db = seed_db();
        save_database(&mut db, &path).expect("save");

        let loaded = open_database(&path, "test".to_string()).expect("open");
        assert_eq!(loaded.tables.len(), 2);

        let users = loaded.get_table("users".to_string()).expect("users table");
        assert_eq!(users.columns.len(), 3);
        let rowids = users.rowids();
        assert_eq!(rowids.len(), 2);
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
        save_database(&mut seed_db(), &path).unwrap();

        let mut db = open_database(&path, "test".to_string()).unwrap();
        process_command("INSERT INTO users (name, age) VALUES ('carol', 40);", &mut db).unwrap();
        save_database(&mut db, &path).unwrap();

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
        save_database(&mut db, &path).unwrap();
        let loaded = open_database(&path, "big".to_string()).unwrap();
        let things = loaded.get_table("things".to_string()).unwrap();
        assert_eq!(things.rowids().len(), 200);
        let _ = std::fs::remove_file(&path);
    }
}
