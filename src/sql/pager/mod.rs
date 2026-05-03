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
#[allow(dead_code)]
pub mod fts_cell;
pub mod header;
#[allow(dead_code)]
pub mod hnsw_cell;
#[allow(dead_code)]
pub mod index_cell;
#[allow(dead_code)]
pub mod interior_page;
pub mod overflow;
pub mod page;
pub mod pager;
#[allow(dead_code)]
pub mod table_page;
#[allow(dead_code)]
pub mod varint;
#[allow(dead_code)]
pub mod wal;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};

use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;

use crate::error::{Result, SQLRiteError};
use crate::sql::db::database::Database;
use crate::sql::db::secondary_index::{IndexOrigin, SecondaryIndex};
use crate::sql::db::table::{Column, DataType, Row, Table, Value};
use crate::sql::pager::cell::Cell;
use crate::sql::pager::header::DbHeader;
use crate::sql::pager::index_cell::IndexCell;
use crate::sql::pager::interior_page::{InteriorCell, InteriorPage};
use crate::sql::pager::overflow::{
    OVERFLOW_THRESHOLD, OverflowRef, PagedEntry, read_overflow_chain, write_overflow_chain,
};
use crate::sql::pager::page::{PAGE_HEADER_SIZE, PAGE_SIZE, PAYLOAD_PER_PAGE, PageType};
use crate::sql::pager::pager::Pager;
use crate::sql::pager::table_page::TablePage;
use crate::sql::parser::create::CreateQuery;

// Re-export so callers can spell `sql::pager::AccessMode` without
// reaching into the `pager::pager::pager` submodule path.
pub use crate::sql::pager::pager::AccessMode;

/// Name of the internal catalog table. Reserved — user CREATEs of this
/// name must be rejected upstream.
pub const MASTER_TABLE_NAME: &str = "sqlrite_master";

/// Opens a database file in read-write mode. Shorthand for
/// [`open_database_with_mode`] with [`AccessMode::ReadWrite`].
pub fn open_database(path: &Path, db_name: String) -> Result<Database> {
    open_database_with_mode(path, db_name, AccessMode::ReadWrite)
}

/// Opens a database file in read-only mode. Acquires a shared OS-level
/// advisory lock, so other read-only openers coexist but any writer is
/// excluded. Attempts to mutate the returned `Database` (e.g. an
/// `INSERT`, or a `save_database` call against it) bottom out in a
/// `cannot commit: database is opened read-only` error from the Pager.
pub fn open_database_read_only(path: &Path, db_name: String) -> Result<Database> {
    open_database_with_mode(path, db_name, AccessMode::ReadOnly)
}

/// Opens a database file and reconstructs the in-memory `Database`,
/// leaving the long-lived `Pager` attached for subsequent auto-save
/// (read-write) or consistent-snapshot reads (read-only).
pub fn open_database_with_mode(path: &Path, db_name: String, mode: AccessMode) -> Result<Database> {
    let pager = Pager::open_with_mode(path, mode)?;

    // 1. Load sqlrite_master from the tree at header.schema_root_page.
    let mut master = build_empty_master_table();
    load_table_rows(&pager, &mut master, pager.header().schema_root_page)?;

    // 2. Two passes over master rows: first build every user table, then
    //    attach secondary indexes. Indexes need their base table to exist
    //    before we can populate them. Auto-indexes are created at table
    //    build time so we only have to load explicit indexes from disk
    //    (but we also reload the auto-index CONTENT because Table::new
    //    built it empty).
    let mut db = Database::new(db_name);
    let mut index_rows: Vec<IndexCatalogRow> = Vec::new();

    for rowid in master.rowids() {
        let ty = take_text(&master, "type", rowid)?;
        let name = take_text(&master, "name", rowid)?;
        let sql = take_text(&master, "sql", rowid)?;
        let rootpage = take_integer(&master, "rootpage", rowid)? as u32;
        let last_rowid = take_integer(&master, "last_rowid", rowid)?;

        match ty.as_str() {
            "table" => {
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
                if last_rowid > table.last_rowid {
                    table.last_rowid = last_rowid;
                }
                db.tables.insert(name, table);
            }
            "index" => {
                index_rows.push(IndexCatalogRow {
                    name,
                    sql,
                    rootpage,
                });
            }
            other => {
                return Err(SQLRiteError::Internal(format!(
                    "sqlrite_master row '{name}' has unknown type '{other}'"
                )));
            }
        }
    }

    // Second pass: attach each index to its table. HNSW indexes
    // (Phase 7d.2) take a different code path because their persisted
    // form is just the CREATE INDEX SQL — the graph itself isn't
    // persisted yet (Phase 7d.3). Detect HNSW via the SQL's USING clause
    // and route to a graph-rebuild instead of the B-Tree-cell load.
    //
    // Phase 8b — same shape for FTS indexes. The posting lists aren't
    // persisted yet (Phase 8c), so we replay the CREATE INDEX SQL on
    // open and let `execute_create_index` walk current rows.
    for row in index_rows {
        if create_index_sql_uses_hnsw(&row.sql) {
            rebuild_hnsw_index(&mut db, &pager, &row)?;
        } else if create_index_sql_uses_fts(&row.sql) {
            rebuild_fts_index(&mut db, &pager, &row)?;
        } else {
            attach_index(&mut db, &pager, row)?;
        }
    }

    db.source_path = Some(path.to_path_buf());
    db.pager = Some(pager);
    Ok(db)
}

/// Catalog row for a secondary index — deferred until after every table is
/// loaded so the index's base table exists by the time we populate it.
struct IndexCatalogRow {
    name: String,
    sql: String,
    rootpage: u32,
}

/// Persists `db` to disk. Same diff-commit behavior as before: only pages
/// whose bytes actually changed get written.
pub fn save_database(db: &mut Database, path: &Path) -> Result<()> {
    // Phase 7d.3 — rebuild any HNSW index that DELETE / UPDATE-on-vector
    // marked dirty. Done up front under the &mut Database borrow we
    // already hold, before the immutable iteration loops below need
    // their own borrow.
    rebuild_dirty_hnsw_indexes(db);
    // Phase 8b — same drill for FTS indexes flagged by DELETE / UPDATE.
    rebuild_dirty_fts_indexes(db);

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

    // 1. Stage each user table's B-Tree, collecting master-row info.
    //    `kind` is "table" or "index" — master has one row per each.
    let mut master_rows: Vec<CatalogEntry> = Vec::new();

    let mut table_names: Vec<&String> = db.tables.keys().collect();
    table_names.sort();
    for name in table_names {
        if name == MASTER_TABLE_NAME {
            return Err(SQLRiteError::Internal(format!(
                "user table cannot be named '{MASTER_TABLE_NAME}' (reserved)"
            )));
        }
        let table = &db.tables[name];
        let (rootpage, new_next) = stage_table_btree(&mut pager, table, next_free_page)?;
        next_free_page = new_next;
        master_rows.push(CatalogEntry {
            kind: "table".into(),
            name: name.clone(),
            sql: table_to_create_sql(table),
            rootpage,
            last_rowid: table.last_rowid,
        });
    }

    // 2. Stage each secondary index's B-Tree. Indexes persist in a
    //    deterministic order: sorted by (owning_table, index_name).
    let mut index_entries: Vec<(&Table, &SecondaryIndex)> = Vec::new();
    for table in db.tables.values() {
        for idx in &table.secondary_indexes {
            index_entries.push((table, idx));
        }
    }
    index_entries
        .sort_by(|(ta, ia), (tb, ib)| ta.tb_name.cmp(&tb.tb_name).then(ia.name.cmp(&ib.name)));
    for (_table, idx) in index_entries {
        let (rootpage, new_next) = stage_index_btree(&mut pager, idx, next_free_page)?;
        next_free_page = new_next;
        master_rows.push(CatalogEntry {
            kind: "index".into(),
            name: idx.name.clone(),
            sql: idx.synthesized_sql(),
            rootpage,
            last_rowid: 0,
        });
    }

    // 2b. Phase 7d.3: persist HNSW indexes as their own cell-encoded
    //     page trees, with the rootpage recorded in sqlrite_master.
    //     Reopen loads the graph back from cells (fast, exact match)
    //     instead of rebuilding from rows.
    //
    //     Dirty indexes (set by DELETE / UPDATE-on-vector-col) are
    //     rebuilt from current rows BEFORE staging, so the on-disk
    //     graph reflects the current row set.
    let mut hnsw_entries: Vec<(&Table, &crate::sql::db::table::HnswIndexEntry)> = Vec::new();
    for table in db.tables.values() {
        for entry in &table.hnsw_indexes {
            hnsw_entries.push((table, entry));
        }
    }
    hnsw_entries
        .sort_by(|(ta, ea), (tb, eb)| ta.tb_name.cmp(&tb.tb_name).then(ea.name.cmp(&eb.name)));
    for (table, entry) in hnsw_entries {
        let (rootpage, new_next) = stage_hnsw_btree(&mut pager, &entry.index, next_free_page)?;
        next_free_page = new_next;
        master_rows.push(CatalogEntry {
            kind: "index".into(),
            name: entry.name.clone(),
            sql: format!(
                "CREATE INDEX {} ON {} USING hnsw ({})",
                entry.name, table.tb_name, entry.column_name
            ),
            rootpage,
            last_rowid: 0,
        });
    }

    // 2c. Phase 8c — persist FTS posting lists as their own
    //     cell-encoded page trees, with the rootpage recorded in
    //     sqlrite_master. Reopen loads the postings back from cells
    //     (fast, exact match) instead of re-tokenizing rows.
    //
    //     Dirty indexes (set by DELETE / UPDATE-on-text-col) are
    //     rebuilt from current rows BEFORE staging by
    //     `rebuild_dirty_fts_indexes`, so the on-disk tree reflects
    //     the current row set.
    let mut fts_entries: Vec<(&Table, &crate::sql::db::table::FtsIndexEntry)> = Vec::new();
    for table in db.tables.values() {
        for entry in &table.fts_indexes {
            fts_entries.push((table, entry));
        }
    }
    fts_entries
        .sort_by(|(ta, ea), (tb, eb)| ta.tb_name.cmp(&tb.tb_name).then(ea.name.cmp(&eb.name)));
    let any_fts = !fts_entries.is_empty();
    for (table, entry) in fts_entries {
        let (rootpage, new_next) = stage_fts_btree(&mut pager, &entry.index, next_free_page)?;
        next_free_page = new_next;
        master_rows.push(CatalogEntry {
            kind: "index".into(),
            name: entry.name.clone(),
            sql: format!(
                "CREATE INDEX {} ON {} USING fts ({})",
                entry.name, table.tb_name, entry.column_name
            ),
            rootpage,
            last_rowid: 0,
        });
    }

    // 3. Build an in-memory sqlrite_master with one row per table or index,
    //    then stage it via the same tree-build path.
    let mut master = build_empty_master_table();
    for (i, entry) in master_rows.into_iter().enumerate() {
        let rowid = (i as i64) + 1;
        master.restore_row(
            rowid,
            vec![
                Some(Value::Text(entry.kind)),
                Some(Value::Text(entry.name)),
                Some(Value::Text(entry.sql)),
                Some(Value::Integer(entry.rootpage as i64)),
                Some(Value::Integer(entry.last_rowid)),
            ],
        )?;
    }
    let (master_root, master_next) = stage_table_btree(&mut pager, &master, next_free_page)?;
    next_free_page = master_next;

    // Phase 8c — on-demand v4→v5 file-format bump per Q10. If any FTS
    // index attached to the database, write v5; otherwise preserve the
    // pre-existing version (v4 for files born before this build, or
    // a previously-promoted v5 file). Reads accept both.
    let format_version = if any_fts {
        crate::sql::pager::header::FORMAT_VERSION_V5
    } else {
        pager.header().format_version
    };

    pager.commit(DbHeader {
        page_count: next_free_page,
        schema_root_page: master_root,
        format_version,
    })?;

    if same_path {
        db.pager = Some(pager);
    }
    Ok(())
}

