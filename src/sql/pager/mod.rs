//! On-disk persistence for a `Database`, using fixed-size paged files.
//!
//! The file is a sequence of 4 KiB pages. Page 0 holds the header
//! (magic, version, page count, schema-root pointer). Every other page carries
//! a small per-page header (type tag + next-page pointer + payload length)
//! followed by a payload of up to 4089 bytes.
//!
//! **Storage strategy (format version 2, Phase 3c.5).**
//!
//! - Each `Table`'s rows live as **cells** in a chain of `TableLeaf` pages.
//!   Cell layout and slot directory are in `cell.rs` / `table_page.rs`;
//!   cells that exceed the inline threshold spill into an overflow chain
//!   via `overflow.rs`.
//! - The schema catalog is itself a regular table named `sqlrite_master`,
//!   with one row per user table:
//!       `(name TEXT PRIMARY KEY, sql TEXT NOT NULL,
//!         rootpage INTEGER NOT NULL, last_rowid INTEGER NOT NULL)`
//!   This is the SQLite-style approach: the schema of `sqlrite_master`
//!   itself is hardcoded into the engine so the open path can bootstrap.
//! - Page 0's `schema_root_page` field points at the first leaf of
//!   `sqlrite_master`.
//!
//! **Format version.** Version 2 is not compatible with files produced by
//! earlier commits. Opening a v1 file returns a clean error — users on
//! old files have to regenerate them from CREATE/INSERT, as there's no
//! production data to migrate yet.

// Data-layer modules. Not every helper in these modules is used by save/open
// yet — some exist for tests, some for the B-Tree layer in Phase 3d.
// Module-level #[allow(dead_code)] keeps the build quiet without dotting
// the modules with per-item attributes.
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

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::rc::Rc;

use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;

use crate::error::{Result, SQLRiteError};
use crate::sql::db::database::Database;
use crate::sql::db::table::{Column, DataType, Row, Table, Value};
use crate::sql::parser::create::CreateQuery;
use crate::sql::pager::cell::Cell;
use crate::sql::pager::header::DbHeader;
use crate::sql::pager::overflow::{
    OVERFLOW_THRESHOLD, OverflowRef, PagedEntry, read_overflow_chain, write_overflow_chain,
};
use crate::sql::pager::page::{PAGE_HEADER_SIZE, PAGE_SIZE, PAYLOAD_PER_PAGE, PageType};
use crate::sql::pager::pager::Pager;
use crate::sql::pager::table_page::TablePage;

/// Name of the internal catalog table. Reserved — user CREATEs of this
/// name must be rejected upstream.
pub const MASTER_TABLE_NAME: &str = "sqlrite_master";

/// Opens a database file and reconstructs the in-memory `Database`,
/// leaving the long-lived `Pager` attached for subsequent auto-save.
pub fn open_database(path: &Path, db_name: String) -> Result<Database> {
    let pager = Pager::open(path)?;

    // 1. Load sqlrite_master from the leaf chain at header.schema_root_page.
    let mut master = build_empty_master_table();
    load_table_rows(&pager, &mut master, pager.header().schema_root_page)?;

    // 2. Each master row describes a user table. Re-parse its `sql` to
    //    reconstruct the column list, then walk its rootpage chain.
    let mut db = Database::new(db_name);
    for rowid in master.rowids() {
        let name = take_text(&master, "name", rowid)?;
        let sql = take_text(&master, "sql", rowid)?;
        let rootpage = take_integer(&master, "rootpage", rowid)? as u32;
        let last_rowid = take_integer(&master, "last_rowid", rowid)?;

        let (parsed_name, columns) = parse_create_sql(&sql)?;
        if parsed_name != name {
            return Err(SQLRiteError::Internal(format!(
                "sqlrite_master row '{name}' carries SQL for '{parsed_name}' — corrupt catalog?"
            )));
        }
        let mut table = build_empty_table(&name, columns, last_rowid);
        if rootpage != 0 {
            load_table_rows(&pager, &mut table, rootpage)?;
        }
        // restore_row may have advanced last_rowid past the stored value if
        // the rows we loaded contain larger rowids; clamp back up either way.
        if last_rowid > table.last_rowid {
            table.last_rowid = last_rowid;
        }
        db.tables.insert(name, table);
    }

    db.source_path = Some(path.to_path_buf());
    db.pager = Some(pager);
    Ok(db)
}

