//! On-disk persistence for a `Database`, using fixed-size paged files.
//!
//! The file is a sequence of 4 KiB pages. Page 0 holds the header
//! (magic, version, page count, schema-root pointer). Every other page carries
//! a small per-page header (type tag + next-page pointer + payload length)
//! followed by a payload of up to 4089 bytes.
//!
//! **Storage strategy (Phase 3c.4).**
//!
//! - Each `Table`'s rows are stored as **cells** in a chain of `TableLeaf`
//!   pages. Cell layout and slot directory are in `cell.rs` and
//!   `table_page.rs`; cells that exceed the inline threshold spill into an
//!   overflow chain via `overflow.rs`.
//! - A `Table`'s schema (column metadata, primary key, last rowid) is still
//!   serialized with `bincode` and stored alongside the per-table root page
//!   in the schema catalog. Phase 3c.5 will promote the catalog itself to a
//!   cell-stored internal table (`sqlrite_master`).
//! - The schema catalog lives on its own page chain. Page 0's
//!   `schema_root_page` field points to the first page.
//!
//! **Pager.** Reads and writes go through [`Pager`], which keeps a long-lived
//! in-memory snapshot of every page currently on disk plus a staging area
//! for the next commit. On commit only pages whose bytes actually changed
//! are written.

// Data-layer modules. Not every helper in these modules is used by save/open
// yet — some exist for tests, some for the B-Tree layer in Phase 3d (which
// will drive inserts and deletes against TablePages one at a time rather
// than rebuilding them from scratch on each save). Module-level
// #[allow(dead_code)] keeps the build quiet without dotting the modules
// with per-item attributes.
#[allow(dead_code)]
pub mod cell;
pub mod file;
pub mod header;
pub mod overflow;
pub mod page;
pub mod pager;
#[allow(dead_code)]
pub mod table_page;
#[allow(dead_code)]
pub mod varint;

use std::path::Path;

use bincode::config::standard;
use bincode::serde::{decode_from_slice, encode_to_vec};
use serde::{Deserialize, Serialize};

use crate::error::{Result, SQLRiteError};
use crate::sql::db::database::Database;
use crate::sql::db::table::{Column, Table, Value};
use crate::sql::pager::cell::Cell;
use crate::sql::pager::header::DbHeader;
use crate::sql::pager::overflow::{
    OVERFLOW_THRESHOLD, OverflowRef, PagedEntry, read_overflow_chain, write_overflow_chain,
};
use crate::sql::pager::page::{PAGE_HEADER_SIZE, PAGE_SIZE, PAYLOAD_PER_PAGE, PageType};
use crate::sql::pager::pager::Pager;
use crate::sql::pager::table_page::TablePage;

/// Snapshot of a table's schema that round-trips through `bincode`. The
/// schema catalog persists one of these per table, alongside the page
/// number where the table's rows begin. This dodges needing to re-parse
/// CREATE TABLE SQL on open (that's the 3c.5 approach for `sqlrite_master`).
#[derive(Debug, Serialize, Deserialize)]
struct TableSchema {
    tb_name: String,
    columns: Vec<Column>,
    primary_key: String,
    last_rowid: i64,
}

/// One entry of the schema catalog.
#[derive(Debug, Serialize, Deserialize)]
struct CatalogEntry {
    name: String,
    schema: TableSchema,
    /// First `TableLeaf` page of the table's row data. `0` means the table
    /// has no row pages (an empty table we never allocated a leaf for —
    /// but in practice we always allocate at least one empty leaf so the
    /// load path doesn't have to special-case zero).
    root_page: u32,
}

