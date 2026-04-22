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
// yet — some exist for tests, some for future maintenance operations.
// Module-level #[allow(dead_code)] keeps the build quiet without dotting
// the modules with per-item attributes.
#[allow(dead_code)]
pub mod cell;
pub mod file;
pub mod header;
#[allow(dead_code)]
pub mod interior_page;
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
use crate::sql::pager::interior_page::{InteriorCell, InteriorPage};
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
        let (rootpage, new_next) = stage_table_btree(&mut pager, table, next_free_page)?;
        next_free_page = new_next;
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
    let (master_root, master_next) = stage_table_btree(&mut pager, &master, next_free_page)?;
    next_free_page = master_next;

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

/// Walks a table's B-Tree from `root_page`, following the leftmost-child
/// chain down to the first leaf, then iterating leaves via their sibling
/// `next_page` pointers. Every cell is decoded and replayed into `table`.
///
/// Open-path note: we eagerly materialize the entire table into `Table`'s
/// in-memory maps. Phase 5 will introduce a `Cursor` that hits the pager
/// on demand so queries can stream through the tree without a full upfront
/// load.
fn load_table_rows(pager: &Pager, table: &mut Table, root_page: u32) -> Result<()> {
    let first_leaf = find_leftmost_leaf(pager, root_page)?;
    let mut current = first_leaf;
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

/// Descends from `root_page` through `InteriorNode` pages, always taking
/// the leftmost child, until a `TableLeaf` is reached. Returns that leaf's
/// page number. A root that's already a leaf is returned as-is.
fn find_leftmost_leaf(pager: &Pager, root_page: u32) -> Result<u32> {
    let mut current = root_page;
    loop {
        let page_buf = pager.read_page(current).ok_or_else(|| {
            SQLRiteError::Internal(format!("missing page {current} during tree descent"))
        })?;
        match page_buf[0] {
            t if t == PageType::TableLeaf as u8 => return Ok(current),
            t if t == PageType::InteriorNode as u8 => {
                let payload: &[u8; PAYLOAD_PER_PAGE] =
                    (&page_buf[PAGE_HEADER_SIZE..]).try_into().map_err(|_| {
                        SQLRiteError::Internal("interior payload slice size".to_string())
                    })?;
                let interior = InteriorPage::from_bytes(payload);
                current = interior.leftmost_child()?;
            }
            other => {
                return Err(SQLRiteError::Internal(format!(
                    "unexpected page type {other} during tree descent at page {current}"
                )));
            }
        }
    }
}

/// Stages a table's B-Tree starting at `start_page`. Returns
/// `(root_page, next_free_page)`. Builds bottom-up:
///
/// 1. Pack all row cells into `TableLeaf` pages, chaining them via each
///    leaf's `next_page` sibling pointer (for fast sequential scans).
/// 2. If the table fits in a single leaf, that leaf is the root.
/// 3. Otherwise, group leaves into `InteriorNode` pages; recurse up the
///    tree until one root remains.
///
/// Deterministic: same in-memory rows → same pages at same offsets, so
/// the Pager's diff commit still skips unchanged tables.
fn stage_table_btree(
    pager: &mut Pager,
    table: &Table,
    start_page: u32,
) -> Result<(u32, u32)> {
    let (leaves, mut next_free_page) = stage_leaves(pager, table, start_page)?;
    if leaves.len() == 1 {
        return Ok((leaves[0].0, next_free_page));
    }
    let mut level: Vec<(u32, i64)> = leaves;
    while level.len() > 1 {
        let (next_level, new_next_free) = stage_interior_level(pager, &level, next_free_page)?;
        next_free_page = new_next_free;
        level = next_level;
    }
    Ok((level[0].0, next_free_page))
}

/// Packs the table's rows into a sibling-linked chain of `TableLeaf` pages.
/// Returns each leaf's `(page_number, max_rowid)` (used by the next level
/// up to build divider cells) and the first free page after the chain
/// including any overflow pages allocated for oversized cells.
fn stage_leaves(
    pager: &mut Pager,
    table: &Table,
    start_page: u32,
) -> Result<(Vec<(u32, i64)>, u32)> {
    let mut leaves: Vec<(u32, i64)> = Vec::new();
    let mut current_leaf = TablePage::empty();
    let mut current_leaf_page = start_page;
    let mut current_max_rowid: Option<i64> = None;
    let mut next_free_page = start_page + 1;

    for rowid in table.rowids() {
        let entry_bytes = build_row_entry(pager, table, rowid, &mut next_free_page)?;

        if !current_leaf.would_fit(entry_bytes.len()) {
            // Commit the current leaf. Its sibling next_page is the page
            // number where the new leaf will go — which is next_free_page
            // right now (no overflow pages have been allocated between
            // this decision and the new leaf's allocation below).
            let next_leaf_page_num = next_free_page;
            emit_leaf(pager, current_leaf_page, &current_leaf, next_leaf_page_num);
            leaves.push((
                current_leaf_page,
                current_max_rowid.unwrap_or(i64::MIN),
            ));
            current_leaf = TablePage::empty();
            current_leaf_page = next_leaf_page_num;
            next_free_page += 1;
            // current_max_rowid is reassigned by the insert below; no need
            // to zero it out here.

            if !current_leaf.would_fit(entry_bytes.len()) {
                return Err(SQLRiteError::Internal(format!(
                    "entry of {} bytes exceeds empty-page capacity {}",
                    entry_bytes.len(),
                    current_leaf.free_space()
                )));
            }
        }
        current_leaf.insert_entry(rowid, &entry_bytes)?;
        current_max_rowid = Some(rowid);
    }

    // Final leaf: sibling next_page = 0 (end of chain).
    emit_leaf(pager, current_leaf_page, &current_leaf, 0);
    leaves.push((current_leaf_page, current_max_rowid.unwrap_or(i64::MIN)));
    Ok((leaves, next_free_page))
}

/// Encodes a single row's on-leaf entry — either the local cell bytes, or
/// an `OverflowRef` pointing at a freshly-allocated overflow chain if the
/// encoded cell exceeded the inline threshold. Advances `next_free_page`
/// past any overflow pages used.
fn build_row_entry(
    pager: &mut Pager,
    table: &Table,
    rowid: i64,
    next_free_page: &mut u32,
) -> Result<Vec<u8>> {
    let values = table.extract_row(rowid);
    let local_cell = Cell::new(rowid, values);
    let local_bytes = local_cell.encode()?;
    if local_bytes.len() > OVERFLOW_THRESHOLD {
        let overflow_start = *next_free_page;
        *next_free_page = write_overflow_chain(pager, &local_bytes, overflow_start)?;
        Ok(OverflowRef {
            rowid,
            total_body_len: local_bytes.len() as u64,
            first_overflow_page: overflow_start,
        }
        .encode())
    } else {
        Ok(local_bytes)
    }
}

/// Builds one level of `InteriorNode` pages above the given children.
/// Each interior packs as many dividers as will fit; the last child
/// assigned to an interior becomes its `rightmost_child`. Returns the
/// emitted interior pages as `(page_number, max_rowid_in_subtree)` so the
/// next level can build on top of them.
fn stage_interior_level(
    pager: &mut Pager,
    children: &[(u32, i64)],
    start_page: u32,
) -> Result<(Vec<(u32, i64)>, u32)> {
    let mut next_level: Vec<(u32, i64)> = Vec::new();
    let mut next_free_page = start_page;
    let mut idx = 0usize;

    while idx < children.len() {
        let interior_page_num = next_free_page;
        next_free_page += 1;

        // Seed the interior with the first unassigned child as its
        // rightmost. As we add more children, the previous rightmost
        // graduates to being a divider and the new arrival takes over
        // as rightmost.
        let (mut rightmost_child_page, mut rightmost_child_max) = children[idx];
        idx += 1;
        let mut interior = InteriorPage::empty(rightmost_child_page);

        while idx < children.len() {
            let new_divider_cell = InteriorCell {
                divider_rowid: rightmost_child_max,
                child_page: rightmost_child_page,
            };
            let new_divider_bytes = new_divider_cell.encode();
            if !interior.would_fit(new_divider_bytes.len()) {
                break;
            }
            interior.insert_divider(rightmost_child_max, rightmost_child_page)?;
            let (next_child_page, next_child_max) = children[idx];
            interior.set_rightmost_child(next_child_page);
            rightmost_child_page = next_child_page;
            rightmost_child_max = next_child_max;
            idx += 1;
        }

        emit_interior(pager, interior_page_num, &interior);
        next_level.push((interior_page_num, rightmost_child_max));
    }

    Ok((next_level, next_free_page))
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

/// Wraps an `InteriorPage` in the 7-byte page header. Interior pages
/// don't use `next_page` (there's no sibling chain between interiors);
/// `payload_len` is also unused (the slot directory self-describes).
fn emit_interior(pager: &mut Pager, page_num: u32, interior: &InteriorPage) {
    let mut buf = [0u8; PAGE_SIZE];
    buf[0] = PageType::InteriorNode as u8;
    buf[1..5].copy_from_slice(&0u32.to_le_bytes());
    buf[5..7].copy_from_slice(&0u16.to_le_bytes());
    buf[PAGE_HEADER_SIZE..].copy_from_slice(interior.as_bytes());
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

    #[test]
    fn multi_leaf_table_produces_an_interior_root() {
        // 200 fat rows force the table into multiple leaves, which means
        // save_database must build at least one InteriorNode above them.
        // The test verifies the round-trip works and confirms the root is
        // indeed an interior page (not a leaf) by reading the page type
        // directly out of the open pager.
        let path = tmp_path("multi_leaf_interior");
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

        // Confirm the round-trip preserved all 200 rows.
        let loaded = open_database(&path, "big".to_string()).unwrap();
        let things = loaded.get_table("things".to_string()).unwrap();
        assert_eq!(things.rowids().len(), 200);

        // Peek at `things`'s root page via the pager attached to the
        // loaded DB and check it's an InteriorNode, not a leaf.
        let pager = loaded.pager.as_ref().expect("loaded DB should have a pager");
        // sqlrite_master's row for `things` holds its root page. Easiest
        // way to find it: walk the leaf chain by using find_leftmost_leaf
        // and then hop one level up. Simpler: read the master, scan for
        // the "things" row, look up rootpage.
        let mut master = build_empty_master_table();
        load_table_rows(pager, &mut master, pager.header().schema_root_page).unwrap();
        let things_root = master
            .rowids()
            .into_iter()
            .find_map(|r| match master.get_value("name", r) {
                Some(Value::Text(s)) if s == "things" => match master.get_value("rootpage", r) {
                    Some(Value::Integer(p)) => Some(p as u32),
                    _ => None,
                },
                _ => None,
            })
            .expect("things should appear in sqlrite_master");
        let root_buf = pager.read_page(things_root).unwrap();
        assert_eq!(
            root_buf[0],
            PageType::InteriorNode as u8,
            "expected a multi-leaf table to have an interior root, got tag {}",
            root_buf[0]
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn deep_tree_round_trips() {
        // Force a 3-level tree by bypassing process_command (which prints
        // the full table on every INSERT, making large bulk loads O(N^2)
        // in I/O). We build the Table directly via restore_row.
        use crate::sql::db::table::Column as TableColumn;

        let path = tmp_path("deep_tree");
        let mut db = Database::new("deep".to_string());
        let columns = vec![
            TableColumn::new("id".into(), "integer".into(), true, true, true),
            TableColumn::new("s".into(), "text".into(), false, true, false),
        ];
        let mut table = build_empty_table("t", columns, 0);
        // ~900-byte rows → ~4 rows per leaf. 6000 rows → ~1500 leaves,
        // which with interior fanout ~400 needs 2 interior levels (3-level
        // tree total, counting leaves).
        for i in 1..=6_000i64 {
            let body = "q".repeat(900);
            table
                .restore_row(
                    i,
                    vec![Some(Value::Integer(i)), Some(Value::Text(format!("r-{i}-{body}")))],
                )
                .unwrap();
        }
        db.tables.insert("t".to_string(), table);
        save_database(&mut db, &path).unwrap();

        let loaded = open_database(&path, "deep".to_string()).unwrap();
        let t = loaded.get_table("t".to_string()).unwrap();
        assert_eq!(t.rowids().len(), 6_000);

        // Confirm the tree actually grew past 2 levels — i.e., the root's
        // leftmost child is itself an interior page, not a leaf.
        let pager = loaded.pager.as_ref().unwrap();
        let mut master = build_empty_master_table();
        load_table_rows(pager, &mut master, pager.header().schema_root_page).unwrap();
        let t_root = master
            .rowids()
            .into_iter()
            .find_map(|r| match master.get_value("name", r) {
                Some(Value::Text(s)) if s == "t" => match master.get_value("rootpage", r) {
                    Some(Value::Integer(p)) => Some(p as u32),
                    _ => None,
                },
                _ => None,
            })
            .expect("t in sqlrite_master");
        let root_buf = pager.read_page(t_root).unwrap();
        assert_eq!(root_buf[0], PageType::InteriorNode as u8);
        let root_payload: &[u8; PAYLOAD_PER_PAGE] =
            (&root_buf[PAGE_HEADER_SIZE..]).try_into().unwrap();
        let root_interior = InteriorPage::from_bytes(root_payload);
        let child = root_interior.leftmost_child().unwrap();
        let child_buf = pager.read_page(child).unwrap();
        assert_eq!(
            child_buf[0],
            PageType::InteriorNode as u8,
            "expected 3-level tree: root's leftmost child should also be InteriorNode",
        );

        let _ = std::fs::remove_file(&path);
    }
}