/// Persists `db` to disk. Same diff-commit behavior as before: only pages
/// whose bytes actually changed get written.
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

    // 1. Stage each user table's leaf chain, collecting (name, sql, root, last_rowid).
    let mut master_rows: Vec<(String, String, u32, i64)> =
        Vec::with_capacity(db.tables.len());
    let mut table_names: Vec<&String> = db.tables.keys().collect();
    table_names.sort();
    for name in table_names {
        if name == MASTER_TABLE_NAME {
            // Shouldn't happen (CREATE TABLE rejects the name), but guard
            // against a programmer-reached state.
            return Err(SQLRiteError::Internal(format!(
                "user table cannot be named '{MASTER_TABLE_NAME}' (reserved)"
            )));
        }
        let table = &db.tables[name];
        let rootpage = next_free_page;
        next_free_page = stage_table_rows(&mut pager, table, rootpage)?;
        master_rows.push((
            name.clone(),
            table_to_create_sql(table),
            rootpage,
            table.last_rowid,
        ));
    }

    // 2. Build an in-memory sqlrite_master with one row per user table,
    //    then stage it via the same code path.
    let mut master = build_empty_master_table();
    for (i, (name, sql, rootpage, last_rowid)) in master_rows.into_iter().enumerate() {
        // Rowid is 1-based and sequential — deterministic so diffing
        // commits stay byte-stable when nothing changes.
        let rowid = (i as i64) + 1;
        master.restore_row(
            rowid,
            vec![
                Some(Value::Text(name)),
                Some(Value::Text(sql)),
                Some(Value::Integer(rootpage as i64)),
                Some(Value::Integer(last_rowid)),
            ],
        )?;
    }
    let master_root = next_free_page;
    next_free_page = stage_table_rows(&mut pager, &master, master_root)?;

    pager.commit(DbHeader {
        page_count: next_free_page,
        schema_root_page: master_root,
    })?;

    if same_path {
        db.pager = Some(pager);
    }
    Ok(())
}

// -------------------------------------------------------------------------
// sqlrite_master — hardcoded catalog table schema

fn build_empty_master_table() -> Table {
    let columns = vec![
        Column::new("name".into(), "text".into(), true, true, true),
        Column::new("sql".into(), "text".into(), false, true, false),
        Column::new("rootpage".into(), "integer".into(), false, true, false),
        Column::new("last_rowid".into(), "integer".into(), false, true, false),
    ];
    build_empty_table(MASTER_TABLE_NAME, columns, 0)
}

/// Reads a required Text column from a known-good catalog row.
fn take_text(table: &Table, col: &str, rowid: i64) -> Result<String> {
    match table.get_value(col, rowid) {
        Some(Value::Text(s)) => Ok(s),
        other => Err(SQLRiteError::Internal(format!(
            "sqlrite_master column '{col}' at rowid {rowid}: expected Text, got {other:?}"
        ))),
    }
}

/// Reads a required Integer column from a known-good catalog row.
fn take_integer(table: &Table, col: &str, rowid: i64) -> Result<i64> {
    match table.get_value(col, rowid) {
        Some(Value::Integer(v)) => Ok(v),
        other => Err(SQLRiteError::Internal(format!(
            "sqlrite_master column '{col}' at rowid {rowid}: expected Integer, got {other:?}"
        ))),
    }
}

// -------------------------------------------------------------------------
// CREATE-TABLE SQL synthesis and re-parsing

/// Synthesizes a CREATE TABLE SQL string that recreates the table's schema.
/// Deterministic: same schema → same SQL, so diffing commits stay stable.
fn table_to_create_sql(table: &Table) -> String {
    let mut parts = Vec::with_capacity(table.columns.len());
    for c in &table.columns {
        let ty = match c.datatype {
            DataType::Integer => "INTEGER",
            DataType::Text => "TEXT",
            DataType::Real => "REAL",
            DataType::Bool => "BOOLEAN",
            DataType::None | DataType::Invalid => "TEXT",
        };
        let mut piece = format!("{} {}", c.column_name, ty);
        if c.is_pk {
            piece.push_str(" PRIMARY KEY");
        } else {
            if c.is_unique {
                piece.push_str(" UNIQUE");
            }
            if c.not_null {
                piece.push_str(" NOT NULL");
            }
        }
        parts.push(piece);
    }
    format!("CREATE TABLE {} ({});", table.tb_name, parts.join(", "))
}

/// Reverses `table_to_create_sql`: feeds the SQL back through `sqlparser`
/// and produces our internal column list. Returns `(table_name, columns)`.
fn parse_create_sql(sql: &str) -> Result<(String, Vec<Column>)> {
    let dialect = SQLiteDialect {};
    let mut ast = Parser::parse_sql(&dialect, sql).map_err(SQLRiteError::from)?;
    let stmt = ast.pop().ok_or_else(|| {
        SQLRiteError::Internal("sqlrite_master row held an empty SQL string".to_string())
    })?;
    let create = CreateQuery::new(&stmt)?;
    let columns = create
        .columns
        .into_iter()
        .map(|pc| Column::new(pc.name, pc.datatype, pc.is_pk, pc.not_null, pc.is_unique))
        .collect();
    Ok((create.table_name, columns))
}