/// Reads a database file at `path` and reconstructs the in-memory `Database`,
/// leaving the long-lived `Pager` attached to it so subsequent writes can
/// auto-commit via `save_database`.
///
/// `db_name` is the logical database name; by convention the caller passes
/// the file's stem.
pub fn open_database(path: &Path, db_name: String) -> Result<Database> {
    let pager = Pager::open(path)?;

    let catalog_bytes = read_page_chain(&pager, pager.header().schema_root_page)?;
    let (catalog_entries, _): (Vec<CatalogEntry>, _) =
        decode_from_slice(&catalog_bytes, standard()).map_err(|e| {
            SQLRiteError::Internal(format!("bincode decode schema catalog: {e}"))
        })?;

    let mut db = Database::new(db_name);
    for entry in catalog_entries {
        let mut table = build_empty_table(entry.schema);
        if entry.root_page != 0 {
            load_table_rows(&pager, &mut table, entry.root_page)?;
        }
        db.tables.insert(entry.name, table);
    }

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
    let mut catalog_entries: Vec<CatalogEntry> = Vec::with_capacity(db.tables.len());

    // Iterate tables in a deterministic order so the on-disk page numbers are
    // stable across saves — required for diffing commits to skip unchanged pages.
    let mut table_names: Vec<&String> = db.tables.keys().collect();
    table_names.sort();
    for name in table_names {
        let table = &db.tables[name];
        let root_page = next_free_page;
        next_free_page = stage_table_rows(&mut pager, table, root_page)?;
        catalog_entries.push(CatalogEntry {
            name: name.clone(),
            schema: TableSchema {
                tb_name: table.tb_name.clone(),
                columns: table.columns.iter().map(strip_runtime_index).collect(),
                primary_key: table.primary_key.clone(),
                last_rowid: table.last_rowid,
            },
            root_page,
        });
    }

    let catalog_bytes = encode_to_vec(&catalog_entries, standard()).map_err(|e| {
        SQLRiteError::Internal(format!("bincode encode schema catalog: {e}"))
    })?;
    let schema_root_page = next_free_page;
    next_free_page = stage_page_chain(
        &mut pager,
        &catalog_bytes,
        PageType::SchemaRoot,
        schema_root_page,
    )?;

    pager.commit(DbHeader {
        page_count: next_free_page,
        schema_root_page,
    })?;

    if same_path {
        db.pager = Some(pager);
    }
    Ok(())
}

/// Emits a freshly-rebuilt `Column` that doesn't carry a populated index —
/// the index is a runtime structure, rebuilt by `restore_row` as cells are
/// read back. Saving the runtime index doubles storage cost for no gain.
fn strip_runtime_index(c: &Column) -> Column {
    use crate::sql::db::table::Index;
    let empty_index = match c.index {
        Index::Integer(_) => Index::Integer(std::collections::BTreeMap::new()),
        Index::Text(_) => Index::Text(std::collections::BTreeMap::new()),
        Index::None => Index::None,
    };
    Column {
        column_name: c.column_name.clone(),
        datatype: match c.datatype {
            crate::sql::db::table::DataType::Integer => {
                crate::sql::db::table::DataType::Integer
            }
            crate::sql::db::table::DataType::Text => crate::sql::db::table::DataType::Text,
            crate::sql::db::table::DataType::Real => crate::sql::db::table::DataType::Real,
            crate::sql::db::table::DataType::Bool => crate::sql::db::table::DataType::Bool,
            crate::sql::db::table::DataType::None => crate::sql::db::table::DataType::None,
            crate::sql::db::table::DataType::Invalid => {
                crate::sql::db::table::DataType::Invalid
            }
        },
        is_pk: c.is_pk,
        not_null: c.not_null,
        is_unique: c.is_unique,
        is_indexed: c.is_indexed,
        index: empty_index,
    }
}