/// Build material for a single row in sqlrite_master.
struct CatalogEntry {
    kind: String, // "table" or "index"
    name: String,
    sql: String,
    rootpage: u32,
    last_rowid: i64,
}

// -------------------------------------------------------------------------
// sqlrite_master — hardcoded catalog table schema

fn build_empty_master_table() -> Table {
    // Phase 3e: `type` is the first column, matching SQLite's convention.
    // It distinguishes `'table'` rows from `'index'` rows.
    let columns = vec![
        Column::new("type".into(), "text".into(), false, true, false),
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
        // Render the SQL type literally so the round-trip through
        // CREATE TABLE re-parsing recreates the same schema. Vector
        // carries its dimension inline.
        let ty: String = match &c.datatype {
            DataType::Integer => "INTEGER".to_string(),
            DataType::Text => "TEXT".to_string(),
            DataType::Real => "REAL".to_string(),
            DataType::Bool => "BOOLEAN".to_string(),
            DataType::Vector(dim) => format!("VECTOR({dim})"),
            DataType::Json => "JSON".to_string(),
            DataType::None | DataType::Invalid => "TEXT".to_string(),
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
    let rows: Arc<Mutex<HashMap<String, Row>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut secondary_indexes: Vec<SecondaryIndex> = Vec::new();
    {
        let mut map = rows.lock().expect("rows mutex poisoned");
        for col in &columns {
            // Mirror the dispatch in `Table::new` so the reconstructed
            // table has the same shape it'd have if it were built fresh
            // from SQL. Phase 7a adds the Vector arm — without it,
            // VECTOR columns silently restore as Row::None and every
            // restore_row hits a "storage None vs value Some(Vector(...))"
            // type mismatch.
            let row = match &col.datatype {
                DataType::Integer => Row::Integer(BTreeMap::new()),
                DataType::Text => Row::Text(BTreeMap::new()),
                DataType::Real => Row::Real(BTreeMap::new()),
                DataType::Bool => Row::Bool(BTreeMap::new()),
                DataType::Vector(_dim) => Row::Vector(BTreeMap::new()),
                // JSON columns reuse Text storage — see Table::new and
                // Phase 7e's scope-correction note.
                DataType::Json => Row::Text(BTreeMap::new()),
                DataType::None | DataType::Invalid => Row::None,
            };
            map.insert(col.column_name.clone(), row);

            // Auto-create UNIQUE/PK indexes so the restored table has the
            // same shape Table::new would have built from fresh SQL.
            if (col.is_pk || col.is_unique)
                && matches!(col.datatype, DataType::Integer | DataType::Text)
            {
                if let Ok(idx) = SecondaryIndex::new(
                    SecondaryIndex::auto_name(name, &col.column_name),
                    name.to_string(),
                    col.column_name.clone(),
                    &col.datatype,
                    true,
                    IndexOrigin::Auto,
                ) {
                    secondary_indexes.push(idx);
                }
            }
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
        secondary_indexes,
        // HNSW indexes (Phase 7d.2) are reconstructed on open by re-
        // executing each `CREATE INDEX … USING hnsw` SQL stored in
        // `sqlrite_master`. This builder produces the empty shell;
        // `replay_create_index_for_hnsw` (in this same module) walks
        // sqlrite_master after every table is loaded and rebuilds the
        // graph from current row data. Persistence of the graph itself
        // (avoiding the on-open rebuild cost) is Phase 7d.3.
        hnsw_indexes: Vec::new(),
        // FTS indexes (Phase 8b) follow the same pattern — the
        // CREATE INDEX … USING fts SQL is the source of truth on open
        // and the in-memory posting list gets rebuilt from current
        // rows. Cell-encoded persistence of the postings is Phase 8c.
        fts_indexes: Vec::new(),
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
/// Re-parses `CREATE INDEX` SQL from sqlrite_master and restores the
/// index on its base table by walking the tree of index cells at
/// `rootpage`. The base table is expected to already be in `db.tables`.
fn attach_index(db: &mut Database, pager: &Pager, row: IndexCatalogRow) -> Result<()> {
    let (table_name, column_name, is_unique) = parse_create_index_sql(&row.sql)?;

    let table = db.get_table_mut(table_name.clone()).map_err(|_| {
        SQLRiteError::Internal(format!(
            "index '{}' references unknown table '{table_name}' (sqlrite_master out of sync?)",
            row.name
        ))
    })?;
    let datatype = table
        .columns
        .iter()
        .find(|c| c.column_name == column_name)
        .map(|c| clone_datatype(&c.datatype))
        .ok_or_else(|| {
            SQLRiteError::Internal(format!(
                "index '{}' references unknown column '{column_name}' on '{table_name}'",
                row.name
            ))
        })?;

    // An auto-index on this column may already exist (built by
    // build_empty_table for UNIQUE/PK columns). If the names match, reuse
    // the slot instead of adding a duplicate entry.
    let existing_slot = table
        .secondary_indexes
        .iter()
        .position(|i| i.name == row.name);
    let idx = match existing_slot {
        Some(i) => {
            // Drain any entries that may have been populated during table
            // restore_row calls — we're about to repopulate from the
            // persisted tree.
            table.secondary_indexes.remove(i)
        }
        None => SecondaryIndex::new(
            row.name.clone(),
            table_name.clone(),
            column_name.clone(),
            &datatype,
            is_unique,
            IndexOrigin::Explicit,
        )?,
    };
    let mut idx = idx;
    // Wipe any stale entries from the auto path so the load is idempotent.
    let is_unique_flag = idx.is_unique;
    let origin = idx.origin;
    idx = SecondaryIndex::new(
        idx.name,
        idx.table_name,
        idx.column_name,
        &datatype,
        is_unique_flag,
        origin,
    )?;

    // Populate from the index tree's cells.
    load_index_rows(pager, &mut idx, row.rootpage)?;

    table.secondary_indexes.push(idx);
    Ok(())
}

/// Walks the leaves of an index B-Tree rooted at `root_page` and inserts
/// every `(value, rowid)` pair into `idx`.
fn load_index_rows(pager: &Pager, idx: &mut SecondaryIndex, root_page: u32) -> Result<()> {
    if root_page == 0 {
        return Ok(());
    }
    let first_leaf = find_leftmost_leaf(pager, root_page)?;
    let mut current = first_leaf;
    while current != 0 {
        let page_buf = pager
            .read_page(current)
            .ok_or_else(|| SQLRiteError::Internal(format!("missing index leaf page {current}")))?;
        if page_buf[0] != PageType::TableLeaf as u8 {
            return Err(SQLRiteError::Internal(format!(
                "page {current} tagged {} but expected TableLeaf (index)",
                page_buf[0]
            )));
        }
        let next_leaf = u32::from_le_bytes(page_buf[1..5].try_into().unwrap());
        let payload: &[u8; PAYLOAD_PER_PAGE] = (&page_buf[PAGE_HEADER_SIZE..])
            .try_into()
            .map_err(|_| SQLRiteError::Internal("index leaf payload size".to_string()))?;
        let leaf = TablePage::from_bytes(payload);

        for slot in 0..leaf.slot_count() {
            // Slots on an index page hold KIND_INDEX cells; decode directly.
            let offset = leaf.slot_offset_raw(slot)?;
            let (ic, _) = IndexCell::decode(leaf.as_bytes(), offset)?;
            idx.insert(&ic.value, ic.rowid)?;
        }
        current = next_leaf;
    }
    Ok(())
}

/// Minimal recognizer for the synthesized-or-user `CREATE INDEX` SQL we
/// store in sqlrite_master. Returns `(table_name, column_name, is_unique)`.
///
/// Uses sqlparser so user-supplied SQL with extra whitespace, case, etc.
/// still works; the only shape we accept is single-column indexes.
fn parse_create_index_sql(sql: &str) -> Result<(String, String, bool)> {
    use sqlparser::ast::{CreateIndex, Expr, Statement};

    let dialect = SQLiteDialect {};
    let mut ast = Parser::parse_sql(&dialect, sql).map_err(SQLRiteError::from)?;
    let Some(Statement::CreateIndex(CreateIndex {
        table_name,
        columns,
        unique,
        ..
    })) = ast.pop()
    else {
        return Err(SQLRiteError::Internal(format!(
            "sqlrite_master index row's SQL isn't a CREATE INDEX: {sql}"
        )));
    };
    if columns.len() != 1 {
        return Err(SQLRiteError::NotImplemented(
            "multi-column indexes aren't supported yet".to_string(),
        ));
    }
    let col = match &columns[0].column.expr {
        Expr::Identifier(ident) => ident.value.clone(),
        Expr::CompoundIdentifier(parts) => {
            parts.last().map(|p| p.value.clone()).unwrap_or_default()
        }
        other => {
            return Err(SQLRiteError::Internal(format!(
                "unsupported indexed column expression: {other:?}"
            )));
        }
    };
    Ok((table_name.to_string(), col, unique))
}

/// True iff a CREATE INDEX SQL string uses `USING hnsw` (case-insensitive).
/// Used by the open path to route HNSW indexes to the graph-rebuild path
/// instead of the standard B-Tree cell-load. Pre-Phase-7d.2 indexes
/// don't have a USING clause, so they all return false and continue
/// taking the existing path.
fn create_index_sql_uses_hnsw(sql: &str) -> bool {
    use sqlparser::ast::{CreateIndex, IndexType, Statement};

    let dialect = SQLiteDialect {};
    let Ok(mut ast) = Parser::parse_sql(&dialect, sql) else {
        return false;
    };
    let Some(Statement::CreateIndex(CreateIndex { using, .. })) = ast.pop() else {
        return false;
    };
    matches!(using, Some(IndexType::Custom(ident)) if ident.value.eq_ignore_ascii_case("hnsw"))
}

/// Phase 8b — peeks at a CREATE INDEX SQL to detect `USING fts(...)`.
/// Mirrors [`create_index_sql_uses_hnsw`].
fn create_index_sql_uses_fts(sql: &str) -> bool {
    use sqlparser::ast::{CreateIndex, IndexType, Statement};

    let dialect = SQLiteDialect {};
    let Ok(mut ast) = Parser::parse_sql(&dialect, sql) else {
        return false;
    };
    let Some(Statement::CreateIndex(CreateIndex { using, .. })) = ast.pop() else {
        return false;
    };
    matches!(using, Some(IndexType::Custom(ident)) if ident.value.eq_ignore_ascii_case("fts"))
}

/// Phase 8c — loads (or rebuilds) an FTS index on database open. Two
/// paths mirror [`rebuild_hnsw_index`]:
///
///   - **rootpage != 0** (Phase 8c default): the posting list is
///     persisted as cell-encoded pages. Read every cell directly via
///     [`load_fts_postings`] and reconstruct the index — no
///     re-tokenization, exact bit-for-bit reproduction.
///
///   - **rootpage == 0** (compatibility): no on-disk postings, e.g.
///     for files saved by Phase 8b before persistence landed. Replay
///     the CREATE INDEX SQL through `execute_create_index`, which
///     walks the table's current rows and tokenizes them fresh.
fn rebuild_fts_index(db: &mut Database, pager: &Pager, row: &IndexCatalogRow) -> Result<()> {
    use crate::sql::db::table::FtsIndexEntry;
    use crate::sql::executor::execute_create_index;
    use crate::sql::fts::PostingList;
    use sqlparser::ast::Statement;

    let dialect = SQLiteDialect {};
    let mut ast = Parser::parse_sql(&dialect, &row.sql).map_err(SQLRiteError::from)?;
    let Some(stmt @ Statement::CreateIndex(_)) = ast.pop() else {
        return Err(SQLRiteError::Internal(format!(
            "sqlrite_master FTS row's SQL isn't a CREATE INDEX: {}",
            row.sql
        )));
    };

    if row.rootpage == 0 {
        // Compatibility path — no persisted postings; replay rows.
        execute_create_index(&stmt, db)?;
        return Ok(());
    }

    let (doc_lengths, postings) = load_fts_postings(pager, row.rootpage)?;
    let index = PostingList::from_persisted_postings(doc_lengths, postings);
    let (tbl_name, col_name) = parse_fts_create_index_sql(&row.sql)?;
    let table_mut = db.get_table_mut(tbl_name.clone()).map_err(|_| {
        SQLRiteError::Internal(format!(
            "FTS index '{}' references unknown table '{tbl_name}'",
            row.name
        ))
    })?;
    table_mut.fts_indexes.push(FtsIndexEntry {
        name: row.name.clone(),
        column_name: col_name,
        index,
        needs_rebuild: false,
    });
    Ok(())
}

/// Pulls (table_name, column_name) out of a `CREATE INDEX … USING fts(col)`
/// SQL string. Same shape as `parse_hnsw_create_index_sql`.
fn parse_fts_create_index_sql(sql: &str) -> Result<(String, String)> {
    use sqlparser::ast::{CreateIndex, Expr, Statement};

    let dialect = SQLiteDialect {};
    let mut ast = Parser::parse_sql(&dialect, sql).map_err(SQLRiteError::from)?;
    let Some(Statement::CreateIndex(CreateIndex {
        table_name,
        columns,
        ..
    })) = ast.pop()
    else {
        return Err(SQLRiteError::Internal(format!(
            "sqlrite_master FTS row's SQL isn't a CREATE INDEX: {sql}"
        )));
    };
    if columns.len() != 1 {
        return Err(SQLRiteError::NotImplemented(
            "multi-column FTS indexes aren't supported yet".to_string(),
        ));
    }
    let col = match &columns[0].column.expr {
        Expr::Identifier(ident) => ident.value.clone(),
        Expr::CompoundIdentifier(parts) => {
            parts.last().map(|p| p.value.clone()).unwrap_or_default()
        }
        other => {
            return Err(SQLRiteError::Internal(format!(
                "FTS CREATE INDEX has unexpected column expr: {other:?}"
            )));
        }
    };
    Ok((table_name.to_string(), col))
}

/// Loads (or rebuilds) an HNSW index on database open. Two paths:
///
///   - **rootpage != 0** (Phase 7d.3 default): the graph is persisted
///     as cell-encoded pages. Read every node directly via
///     `load_hnsw_nodes` and reconstruct the index — fast, zero
///     algorithm runs, exact bit-for-bit reproduction of what was saved.
///
///   - **rootpage == 0** (compatibility): no on-disk graph, e.g. for
///     files saved by Phase 7d.2 before persistence landed. Replay the
///     CREATE INDEX SQL through `execute_create_index`, which walks the
///     table's current rows and populates a fresh graph. Slower but
///     correctness-equivalent on the first save with the new code.
fn rebuild_hnsw_index(db: &mut Database, pager: &Pager, row: &IndexCatalogRow) -> Result<()> {
    use crate::sql::db::table::HnswIndexEntry;
    use crate::sql::executor::execute_create_index;
    use crate::sql::hnsw::{DistanceMetric, HnswIndex};
    use sqlparser::ast::Statement;

    let dialect = SQLiteDialect {};
    let mut ast = Parser::parse_sql(&dialect, &row.sql).map_err(SQLRiteError::from)?;
    let Some(stmt @ Statement::CreateIndex(_)) = ast.pop() else {
        return Err(SQLRiteError::Internal(format!(
            "sqlrite_master HNSW row's SQL isn't a CREATE INDEX: {}",
            row.sql
        )));
    };

    if row.rootpage == 0 {
        // Compatibility path — no persisted graph; walk current rows.
        execute_create_index(&stmt, db)?;
        return Ok(());
    }

    // Persistence path — read the cell tree, deserialize.
    let nodes = load_hnsw_nodes(pager, row.rootpage)?;
    let index = HnswIndex::from_persisted_nodes(DistanceMetric::L2, 0xC0FFEE, nodes);

    // Parse the CREATE INDEX to know which table + column to attach to
    // — same shape as the row-walk path; we just don't execute it.
    let (tbl_name, col_name) = parse_hnsw_create_index_sql(&row.sql)?;
    let table_mut = db.get_table_mut(tbl_name.clone()).map_err(|_| {
        SQLRiteError::Internal(format!(
            "HNSW index '{}' references unknown table '{tbl_name}'",
            row.name
        ))
    })?;
    table_mut.hnsw_indexes.push(HnswIndexEntry {
        name: row.name.clone(),
        column_name: col_name,
        index,
        needs_rebuild: false,
    });
    Ok(())
}

/// Phase 7d.3 — Phase-7d.3-side helper: walk every leaf in the HNSW
/// page tree at `root_page` and decode each cell as a node. Returns
/// the (node_id, layers) tuples in slot-order (already ascending by
/// node_id since they were staged that way). The caller hands them to
/// `HnswIndex::from_persisted_nodes`.
fn load_hnsw_nodes(pager: &Pager, root_page: u32) -> Result<Vec<(i64, Vec<Vec<i64>>)>> {
    use crate::sql::pager::hnsw_cell::HnswNodeCell;

    let mut nodes: Vec<(i64, Vec<Vec<i64>>)> = Vec::new();
    let first_leaf = find_leftmost_leaf(pager, root_page)?;
    let mut current = first_leaf;
    while current != 0 {
        let page_buf = pager
            .read_page(current)
            .ok_or_else(|| SQLRiteError::Internal(format!("missing HNSW leaf page {current}")))?;
        if page_buf[0] != PageType::TableLeaf as u8 {
            return Err(SQLRiteError::Internal(format!(
                "page {current} tagged {} but expected TableLeaf (HNSW)",
                page_buf[0]
            )));
        }
        let next_leaf = u32::from_le_bytes(page_buf[1..5].try_into().unwrap());
        let payload: &[u8; PAYLOAD_PER_PAGE] = (&page_buf[PAGE_HEADER_SIZE..])
            .try_into()
            .map_err(|_| SQLRiteError::Internal("HNSW leaf payload size".to_string()))?;
        let leaf = TablePage::from_bytes(payload);
        for slot in 0..leaf.slot_count() {
            let offset = leaf.slot_offset_raw(slot)?;
            let (cell, _) = HnswNodeCell::decode(leaf.as_bytes(), offset)?;
            nodes.push((cell.node_id, cell.layers));
        }
        current = next_leaf;
    }
    Ok(nodes)
}

/// Pulls (table_name, column_name) out of a `CREATE INDEX … USING hnsw (col)`
/// SQL string. Used by the persistence path on open to know where to
/// attach the loaded graph. Same shape as `parse_create_index_sql` for
/// regular indexes — only the assertion differs (we don't care about
/// UNIQUE for HNSW).
fn parse_hnsw_create_index_sql(sql: &str) -> Result<(String, String)> {
    use sqlparser::ast::{CreateIndex, Expr, Statement};

    let dialect = SQLiteDialect {};
    let mut ast = Parser::parse_sql(&dialect, sql).map_err(SQLRiteError::from)?;
    let Some(Statement::CreateIndex(CreateIndex {
        table_name,
        columns,
        ..
    })) = ast.pop()
    else {
        return Err(SQLRiteError::Internal(format!(
            "sqlrite_master HNSW row's SQL isn't a CREATE INDEX: {sql}"
        )));
    };
    if columns.len() != 1 {
        return Err(SQLRiteError::NotImplemented(
            "multi-column HNSW indexes aren't supported yet".to_string(),
        ));
    }
    let col = match &columns[0].column.expr {
        Expr::Identifier(ident) => ident.value.clone(),
        Expr::CompoundIdentifier(parts) => {
            parts.last().map(|p| p.value.clone()).unwrap_or_default()
        }
        other => {
            return Err(SQLRiteError::Internal(format!(
                "unsupported HNSW indexed column expression: {other:?}"
            )));
        }
    };
    Ok((table_name.to_string(), col))
}

/// Phase 7d.3 — rebuilds in-place any HnswIndexEntry whose
/// `needs_rebuild` flag is set (DELETE / UPDATE-on-vector marked it).
/// Walks the table's current Vec<f32> column storage and runs the
/// HNSW algorithm fresh. Called at the top of `save_database` before
/// any immutable borrows of `db` start.
///
/// Cost: O(N · ef_construction · log N) per dirty index. Fine for
/// small tables, expensive for ≥100k-row tables — matches the
/// trade-off SQLite makes for FTS5: dirtying-and-rebuilding is the
/// MVP, more sophisticated incremental delete strategies (soft-delete
/// + tombstones, neighbor reconnection) are future polish.
fn rebuild_dirty_hnsw_indexes(db: &mut Database) {
    use crate::sql::hnsw::{DistanceMetric, HnswIndex};

    for table in db.tables.values_mut() {
        // Snapshot which (index_name, column) pairs need rebuilding,
        // before we go grabbing column data — keeps the borrow
        // structure simple.
        let dirty: Vec<(String, String)> = table
            .hnsw_indexes
            .iter()
            .filter(|e| e.needs_rebuild)
            .map(|e| (e.name.clone(), e.column_name.clone()))
            .collect();
        if dirty.is_empty() {
            continue;
        }

        for (idx_name, col_name) in dirty {
            // Snapshot every (rowid, vec) for this column.
            let mut vectors: Vec<(i64, Vec<f32>)> = Vec::new();
            {
                let row_data = table.rows.lock().expect("rows mutex poisoned");
                if let Some(Row::Vector(map)) = row_data.get(&col_name) {
                    for (id, v) in map.iter() {
                        vectors.push((*id, v.clone()));
                    }
                }
            }
            // Pre-build a HashMap for the get_vec closure so we don't
            // pay O(N) lookup per insert call.
            let snapshot: std::collections::HashMap<i64, Vec<f32>> =
                vectors.iter().cloned().collect();

            let mut new_idx = HnswIndex::new(DistanceMetric::L2, 0xC0FFEE);
            // Sort by id so the rebuild is deterministic across runs.
            vectors.sort_by_key(|(id, _)| *id);
            for (id, v) in &vectors {
                new_idx.insert(*id, v, |q| snapshot.get(&q).cloned().unwrap_or_default());
            }

            // Replace the entry's index + clear the dirty flag.
            if let Some(entry) = table.hnsw_indexes.iter_mut().find(|e| e.name == idx_name) {
                entry.index = new_idx;
                entry.needs_rebuild = false;
            }
        }
    }
}

/// Phase 8b — rebuild every FTS index a DELETE / UPDATE-on-text-col
/// marked dirty. Mirrors [`rebuild_dirty_hnsw_indexes`]; runs at save
/// time under `&mut Database`. Cheap on a clean DB (the `dirty` snapshot
/// is empty so the per-table loop short-circuits).
fn rebuild_dirty_fts_indexes(db: &mut Database) {
    use crate::sql::fts::PostingList;

    for table in db.tables.values_mut() {
        let dirty: Vec<(String, String)> = table
            .fts_indexes
            .iter()
            .filter(|e| e.needs_rebuild)
            .map(|e| (e.name.clone(), e.column_name.clone()))
            .collect();
        if dirty.is_empty() {
            continue;
        }

        for (idx_name, col_name) in dirty {
            // Snapshot every (rowid, text) pair for this column under
            // the row mutex, then drop the lock before re-tokenizing.
            let mut docs: Vec<(i64, String)> = Vec::new();
            {
                let row_data = table.rows.lock().expect("rows mutex poisoned");
                if let Some(Row::Text(map)) = row_data.get(&col_name) {
                    for (id, v) in map.iter() {
                        // "Null" sentinel is the parser's
                        // null-marker for TEXT cells; skip those —
                        // they'd round-trip as the literal string
                        // "Null" otherwise. Aligns with insert_row's
                        // typed_value gate.
                        if v != "Null" {
                            docs.push((*id, v.clone()));
                        }
                    }
                }
            }

            let mut new_idx = PostingList::new();
            // Sort by id so the rebuild is deterministic across runs
            // (the BTreeMap inside PostingList is order-stable, but
            // doc-length aggregation order doesn't matter — sorting
            // here is purely for reproducibility on inspection).
            docs.sort_by_key(|(id, _)| *id);
            for (id, text) in &docs {
                new_idx.insert(*id, text);
            }

            if let Some(entry) = table.fts_indexes.iter_mut().find(|e| e.name == idx_name) {
                entry.index = new_idx;
                entry.needs_rebuild = false;
            }
        }
    }
}

/// Cheap clone helper — `DataType` doesn't derive `Clone` elsewhere.
fn clone_datatype(dt: &DataType) -> DataType {
    match dt {
        DataType::Integer => DataType::Integer,
        DataType::Text => DataType::Text,
        DataType::Real => DataType::Real,
        DataType::Bool => DataType::Bool,
        DataType::Vector(dim) => DataType::Vector(*dim),
        DataType::Json => DataType::Json,
        DataType::None => DataType::None,
        DataType::Invalid => DataType::Invalid,
    }
}

/// Stages an index's B-Tree at `start_page`. Each leaf cell is a
/// `KIND_INDEX` entry carrying `(original_rowid, value)`. Returns
/// `(root_page, next_free_page)`.
///
/// The tree's shape matches a regular table's — leaves chained via
/// `next_page`, optional interior layer above. `Cell::peek_rowid` works
/// uniformly for index cells (same prefix as local cells), so the
/// existing slot directory and binary search carry over.
fn stage_index_btree(
    pager: &mut Pager,
    idx: &SecondaryIndex,
    start_page: u32,
) -> Result<(u32, u32)> {
    // Build the leaves.
    let (leaves, mut next_free_page) = stage_index_leaves(pager, idx, start_page)?;
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

/// Packs the index's (value, rowid) entries into a sibling-chained run
/// of `TableLeaf` pages. Iteration order matches `SecondaryIndex::iter_entries`
/// (ascending value; rowids in insertion order within a value), which is
/// also ascending by the "cell rowid" carried in each IndexCell (the
/// original row's rowid) — so Cell::peek_rowid + the slot directory's
/// rowid ordering stays consistent.
fn stage_index_leaves(
    pager: &mut Pager,
    idx: &SecondaryIndex,
    start_page: u32,
) -> Result<(Vec<(u32, i64)>, u32)> {
    let mut leaves: Vec<(u32, i64)> = Vec::new();
    let mut current_leaf = TablePage::empty();
    let mut current_leaf_page = start_page;
    let mut current_max_rowid: Option<i64> = None;
    let mut next_free_page = start_page + 1;

    // Sort the entries by original rowid so the in-page slot directory,
    // which binary-searches by rowid, stays valid. (iter_entries orders by
    // value; we reorder here for B-Tree correctness.)
    let mut entries: Vec<(Value, i64)> = idx.iter_entries().collect();
    entries.sort_by_key(|(_, r)| *r);

    for (value, rowid) in entries {
        let cell = IndexCell::new(rowid, value);
        let entry_bytes = cell.encode()?;

        if !current_leaf.would_fit(entry_bytes.len()) {
            let next_leaf_page_num = next_free_page;
            emit_leaf(pager, current_leaf_page, &current_leaf, next_leaf_page_num);
            leaves.push((current_leaf_page, current_max_rowid.unwrap_or(i64::MIN)));
            current_leaf = TablePage::empty();
            current_leaf_page = next_leaf_page_num;
            next_free_page += 1;

            if !current_leaf.would_fit(entry_bytes.len()) {
                return Err(SQLRiteError::Internal(format!(
                    "index entry of {} bytes exceeds empty-page capacity {}",
                    entry_bytes.len(),
                    current_leaf.free_space()
                )));
            }
        }
        current_leaf.insert_entry(rowid, &entry_bytes)?;
        current_max_rowid = Some(rowid);
    }

    emit_leaf(pager, current_leaf_page, &current_leaf, 0);
    leaves.push((current_leaf_page, current_max_rowid.unwrap_or(i64::MIN)));
    Ok((leaves, next_free_page))
}

/// Phase 7d.3 — stages an HNSW index's page tree at `start_page`.
/// Each leaf cell is a `KIND_HNSW` entry carrying one node's
/// (node_id, layers). Returns `(root_page, next_free_page)`.
///
/// Tree shape is identical to `stage_index_btree` — chained leaves +
/// optional interior layers. The slot directory binary-searches by
/// node_id (which is the cell's "rowid" in `Cell::peek_rowid` terms),
/// so reads can locate any node in O(log N) once 7d.4-or-later
/// optimizes the load path to lazy-fetch instead of read-all.
/// Today, `load_hnsw_nodes` reads the entire tree on open.
fn stage_hnsw_btree(
    pager: &mut Pager,
    idx: &crate::sql::hnsw::HnswIndex,
    start_page: u32,
) -> Result<(u32, u32)> {
    let (leaves, mut next_free_page) = stage_hnsw_leaves(pager, idx, start_page)?;
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

/// Phase 8c — stage one FTS index as a `TableLeaf`-shaped B-Tree.
/// Mirrors `stage_hnsw_btree` (sibling-chained leaves, optional interior
/// levels). Returns `(root_page, next_free_page)`. Each leaf is filled
/// with `KIND_FTS_POSTING` cells: one sidecar cell holding the
/// doc-lengths map, then one cell per term in lexicographic order.
fn stage_fts_btree(
    pager: &mut Pager,
    idx: &crate::sql::fts::PostingList,
    start_page: u32,
) -> Result<(u32, u32)> {
    let (leaves, mut next_free_page) = stage_fts_leaves(pager, idx, start_page)?;
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

/// Packs FTS posting cells into a sibling-chained run of `TableLeaf`
/// pages. Cell layout: a single doc-lengths sidecar at `cell_id = 1`,
/// followed by one cell per term in lexicographic order with
/// `cell_id = 2..=N + 1`. Sequential ids keep the slot directory's
/// rowid ordering valid (the `cell_id` field is what `peek_rowid`
/// returns).
fn stage_fts_leaves(
    pager: &mut Pager,
    idx: &crate::sql::fts::PostingList,
    start_page: u32,
) -> Result<(Vec<(u32, i64)>, u32)> {
    use crate::sql::pager::fts_cell::FtsPostingCell;

    let mut leaves: Vec<(u32, i64)> = Vec::new();
    let mut current_leaf = TablePage::empty();
    let mut current_leaf_page = start_page;
    let mut current_max_rowid: Option<i64> = None;
    let mut next_free_page = start_page + 1;

    // Build the cell sequence: sidecar first, then per-term cells. The
    // sidecar always exists (even on an empty index) so reload sees a
    // canonical "this index was persisted" marker in slot 0.
    let mut cell_id: i64 = 1;
    let mut cells: Vec<FtsPostingCell> = Vec::new();
    cells.push(FtsPostingCell::doc_lengths(
        cell_id,
        idx.serialize_doc_lengths(),
    ));
    for (term, entries) in idx.serialize_postings() {
        cell_id += 1;
        cells.push(FtsPostingCell::posting(cell_id, term, entries));
    }

    for cell in cells {
        let entry_bytes = cell.encode()?;

        if !current_leaf.would_fit(entry_bytes.len()) {
            let next_leaf_page_num = next_free_page;
            emit_leaf(pager, current_leaf_page, &current_leaf, next_leaf_page_num);
            leaves.push((current_leaf_page, current_max_rowid.unwrap_or(i64::MIN)));
            current_leaf = TablePage::empty();
            current_leaf_page = next_leaf_page_num;
            next_free_page += 1;

            if !current_leaf.would_fit(entry_bytes.len()) {
                // A single posting cell exceeds page capacity. Phase
                // 8c MVP doesn't chain via overflow cells (the plan
                // notes this as a stretch goal); surface a clear
                // error so users know which term tripped it.
                return Err(SQLRiteError::Internal(format!(
                    "FTS posting cell {} of {} bytes exceeds empty-page capacity {} \
                     (term too long or too many postings; overflow chaining is Phase 8.1)",
                    cell.cell_id,
                    entry_bytes.len(),
                    current_leaf.free_space()
                )));
            }
        }
        current_leaf.insert_entry(cell.cell_id, &entry_bytes)?;
        current_max_rowid = Some(cell.cell_id);
    }

    emit_leaf(pager, current_leaf_page, &current_leaf, 0);
    leaves.push((current_leaf_page, current_max_rowid.unwrap_or(i64::MIN)));
    Ok((leaves, next_free_page))
}

/// (rowid, value) pairs as decoded from a single FTS cell — value is
/// either term frequency (posting cell) or doc length (sidecar cell).
type FtsEntries = Vec<(i64, u32)>;
/// (term, posting list) pairs as decoded from non-sidecar FTS cells.
type FtsPostings = Vec<(String, FtsEntries)>;

/// Phase 8c — read every cell of an FTS index from `root_page` back
/// into the `(doc_lengths, postings)` shape `PostingList::from_persisted_postings`
/// expects. Mirrors `load_hnsw_nodes`: leftmost-leaf descent, walk the
/// sibling chain, decode each slot.
fn load_fts_postings(pager: &Pager, root_page: u32) -> Result<(FtsEntries, FtsPostings)> {
    use crate::sql::pager::fts_cell::FtsPostingCell;

    let mut doc_lengths: Vec<(i64, u32)> = Vec::new();
    let mut postings: Vec<(String, Vec<(i64, u32)>)> = Vec::new();
    let mut saw_sidecar = false;

    let first_leaf = find_leftmost_leaf(pager, root_page)?;
    let mut current = first_leaf;
    while current != 0 {
        let page_buf = pager
            .read_page(current)
            .ok_or_else(|| SQLRiteError::Internal(format!("missing FTS leaf page {current}")))?;
        if page_buf[0] != PageType::TableLeaf as u8 {
            return Err(SQLRiteError::Internal(format!(
                "page {current} tagged {} but expected TableLeaf (FTS)",
                page_buf[0]
            )));
        }
        let next_leaf = u32::from_le_bytes(page_buf[1..5].try_into().unwrap());
        let payload: &[u8; PAYLOAD_PER_PAGE] = (&page_buf[PAGE_HEADER_SIZE..])
            .try_into()
            .map_err(|_| SQLRiteError::Internal("FTS leaf payload size".to_string()))?;
        let leaf = TablePage::from_bytes(payload);
        for slot in 0..leaf.slot_count() {
            let offset = leaf.slot_offset_raw(slot)?;
            let (cell, _) = FtsPostingCell::decode(leaf.as_bytes(), offset)?;
            if cell.is_doc_lengths() {
                if saw_sidecar {
                    return Err(SQLRiteError::Internal(
                        "FTS index has more than one doc-lengths sidecar cell".to_string(),
                    ));
                }
                saw_sidecar = true;
                doc_lengths = cell.entries;
            } else {
                postings.push((cell.term, cell.entries));
            }
        }
        current = next_leaf;
    }

    if !saw_sidecar {
        return Err(SQLRiteError::Internal(
            "FTS index missing doc-lengths sidecar cell — corrupt or truncated tree".to_string(),
        ));
    }
    Ok((doc_lengths, postings))
}

/// Packs HNSW nodes into a sibling-chained run of `TableLeaf` pages.
/// `serialize_nodes` already returns nodes in ascending node_id order,
/// so the slot directory's rowid ordering stays valid.
fn stage_hnsw_leaves(
    pager: &mut Pager,
    idx: &crate::sql::hnsw::HnswIndex,
    start_page: u32,
) -> Result<(Vec<(u32, i64)>, u32)> {
    use crate::sql::pager::hnsw_cell::HnswNodeCell;

    let mut leaves: Vec<(u32, i64)> = Vec::new();
    let mut current_leaf = TablePage::empty();
    let mut current_leaf_page = start_page;
    let mut current_max_rowid: Option<i64> = None;
    let mut next_free_page = start_page + 1;

    let serialized = idx.serialize_nodes();

    // Empty index → emit a single empty leaf page so the rootpage
    // pointer in sqlrite_master stays nonzero (== "graph is persisted,
    // it just happens to be empty"). load_hnsw_nodes is fine with an
    // empty leaf — slot_count() returns 0.
    for (node_id, layers) in serialized {
        let cell = HnswNodeCell::new(node_id, layers);
        let entry_bytes = cell.encode()?;

        if !current_leaf.would_fit(entry_bytes.len()) {
            let next_leaf_page_num = next_free_page;
            emit_leaf(pager, current_leaf_page, &current_leaf, next_leaf_page_num);
            leaves.push((current_leaf_page, current_max_rowid.unwrap_or(i64::MIN)));
            current_leaf = TablePage::empty();
            current_leaf_page = next_leaf_page_num;
            next_free_page += 1;

            if !current_leaf.would_fit(entry_bytes.len()) {
                return Err(SQLRiteError::Internal(format!(
                    "HNSW node {node_id} cell of {} bytes exceeds empty-page capacity {}",
                    entry_bytes.len(),
                    current_leaf.free_space()
                )));
            }
        }
        current_leaf.insert_entry(node_id, &entry_bytes)?;
        current_max_rowid = Some(node_id);
    }

    emit_leaf(pager, current_leaf_page, &current_leaf, 0);
    leaves.push((current_leaf_page, current_max_rowid.unwrap_or(i64::MIN)));
    Ok((leaves, next_free_page))
}

fn load_table_rows(pager: &Pager, table: &mut Table, root_page: u32) -> Result<()> {
    let first_leaf = find_leftmost_leaf(pager, root_page)?;
    let mut current = first_leaf;
    while current != 0 {
        let page_buf = pager
            .read_page(current)
            .ok_or_else(|| SQLRiteError::Internal(format!("missing leaf page {current}")))?;
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
                    let body_bytes =
                        read_overflow_chain(pager, r.first_overflow_page, r.total_body_len)?;
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
fn stage_table_btree(pager: &mut Pager, table: &Table, start_page: u32) -> Result<(u32, u32)> {
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
            leaves.push((current_leaf_page, current_max_rowid.unwrap_or(i64::MIN)));
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
        process_command(
            "INSERT INTO users (name, age) VALUES ('alice', 30);",
            &mut db,
        )
        .unwrap();
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

    /// Phase 4c: every .sqlrite has a `-wal` sidecar now. Delete both so
    /// `/tmp` doesn't accumulate orphan WALs across test runs.
    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let _ = std::fs::remove_file(std::path::PathBuf::from(wal));
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

        cleanup(&path);
    }

    // -----------------------------------------------------------------
    // Phase 7a — VECTOR(N) save / reopen round-trip
    // -----------------------------------------------------------------

    #[test]
    fn round_trip_preserves_vector_column() {
        let path = tmp_path("vec_roundtrip");

        // Build, populate, save.
        {
            let mut db = Database::new("test".to_string());
            process_command(
                "CREATE TABLE docs (id INTEGER PRIMARY KEY, embedding VECTOR(3));",
                &mut db,
            )
            .unwrap();
            process_command(
                "INSERT INTO docs (embedding) VALUES ([0.1, 0.2, 0.3]);",
                &mut db,
            )
            .unwrap();
            process_command(
                "INSERT INTO docs (embedding) VALUES ([1.5, -2.0, 3.5]);",
                &mut db,
            )
            .unwrap();
            save_database(&mut db, &path).expect("save");
        } // db drops → its exclusive lock releases before reopen.

        // Reopen and verify schema + data both round-tripped.
        let loaded = open_database(&path, "test".to_string()).expect("open");
        let docs = loaded.get_table("docs".to_string()).expect("docs table");

        // Schema preserved: column is still VECTOR(3).
        let embedding_col = docs
            .columns
            .iter()
            .find(|c| c.column_name == "embedding")
            .expect("embedding column");
        assert!(
            matches!(embedding_col.datatype, DataType::Vector(3)),
            "expected DataType::Vector(3) after round-trip, got {:?}",
            embedding_col.datatype
        );

        // Data preserved: both vectors still readable bit-for-bit.
        let mut rows: Vec<Vec<f32>> = docs
            .rowids()
            .iter()
            .filter_map(|r| match docs.get_value("embedding", *r) {
                Some(Value::Vector(v)) => Some(v),
                _ => None,
            })
            .collect();
        rows.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], vec![0.1f32, 0.2, 0.3]);
        assert_eq!(rows[1], vec![1.5f32, -2.0, 3.5]);

        cleanup(&path);
    }

    #[test]
    fn round_trip_preserves_json_column() {
        // Phase 7e — JSON columns are stored as Text under the hood with
        // INSERT-time validation. Save + reopen should preserve the
        // schema (DataType::Json) and the underlying text bytes; a
        // post-reopen json_extract should still resolve paths correctly.
        let path = tmp_path("json_roundtrip");

        {
            let mut db = Database::new("test".to_string());
            process_command(
                "CREATE TABLE docs (id INTEGER PRIMARY KEY, payload JSON);",
                &mut db,
            )
            .unwrap();
            process_command(
                r#"INSERT INTO docs (payload) VALUES ('{"name": "alice", "tags": ["rust","sql"]}');"#,
                &mut db,
            )
            .unwrap();
            save_database(&mut db, &path).expect("save");
        }

        let mut loaded = open_database(&path, "test".to_string()).expect("open");
        let docs = loaded.get_table("docs".to_string()).expect("docs");

        // Schema: column declared as JSON, restored with the same type.
        let payload_col = docs
            .columns
            .iter()
            .find(|c| c.column_name == "payload")
            .unwrap();
        assert!(
            matches!(payload_col.datatype, DataType::Json),
            "expected DataType::Json, got {:?}",
            payload_col.datatype
        );

        // json_extract works against the reopened data — exercises the
        // full Text-storage + serde_json::from_str path post-reopen.
        let resp = process_command(
            r#"SELECT id FROM docs WHERE json_extract(payload, '$.name') = 'alice';"#,
            &mut loaded,
        )
        .expect("select via json_extract after reopen");
        assert!(resp.contains("1 row returned"), "got: {resp}");

        cleanup(&path);
    }

    #[test]
    fn round_trip_rebuilds_hnsw_index_from_create_sql() {
        // Phase 7d.3: HNSW indexes now persist their graph as cell-encoded
        // pages. After save+reopen the index entry reattaches with the
        // same column + same node count, loaded directly from disk
        // instead of re-walking rows.
        let path = tmp_path("hnsw_roundtrip");

        // Build, populate, index, save.
        {
            let mut db = Database::new("test".to_string());
            process_command(
                "CREATE TABLE docs (id INTEGER PRIMARY KEY, e VECTOR(2));",
                &mut db,
            )
            .unwrap();
            for v in &[
                "[1.0, 0.0]",
                "[2.0, 0.0]",
                "[0.0, 3.0]",
                "[1.0, 4.0]",
                "[10.0, 10.0]",
            ] {
                process_command(&format!("INSERT INTO docs (e) VALUES ({v});"), &mut db).unwrap();
            }
            process_command("CREATE INDEX ix_e ON docs USING hnsw (e);", &mut db).unwrap();
            save_database(&mut db, &path).expect("save");
        } // db drops → exclusive lock releases.

        // Reopen and verify the index reattached, with the same name +
        // column + populated graph.
        let mut loaded = open_database(&path, "test".to_string()).expect("open");
        {
            let table = loaded.get_table("docs".to_string()).expect("docs");
            assert_eq!(table.hnsw_indexes.len(), 1, "HNSW index should reattach");
            let entry = &table.hnsw_indexes[0];
            assert_eq!(entry.name, "ix_e");
            assert_eq!(entry.column_name, "e");
            assert_eq!(entry.index.len(), 5, "loaded graph should hold all 5 rows");
            assert!(
                !entry.needs_rebuild,
                "fresh load should not be marked dirty"
            );
        }

        // Quick functional check: KNN query through the loaded index
        // returns results.
        let resp = process_command(
            "SELECT id FROM docs ORDER BY vec_distance_l2(e, [1.0, 0.0]) ASC LIMIT 3;",
            &mut loaded,
        )
        .unwrap();
        assert!(resp.contains("3 rows returned"), "got: {resp}");

        cleanup(&path);
    }

    #[test]
    fn round_trip_rebuilds_fts_index_from_create_sql() {
        // Phase 8c: FTS indexes now persist their posting lists as
        // cell-encoded pages. After save+reopen the index entry
        // reattaches with the same column + same posting count, loaded
        // directly from disk (no re-tokenization).
        let path = tmp_path("fts_roundtrip");

        {
            let mut db = Database::new("test".to_string());
            process_command(
                "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT);",
                &mut db,
            )
            .unwrap();
            for body in &[
                "rust embedded database",
                "rust web framework",
                "go embedded systems",
                "python web framework",
                "rust rust embedded power",
            ] {
                process_command(
                    &format!("INSERT INTO docs (body) VALUES ('{body}');"),
                    &mut db,
                )
                .unwrap();
            }
            process_command("CREATE INDEX ix_body ON docs USING fts (body);", &mut db).unwrap();
            save_database(&mut db, &path).expect("save");
        } // db drops → exclusive lock releases.

        let mut loaded = open_database(&path, "test".to_string()).expect("open");
        {
            let table = loaded.get_table("docs".to_string()).expect("docs");
            assert_eq!(table.fts_indexes.len(), 1, "FTS index should reattach");
            let entry = &table.fts_indexes[0];
            assert_eq!(entry.name, "ix_body");
            assert_eq!(entry.column_name, "body");
            assert_eq!(
                entry.index.len(),
                5,
                "rebuilt posting list should hold all 5 rows"
            );
            assert!(!entry.needs_rebuild);
        }

        // Functional smoke: an FTS query through the reloaded index
        // returns the expected hit count.
        let resp = process_command(
            "SELECT id FROM docs WHERE fts_match(body, 'rust');",
            &mut loaded,
        )
        .unwrap();
        assert!(resp.contains("3 rows returned"), "got: {resp}");

        cleanup(&path);
    }

    #[test]
    fn delete_then_save_then_reopen_excludes_deleted_node_from_fts() {
        // Phase 8b — DELETE marks the FTS index dirty; save rebuilds it
        // from current rows; reopen replays the CREATE INDEX SQL against
        // the post-delete row set. The deleted rowid must not surface
        // in `fts_match` results post-reopen.
        let path = tmp_path("fts_delete_rebuild");
        let mut db = Database::new("test".to_string());
        process_command(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT);",
            &mut db,
        )
        .unwrap();
        for body in &[
            "rust embedded",
            "rust framework",
            "go embedded",
            "python web",
        ] {
            process_command(
                &format!("INSERT INTO docs (body) VALUES ('{body}');"),
                &mut db,
            )
            .unwrap();
        }
        process_command("CREATE INDEX ix_body ON docs USING fts (body);", &mut db).unwrap();

        // Delete row 1 ('rust embedded'); save (rebuild fires); reopen.
        process_command("DELETE FROM docs WHERE id = 1;", &mut db).unwrap();
        save_database(&mut db, &path).expect("save");
        drop(db);

        let mut loaded = open_database(&path, "test".to_string()).expect("open");
        let resp = process_command(
            "SELECT id FROM docs WHERE fts_match(body, 'rust');",
            &mut loaded,
        )
        .unwrap();
        // Pre-delete: 2 rows ('rust embedded', 'rust framework') had
        // 'rust'. Post-delete: only id=2 remains.
        assert!(resp.contains("1 row returned"), "got: {resp}");

        cleanup(&path);
    }

    #[test]
    fn fts_roundtrip_uses_persistence_path_not_replay() {
        // Phase 8c — assert the reload didn't go through the
        // rootpage=0 replay shortcut. We do this by reading the
        // sqlrite_master row for the FTS index and confirming its
        // rootpage field is non-zero.
        let path = tmp_path("fts_persistence_path");

        {
            let mut db = Database::new("test".to_string());
            process_command(
                "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT);",
                &mut db,
            )
            .unwrap();
            process_command(
                "INSERT INTO docs (body) VALUES ('rust embedded database');",
                &mut db,
            )
            .unwrap();
            process_command("CREATE INDEX ix_body ON docs USING fts (body);", &mut db).unwrap();
            save_database(&mut db, &path).expect("save");
        }

        // Read raw sqlrite_master to find the FTS index row.
        let pager = Pager::open(&path).expect("open pager");
        let mut master = build_empty_master_table();
        load_table_rows(&pager, &mut master, pager.header().schema_root_page).unwrap();
        let mut found_rootpage: Option<u32> = None;
        for rowid in master.rowids() {
            let name = take_text(&master, "name", rowid).unwrap();
            if name == "ix_body" {
                let rp = take_integer(&master, "rootpage", rowid).unwrap();
                found_rootpage = Some(rp as u32);
            }
        }
        let rootpage = found_rootpage.expect("ix_body row in sqlrite_master");
        assert!(
            rootpage != 0,
            "Phase 8c FTS save should set rootpage != 0; got {rootpage}"
        );

        cleanup(&path);
    }

    #[test]
    fn save_without_fts_keeps_format_v4() {
        // Phase 8c on-demand bump — a database with zero FTS indexes
        // continues writing the v4 header. Existing v4 users must not
        // see their files silently promoted to v5 by an upgrade.
        use crate::sql::pager::header::FORMAT_VERSION_V4;

        let path = tmp_path("fts_no_bump");
        let mut db = Database::new("test".to_string());
        process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER);",
            &mut db,
        )
        .unwrap();
        process_command("INSERT INTO t (n) VALUES (1);", &mut db).unwrap();
        save_database(&mut db, &path).unwrap();
        drop(db);

        let pager = Pager::open(&path).expect("open");
        assert_eq!(
            pager.header().format_version,
            FORMAT_VERSION_V4,
            "no-FTS save should keep v4"
        );
        cleanup(&path);
    }

    #[test]
    fn save_with_fts_bumps_to_v5() {
        // Phase 8c on-demand bump — first FTS-bearing save promotes
        // the file to v5. v5 readers handle both v4 and v5; v4
        // readers correctly refuse a v5 file.
        use crate::sql::pager::header::FORMAT_VERSION_V5;

        let path = tmp_path("fts_bump_v5");
        let mut db = Database::new("test".to_string());
        process_command(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT);",
            &mut db,
        )
        .unwrap();
        process_command("INSERT INTO docs (body) VALUES ('hello');", &mut db).unwrap();
        process_command("CREATE INDEX ix_body ON docs USING fts (body);", &mut db).unwrap();
        save_database(&mut db, &path).unwrap();
        drop(db);

        let pager = Pager::open(&path).expect("open");
        assert_eq!(
            pager.header().format_version,
            FORMAT_VERSION_V5,
            "FTS save should promote to v5"
        );
        cleanup(&path);
    }

    #[test]
    fn fts_persistence_handles_empty_and_zero_token_docs() {
        // Phase 8c — sidecar cell carries doc-lengths for every doc
        // including any with zero tokens (so total_docs is honest
        // post-reopen). Empty index also round-trips: a CREATE INDEX
        // on an empty table emits a single empty leaf with just the
        // (empty) sidecar.
        let path = tmp_path("fts_edges");

        {
            let mut db = Database::new("test".to_string());
            process_command(
                "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT);",
                &mut db,
            )
            .unwrap();
            process_command("CREATE INDEX ix_body ON docs USING fts (body);", &mut db).unwrap();
            // Mix: real text, then a row that tokenizes to zero tokens
            // (only punctuation), then real again.
            process_command("INSERT INTO docs (body) VALUES ('rust embedded');", &mut db).unwrap();
            process_command("INSERT INTO docs (body) VALUES ('!!!---???');", &mut db).unwrap();
            process_command("INSERT INTO docs (body) VALUES ('go embedded');", &mut db).unwrap();
            save_database(&mut db, &path).unwrap();
        }

        let loaded = open_database(&path, "test".to_string()).expect("open");
        let table = loaded.get_table("docs".to_string()).unwrap();
        let entry = &table.fts_indexes[0];
        // All three rows present — including the zero-token row,
        // which is critical for total_docs honesty in BM25.
        assert_eq!(entry.index.len(), 3);
        // 'embedded' appears in 2 rows after reload.
        let res = entry
            .index
            .query("embedded", &crate::sql::fts::Bm25Params::default());
        assert_eq!(res.len(), 2);

        cleanup(&path);
    }

    #[test]
    fn fts_persistence_round_trips_large_corpus() {
        // Phase 8c — exercise multi-leaf staging. ~500 docs with
        // single-token bodies generates enough cells to overflow a
        // single 4 KiB leaf (each posting cell averages ~8 bytes).
        let path = tmp_path("fts_large_corpus");

        let mut expected_terms: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        {
            let mut db = Database::new("test".to_string());
            process_command(
                "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT);",
                &mut db,
            )
            .unwrap();
            process_command("CREATE INDEX ix_body ON docs USING fts (body);", &mut db).unwrap();
            // 500 docs, each one a unique term — drives unique-term
            // count up so multiple leaves are required.
            for i in 0..500 {
                let term = format!("term{i:04}");
                process_command(
                    &format!("INSERT INTO docs (body) VALUES ('{term}');"),
                    &mut db,
                )
                .unwrap();
                expected_terms.insert(term);
            }
            save_database(&mut db, &path).unwrap();
        }

        let loaded = open_database(&path, "test".to_string()).expect("open");
        let table = loaded.get_table("docs".to_string()).unwrap();
        let entry = &table.fts_indexes[0];
        assert_eq!(entry.index.len(), 500);

        // Spot-check a handful of terms come back with their original
        // single-row posting list.
        for &i in &[0_i64, 137, 248, 391, 499] {
            let term = format!("term{i:04}");
            let res = entry
                .index
                .query(&term, &crate::sql::fts::Bm25Params::default());
            assert_eq!(res.len(), 1, "term {term} should match exactly 1 row");
            // PrimaryKey rowids start at 1; doc i was inserted at
            // rowid i+1.
            assert_eq!(res[0].0, i + 1);
        }

        cleanup(&path);
    }

    #[test]
    fn delete_then_save_then_reopen_excludes_deleted_node_from_hnsw() {
        // Phase 7d.3 — DELETE marks HNSW dirty; save rebuilds it from
        // current rows + serializes; reopen loads the post-delete graph.
        // After all that, the deleted rowid must NOT come back from a
        // KNN query.
        let path = tmp_path("hnsw_delete_rebuild");
        let mut db = Database::new("test".to_string());
        process_command(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, e VECTOR(2));",
            &mut db,
        )
        .unwrap();
        for v in &["[1.0, 0.0]", "[2.0, 0.0]", "[3.0, 0.0]", "[4.0, 0.0]"] {
            process_command(&format!("INSERT INTO docs (e) VALUES ({v});"), &mut db).unwrap();
        }
        process_command("CREATE INDEX ix_e ON docs USING hnsw (e);", &mut db).unwrap();

        // Delete row 1 (the closest match to [0.5, 0.0]).
        process_command("DELETE FROM docs WHERE id = 1;", &mut db).unwrap();
        // Confirm it marked dirty.
        let dirty_before_save = db.tables["docs"].hnsw_indexes[0].needs_rebuild;
        assert!(dirty_before_save, "DELETE should mark dirty");

        save_database(&mut db, &path).expect("save");
        // Confirm save cleared the dirty flag.
        let dirty_after_save = db.tables["docs"].hnsw_indexes[0].needs_rebuild;
        assert!(!dirty_after_save, "save should clear dirty");
        drop(db);

        // Reopen, query for the closest match. Row 1 is gone; row 2
        // (id=2, vector [2.0, 0.0]) should now be the nearest.
        let loaded = open_database(&path, "test".to_string()).expect("open");
        let docs = loaded.get_table("docs".to_string()).expect("docs");

        // Row 1 must not appear in any storage anymore.
        assert!(
            !docs.rowids().contains(&1),
            "deleted row 1 should not be in row storage"
        );
        assert_eq!(docs.rowids().len(), 3, "should have 3 surviving rows");

        // The HNSW index must also have shed the deleted node.
        assert_eq!(
            docs.hnsw_indexes[0].index.len(),
            3,
            "HNSW graph should have shed the deleted node"
        );

        cleanup(&path);
    }

    #[test]
    fn round_trip_survives_writes_after_load() {
        let path = tmp_path("after_load");
        save_database(&mut seed_db(), &path).unwrap();

        {
            let mut db = open_database(&path, "test".to_string()).unwrap();
            process_command(
                "INSERT INTO users (name, age) VALUES ('carol', 40);",
                &mut db,
            )
            .unwrap();
            save_database(&mut db, &path).unwrap();
        } // db drops → its exclusive lock releases before we reopen below.

        let db2 = open_database(&path, "test".to_string()).unwrap();
        let users = db2.get_table("users".to_string()).unwrap();
        assert_eq!(users.rowids().len(), 3);

        cleanup(&path);
    }

    #[test]
    fn open_rejects_garbage_file() {
        let path = tmp_path("bad");
        std::fs::write(&path, b"not a sqlrite database, just bytes").unwrap();
        let result = open_database(&path, "x".to_string());
        assert!(result.is_err());
        cleanup(&path);
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
        cleanup(&path);
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
        process_command(
            &format!("INSERT INTO docs (body) VALUES ('{body}');"),
            &mut db,
        )
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
        cleanup(&path);
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
        cleanup(&path);
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
        let pager = loaded
            .pager
            .as_ref()
            .expect("loaded DB should have a pager");
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

        cleanup(&path);
    }

    #[test]
    fn explicit_index_persists_across_save_and_open() {
        let path = tmp_path("idx_persist");
        let mut db = Database::new("idx".to_string());
        process_command(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, tag TEXT);",
            &mut db,
        )
        .unwrap();
        for i in 1..=5 {
            let tag = if i % 2 == 0 { "odd" } else { "even" };
            process_command(
                &format!("INSERT INTO users (tag) VALUES ('{tag}');"),
                &mut db,
            )
            .unwrap();
        }
        process_command("CREATE INDEX users_tag_idx ON users (tag);", &mut db).unwrap();
        save_database(&mut db, &path).unwrap();

        let loaded = open_database(&path, "idx".to_string()).unwrap();
        let users = loaded.get_table("users".to_string()).unwrap();
        let idx = users
            .index_by_name("users_tag_idx")
            .expect("explicit index should survive save/open");
        assert_eq!(idx.column_name, "tag");
        assert!(!idx.is_unique);
        // 5 rows: rowids 2, 4 are "odd" (i % 2 == 0 when i is 2 or 4) — 2 entries;
        // rowids 1, 3, 5 are "even" (i % 2 != 0) — 3 entries.
        let even_rowids = idx.lookup(&Value::Text("even".into()));
        let odd_rowids = idx.lookup(&Value::Text("odd".into()));
        assert_eq!(even_rowids.len(), 3);
        assert_eq!(odd_rowids.len(), 2);

        cleanup(&path);
    }

    #[test]
    fn auto_indexes_for_unique_columns_survive_save_open() {
        let path = tmp_path("auto_idx_persist");
        let mut db = Database::new("a".to_string());
        process_command(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE);",
            &mut db,
        )
        .unwrap();
        process_command("INSERT INTO users (email) VALUES ('a@x');", &mut db).unwrap();
        process_command("INSERT INTO users (email) VALUES ('b@x');", &mut db).unwrap();
        save_database(&mut db, &path).unwrap();

        let loaded = open_database(&path, "a".to_string()).unwrap();
        let users = loaded.get_table("users".to_string()).unwrap();
        // Every UNIQUE column auto-creates an index; the load path populated
        // it from the persisted entries.
        let auto_name = SecondaryIndex::auto_name("users", "email");
        let idx = users
            .index_by_name(&auto_name)
            .expect("auto index should be restored");
        assert!(idx.is_unique);
        assert_eq!(idx.lookup(&Value::Text("a@x".into())).len(), 1);
        assert_eq!(idx.lookup(&Value::Text("b@x".into())).len(), 1);

        cleanup(&path);
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
                    vec![
                        Some(Value::Integer(i)),
                        Some(Value::Text(format!("r-{i}-{body}"))),
                    ],
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

        cleanup(&path);
    }
}