// -------------------------------------------------------------------------
// In-memory table (re)construction

/// Builds an empty in-memory `Table` given the declared columns.
fn build_empty_table(name: &str, columns: Vec<Column>, last_rowid: i64) -> Table {
    let rows: Rc<RefCell<HashMap<String, Row>>> = Rc::new(RefCell::new(HashMap::new()));
    {
        let mut map = rows.borrow_mut();
        for col in &columns {
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

    let primary_key = columns
        .iter()
        .find(|c| c.is_pk)
        .map(|c| c.column_name.clone())
        .unwrap_or_else(|| "-1".to_string());

    Table {
        tb_name: name.to_string(),
        columns,
        rows,
        indexes: HashMap::new(),
        last_rowid,
        primary_key,
    }
}

// -------------------------------------------------------------------------
// Leaf-chain read / write

/// Walks the leaf chain starting at `root_page`, decoding every cell and
/// calling `Table::restore_row` for each row.
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
            table.restore_row(cell.rowid, cell.values)?;
        }
        current = next_leaf;
    }
    Ok(())
}

/// Stages a table's row data as a chain of `TableLeaf` pages starting at
/// `start_page`. Returns the first free page number after the chain.
fn stage_table_rows(
    pager: &mut Pager,
    table: &Table,
    start_page: u32,
) -> Result<u32> {
    let mut current_leaf = TablePage::empty();
    let mut current_leaf_page = start_page;
    let mut next_free_page = start_page + 1;

    for rowid in table.rowids() {
        let values = table.extract_row(rowid);
        let local_cell = Cell::new(rowid, values);
        let local_bytes = local_cell.encode()?;

        let entry_bytes = if local_bytes.len() > OVERFLOW_THRESHOLD {
            let overflow_start = next_free_page;
            next_free_page = write_overflow_chain(pager, &local_bytes, overflow_start)?;
            OverflowRef {
                rowid,
                total_body_len: local_bytes.len() as u64,
                first_overflow_page: overflow_start,
            }
            .encode()
        } else {
            local_bytes
        };

        if !current_leaf.would_fit(entry_bytes.len()) {
            let next_leaf_page_num = next_free_page;
            emit_leaf(pager, current_leaf_page, &current_leaf, next_leaf_page_num);
            current_leaf = TablePage::empty();
            current_leaf_page = next_leaf_page_num;
            next_free_page += 1;

            if !current_leaf.would_fit(entry_bytes.len()) {
                return Err(SQLRiteError::Internal(format!(
                    "entry of {} bytes exceeds empty-page capacity {}",
                    entry_bytes.len(),
                    current_leaf.free_space()
                )));
            }
        }
        current_leaf.insert_entry(rowid, &entry_bytes)?;
    }

    emit_leaf(pager, current_leaf_page, &current_leaf, 0);
    Ok(next_free_page)
}

/// Wraps a `TablePage` in the 7-byte page header and hands it to the pager.
fn emit_leaf(pager: &mut Pager, page_num: u32, leaf: &TablePage, next_leaf: u32) {
    let mut buf = [0u8; PAGE_SIZE];
    buf[0] = PageType::TableLeaf as u8;
    buf[1..5].copy_from_slice(&next_leaf.to_le_bytes());
    // For leaf pages the legacy `payload_len` field isn't used — the slot
    // directory self-describes. Zero it by convention.
    buf[5..7].copy_from_slice(&0u16.to_le_bytes());
    buf[PAGE_HEADER_SIZE..].copy_from_slice(leaf.as_bytes());
    pager.stage_page(page_num, buf);
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

    #[test]
    fn create_sql_synthesis_round_trips() {
        // Build a table via CREATE, then verify table_to_create_sql +
        // parse_create_sql reproduce an equivalent column list.
        let mut db = Database::new("x".to_string());
        process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, tag TEXT UNIQUE, note TEXT NOT NULL);",
            &mut db,
        )
        .unwrap();
        let t = db.get_table("t".to_string()).unwrap();
        let sql = table_to_create_sql(t);
        let (name, cols) = parse_create_sql(&sql).unwrap();
        assert_eq!(name, "t");
        assert_eq!(cols.len(), 3);
        assert!(cols[0].is_pk);
        assert!(cols[1].is_unique);
        assert!(cols[2].not_null);
    }

    #[test]
    fn sqlrite_master_is_not_exposed_as_a_user_table() {
        // After open, the public db.tables map should not list the master.
        let path = tmp_path("no_master");
        save_database(&mut seed_db(), &path).unwrap();
        let loaded = open_database(&path, "x".to_string()).unwrap();
        assert!(!loaded.tables.contains_key(MASTER_TABLE_NAME));
        let _ = std::fs::remove_file(&path);
    }
}