/// Builds an empty in-memory `Table` from a deserialized `TableSchema`.
/// Every declared column gets a fresh empty row BTreeMap — subsequent
/// calls to `restore_row` populate them.
fn build_empty_table(schema: TableSchema) -> Table {
    use crate::sql::db::table::{DataType, Row};
    use std::cell::RefCell;
    use std::collections::{BTreeMap, HashMap};
    use std::rc::Rc;

    let rows: Rc<RefCell<HashMap<String, Row>>> =
        Rc::new(RefCell::new(HashMap::new()));
    {
        let mut map = rows.borrow_mut();
        for col in &schema.columns {
            let row = match col.datatype {
                DataType::Integer => Row::Integer(BTreeMap::new()),
                DataType::Text => Row::Text(BTreeMap::new()),
                DataType::Real => Row::Real(BTreeMap::new()),
                DataType::Bool => Row::Bool(BTreeMap::new()),
                _ => Row::None,
            };
            map.insert(col.column_name.clone(), row);
        }
    }

    Table {
        tb_name: schema.tb_name,
        columns: schema.columns,
        rows,
        indexes: HashMap::new(),
        last_rowid: schema.last_rowid,
        primary_key: schema.primary_key,
    }
}

/// Walks the leaf-page chain starting at `root_page`, decoding every
/// paged entry and calling `Table::restore_row` for each row.
fn load_table_rows(pager: &Pager, table: &mut Table, root_page: u32) -> Result<()> {
    let mut current = root_page;
    while current != 0 {
        let page_buf = pager.read_page(current).ok_or_else(|| {
            SQLRiteError::Internal(format!("missing leaf page {current}"))
        })?;
        if page_buf[0] != PageType::TableLeaf as u8 {
            return Err(SQLRiteError::Internal(format!(
                "page {current} tagged {} but expected TableLeaf",
                page_buf[0]
            )));
        }
        let next_leaf = u32::from_le_bytes(page_buf[1..5].try_into().unwrap());
        let payload: &[u8; PAYLOAD_PER_PAGE] = (&page_buf[PAGE_HEADER_SIZE..])
            .try_into()
            .map_err(|_| SQLRiteError::Internal("leaf payload slice size".to_string()))?;
        let leaf = TablePage::from_bytes(payload);

        for slot in 0..leaf.slot_count() {
            let entry = leaf.entry_at(slot)?;
            let cell = match entry {
                PagedEntry::Local(c) => c,
                PagedEntry::Overflow(r) => {
                    let body_bytes = read_overflow_chain(
                        pager,
                        r.first_overflow_page,
                        r.total_body_len,
                    )?;
                    let (c, _) = Cell::decode(&body_bytes, 0)?;
                    c
                }
            };
            let values: Vec<Option<Value>> = cell.values;
            table.restore_row(cell.rowid, values)?;
        }
        current = next_leaf;
    }
    Ok(())
}

/// Stages a table's row data as a chain of `TableLeaf` pages, starting at
/// `start_page`. Large rows overflow into `Overflow`-typed pages interleaved
/// in page-number order. Returns the first page number *after* the chain.
///
/// An empty table still consumes one (empty) leaf page so the catalog can
/// point at a real page and the load loop doesn't have to handle the
/// zero-page edge case.
fn stage_table_rows(
    pager: &mut Pager,
    table: &Table,
    start_page: u32,
) -> Result<u32> {
    let mut current_leaf = TablePage::empty();
    let mut current_leaf_page = start_page;
    // We'll use (current_leaf_page + 1) onward for overflow pages allocated
    // while packing this leaf. Once we commit the current leaf (either by
    // starting a new one or at end-of-table), we jump past all its overflow
    // pages and the leaf page itself.
    let mut next_free_page = start_page + 1;

    for rowid in table.rowids() {
        let values = table.extract_row(rowid);
        let local_cell = Cell::new(rowid, values);
        let local_bytes = local_cell.encode()?;

        let (entry_bytes, _overflow_pages_used) = if local_bytes.len() > OVERFLOW_THRESHOLD {
            // Write the full cell bytes to the overflow chain; the
            // OverflowRef's `total_body_len` counts those same bytes.
            let overflow_start = next_free_page;
            next_free_page = write_overflow_chain(pager, &local_bytes, overflow_start)?;
            let oref = OverflowRef {
                rowid,
                total_body_len: local_bytes.len() as u64,
                first_overflow_page: overflow_start,
            };
            (oref.encode(), next_free_page - overflow_start)
        } else {
            (local_bytes, 0)
        };

        if !current_leaf.would_fit(entry_bytes.len()) {
            // Commit the current leaf pointing at the next leaf we're about
            // to open, then start fresh.
            let next_leaf_page_num = next_free_page;
            emit_leaf(pager, current_leaf_page, &current_leaf, next_leaf_page_num);
            current_leaf = TablePage::empty();
            current_leaf_page = next_leaf_page_num;
            next_free_page += 1;

            if !current_leaf.would_fit(entry_bytes.len()) {
                // A single entry that won't fit in an empty page is a
                // programming error — even very large cells go through
                // OverflowRef (a few tens of bytes).
                return Err(SQLRiteError::Internal(format!(
                    "entry of {} bytes exceeds empty-page capacity {}",
                    entry_bytes.len(),
                    current_leaf.free_space()
                )));
            }
        }
        current_leaf.insert_entry(rowid, &entry_bytes)?;
    }

    // Final leaf: next_page = 0 (end of chain).
    emit_leaf(pager, current_leaf_page, &current_leaf, 0);
    Ok(next_free_page)
}

/// Wraps a `TablePage` in the 7-byte page header and hands it to the pager.
fn emit_leaf(pager: &mut Pager, page_num: u32, leaf: &TablePage, next_leaf: u32) {
    let mut buf = [0u8; PAGE_SIZE];
    buf[0] = PageType::TableLeaf as u8;
    buf[1..5].copy_from_slice(&next_leaf.to_le_bytes());
    // For leaf pages the legacy `payload_len` field isn't used by readers —
    // the slot directory self-describes. Set it to 0 by convention.
    buf[5..7].copy_from_slice(&0u16.to_le_bytes());
    buf[PAGE_HEADER_SIZE..].copy_from_slice(leaf.as_bytes());
    pager.stage_page(page_num, buf);
}

/// Stages `payload` as a chain of pages starting at `start_page`. The first
/// page carries `head_type`; continuations are `PageType::Overflow`. Used
/// for the schema-catalog bincode blob — tables go through
/// [`stage_table_rows`] instead.
fn stage_page_chain(
    pager: &mut Pager,
    payload: &[u8],
    head_type: PageType,
    start_page: u32,
) -> Result<u32> {
    if payload.is_empty() {
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

/// Reassembles a chained payload by following `next` pointers. Used for
/// the schema catalog; table rows are walked cell-by-cell instead.
fn read_page_chain(pager: &Pager, start_page: u32) -> Result<Vec<u8>> {
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
    fn many_small_rows_spread_across_leaves() {
        let path = tmp_path("many_rows");
        let mut db = Database::new("big".to_string());
        process_command(
            "CREATE TABLE things (id INTEGER PRIMARY KEY, data TEXT);",
            &mut db,
        )
        .unwrap();
        // Each row body is ~200 chars; one leaf page (~4 KiB usable) holds
        // roughly 15–20 of these, so 200 rows forces multiple leaves.
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

    #[test]
    fn huge_row_goes_through_overflow() {
        let path = tmp_path("overflow_row");
        let mut db = Database::new("big".to_string());
        process_command(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT);",
            &mut db,
        )
        .unwrap();
        // Single row bigger than OVERFLOW_THRESHOLD: triggers write_overflow_chain.
        let body = "A".repeat(10_000);
        process_command(&format!("INSERT INTO docs (body) VALUES ('{body}');"), &mut db)
            .unwrap();
        save_database(&mut db, &path).unwrap();

        let loaded = open_database(&path, "big".to_string()).unwrap();
        let docs = loaded.get_table("docs".to_string()).unwrap();
        let rowids = docs.rowids();
        assert_eq!(rowids.len(), 1);
        let stored = docs.get_value("body", rowids[0]);
        match stored {
            Some(Value::Text(s)) => assert_eq!(s.len(), 10_000),
            other => panic!("expected Text, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }
}
