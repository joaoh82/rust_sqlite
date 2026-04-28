use crate::error::{Result, SQLRiteError};
use crate::sql::db::secondary_index::{IndexOrigin, SecondaryIndex};
use crate::sql::hnsw::HnswIndex;
use crate::sql::parser::create::CreateQuery;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex};

use prettytable::{Cell as PrintCell, Row as PrintRow, Table as PrintTable};

/// SQLRite data types
/// Mapped after SQLite Data Type Storage Classes and SQLite Affinity Type
/// (Datatypes In SQLite Version 3)[https://www.sqlite.org/datatype3.html]
///
/// `Vector(dim)` is the Phase 7a addition — a fixed-dimension dense f32
/// array. The dimension is part of the type so a `VECTOR(384)` column
/// rejects `[0.1, 0.2, 0.3]` at INSERT time as a clean type error
/// rather than silently storing the wrong shape.
#[derive(PartialEq, Debug, Clone)]
pub enum DataType {
    Integer,
    Text,
    Real,
    Bool,
    /// Dense f32 vector of fixed dimension. The `usize` is the column's
    /// declared dimension; every value stored in the column must have
    /// exactly that many elements.
    Vector(usize),
    None,
    Invalid,
}

impl DataType {
    /// Constructs a `DataType` from the wire string the parser produces.
    /// Pre-Phase-7 the strings were one-of `"integer" | "text" | "real" |
    /// "bool" | "none"`. Phase 7a adds `"vector(N)"` (case-insensitive,
    /// N a positive integer) for the new vector column type — encoded
    /// in-band so we don't have to plumb a richer type through the
    /// existing string-based ParsedColumn pipeline.
    pub fn new(cmd: String) -> DataType {
        let lower = cmd.to_lowercase();
        match lower.as_str() {
            "integer" => DataType::Integer,
            "text" => DataType::Text,
            "real" => DataType::Real,
            "bool" => DataType::Bool,
            "none" => DataType::None,
            other if other.starts_with("vector(") && other.ends_with(')') => {
                // Strip the `vector(` prefix and trailing `)`, parse what's
                // left as a positive integer dimension. Anything else is
                // Invalid — surfaces a clean error at CREATE TABLE time.
                let inside = &other["vector(".len()..other.len() - 1];
                match inside.trim().parse::<usize>() {
                    Ok(dim) if dim > 0 => DataType::Vector(dim),
                    _ => {
                        eprintln!("Invalid VECTOR dimension in {cmd}");
                        DataType::Invalid
                    }
                }
            }
            _ => {
                eprintln!("Invalid data type given {}", cmd);
                DataType::Invalid
            }
        }
    }

    /// Inverse of `new` — returns the canonical lowercased wire string
    /// for this DataType. Used by the parser to round-trip
    /// `VECTOR(N)` → `DataType::Vector(N)` → `"vector(N)"` into
    /// `ParsedColumn::datatype` so the rest of the pipeline keeps
    /// working with strings.
    pub fn to_wire_string(&self) -> String {
        match self {
            DataType::Integer => "Integer".to_string(),
            DataType::Text => "Text".to_string(),
            DataType::Real => "Real".to_string(),
            DataType::Bool => "Bool".to_string(),
            DataType::Vector(dim) => format!("vector({dim})"),
            DataType::None => "None".to_string(),
            DataType::Invalid => "Invalid".to_string(),
        }
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DataType::Integer => f.write_str("Integer"),
            DataType::Text => f.write_str("Text"),
            DataType::Real => f.write_str("Real"),
            DataType::Bool => f.write_str("Boolean"),
            DataType::Vector(dim) => write!(f, "Vector({dim})"),
            DataType::None => f.write_str("None"),
            DataType::Invalid => f.write_str("Invalid"),
        }
    }
}

/// The schema for each SQL Table is represented in memory by
/// following structure.
///
/// `rows` is `Arc<Mutex<...>>` rather than `Rc<RefCell<...>>` so `Table`
/// (and by extension `Database`) is `Send + Sync` — the Tauri desktop
/// app holds the engine in shared state behind a `Mutex<Database>`, and
/// Tauri's state container requires its contents to be thread-safe.
#[derive(Debug)]
pub struct Table {
    /// Name of the table
    pub tb_name: String,
    /// Schema for each column, in declaration order.
    pub columns: Vec<Column>,
    /// Per-column row storage, keyed by column name. Every column's
    /// `Row::T(BTreeMap)` is keyed by rowid, so all columns share the same
    /// keyset after each write.
    pub rows: Arc<Mutex<HashMap<String, Row>>>,
    /// Secondary indexes on this table (Phase 3e). One auto-created entry
    /// per UNIQUE or PRIMARY KEY column; explicit `CREATE INDEX` statements
    /// add more. Looking up an index: iterate by column name, or by index
    /// name via `Table::index_by_name`.
    pub secondary_indexes: Vec<SecondaryIndex>,
    /// HNSW indexes on VECTOR columns (Phase 7d.2). Maintained in lockstep
    /// with row storage on INSERT (incremental); rebuilt on open from the
    /// persisted CREATE INDEX SQL. The graph itself is NOT yet persisted —
    /// see Phase 7d.3 for cell-encoded graph storage.
    pub hnsw_indexes: Vec<HnswIndexEntry>,
    /// ROWID of most recent insert.
    pub last_rowid: i64,
    /// PRIMARY KEY column name, or "-1" if the table has no PRIMARY KEY.
    pub primary_key: String,
}

/// One HNSW index attached to a table. Phase 7d.2 only supports L2
/// distance; cosine and dot are 7d.x follow-ups (would require either
/// distinct USING methods like `hnsw_cosine` or a `WITH (metric = …)`
/// clause — see `docs/phase-7-plan.md` for the deferred decision).
#[derive(Debug, Clone)]
pub struct HnswIndexEntry {
    /// User-supplied name from `CREATE INDEX <name> …`. Unique across
    /// both `secondary_indexes` and `hnsw_indexes` on a given table.
    pub name: String,
    /// The VECTOR column this index covers.
    pub column_name: String,
    /// The graph itself.
    pub index: HnswIndex,
}

impl Table {
    pub fn new(create_query: CreateQuery) -> Self {
        let table_name = create_query.table_name;
        let mut primary_key: String = String::from("-1");
        let columns = create_query.columns;

        let mut table_cols: Vec<Column> = vec![];
        let table_rows: Arc<Mutex<HashMap<String, Row>>> = Arc::new(Mutex::new(HashMap::new()));
        let mut secondary_indexes: Vec<SecondaryIndex> = Vec::new();
        for col in &columns {
            let col_name = &col.name;
            if col.is_pk {
                primary_key = col_name.to_string();
            }
            table_cols.push(Column::new(
                col_name.to_string(),
                col.datatype.to_string(),
                col.is_pk,
                col.not_null,
                col.is_unique,
            ));

            let dt = DataType::new(col.datatype.to_string());
            let row_storage = match &dt {
                DataType::Integer => Row::Integer(BTreeMap::new()),
                DataType::Real => Row::Real(BTreeMap::new()),
                DataType::Text => Row::Text(BTreeMap::new()),
                DataType::Bool => Row::Bool(BTreeMap::new()),
                // The dimension is enforced at INSERT time against the
                // column's declared DataType::Vector(dim). The Row variant
                // itself doesn't carry the dim — every stored Vec<f32>
                // already has it via .len().
                DataType::Vector(_dim) => Row::Vector(BTreeMap::new()),
                DataType::Invalid | DataType::None => Row::None,
            };
            table_rows
                .lock()
                .expect("Table row storage mutex poisoned")
                .insert(col.name.to_string(), row_storage);

            // Auto-create an index for every UNIQUE / PRIMARY KEY column,
            // but only for types we know how to index. Real / Bool / Vector
            // UNIQUE columns fall back to the linear scan path in
            // validate_unique_constraint — same behavior as before 3e.
            // (Vector UNIQUE is unusual; the linear-scan path will work
            // via Value::Vector PartialEq, just at O(N) cost.)
            if (col.is_pk || col.is_unique) && matches!(dt, DataType::Integer | DataType::Text) {
                let name = SecondaryIndex::auto_name(&table_name, &col.name);
                match SecondaryIndex::new(
                    name,
                    table_name.clone(),
                    col.name.clone(),
                    &dt,
                    true,
                    IndexOrigin::Auto,
                ) {
                    Ok(idx) => secondary_indexes.push(idx),
                    Err(_) => {
                        // Unreachable given the matches! guard above, but
                        // the builder returns Result so we keep the arm.
                    }
                }
            }
        }

        Table {
            tb_name: table_name,
            columns: table_cols,
            rows: table_rows,
            secondary_indexes,
            // HNSW indexes only land via explicit CREATE INDEX … USING hnsw
            // statements (Phase 7d.2); never auto-created at CREATE TABLE
            // time, because there's no UNIQUE-style constraint that
            // implies a vector index.
            hnsw_indexes: Vec::new(),
            last_rowid: 0,
            primary_key,
        }
    }

    /// Deep-clones a `Table` for transaction snapshots (Phase 4f).
    ///
    /// The normal `Clone` derive would shallow-clone the `Arc<Mutex<_>>`
    /// wrapping our row storage, leaving both copies sharing the same
    /// inner map — mutating the snapshot would corrupt the live table
    /// and vice versa. Instead we lock, clone the inner `HashMap`, and
    /// wrap it in a fresh `Arc<Mutex<_>>`. Columns and indexes derive
    /// `Clone` directly (all their fields are plain data).
    pub fn deep_clone(&self) -> Self {
        let cloned_rows: HashMap<String, Row> = {
            let guard = self.rows.lock().expect("row mutex poisoned");
            guard.clone()
        };
        Table {
            tb_name: self.tb_name.clone(),
            columns: self.columns.clone(),
            rows: Arc::new(Mutex::new(cloned_rows)),
            secondary_indexes: self.secondary_indexes.clone(),
            // HnswIndexEntry derives Clone, so the snapshot owns its own
            // graph copy. Phase 4f's snapshot-rollback semantics require
            // the snapshot to be fully decoupled from live state.
            hnsw_indexes: self.hnsw_indexes.clone(),
            last_rowid: self.last_rowid,
            primary_key: self.primary_key.clone(),
        }
    }

    /// Finds an auto- or explicit-index entry for a given column. Returns
    /// `None` if the column isn't indexed.
    pub fn index_for_column(&self, column: &str) -> Option<&SecondaryIndex> {
        self.secondary_indexes
            .iter()
            .find(|i| i.column_name == column)
    }

    fn index_for_column_mut(&mut self, column: &str) -> Option<&mut SecondaryIndex> {
        self.secondary_indexes
            .iter_mut()
            .find(|i| i.column_name == column)
    }

    /// Finds a secondary index by its own name (e.g., `sqlrite_autoindex_users_email`
    /// or a user-provided CREATE INDEX name). Used by Phase 3e.2 to look up
    /// explicit indexes when DROP INDEX lands.
    #[allow(dead_code)]
    pub fn index_by_name(&self, name: &str) -> Option<&SecondaryIndex> {
        self.secondary_indexes.iter().find(|i| i.name == name)
    }

    /// Returns a `bool` informing if a `Column` with a specific name exists or not
    ///
    pub fn contains_column(&self, column: String) -> bool {
        self.columns.iter().any(|col| col.column_name == column)
    }

    /// Returns the list of column names in declaration order.
    pub fn column_names(&self) -> Vec<String> {
        self.columns.iter().map(|c| c.column_name.clone()).collect()
    }

    /// Returns all rowids currently stored in the table, in ascending order.
    /// Every column's BTreeMap has the same keyset, so we just read from the first column.
    pub fn rowids(&self) -> Vec<i64> {
        let Some(first) = self.columns.first() else {
            return vec![];
        };
        let rows = self.rows.lock().expect("rows mutex poisoned");
        rows.get(&first.column_name)
            .map(|r| r.rowids())
            .unwrap_or_default()
    }

    /// Reads a single cell at `(column, rowid)`.
    pub fn get_value(&self, column: &str, rowid: i64) -> Option<Value> {
        let rows = self.rows.lock().expect("rows mutex poisoned");
        rows.get(column).and_then(|r| r.get(rowid))
    }

    /// Removes the row identified by `rowid` from every column's storage and
    /// from every secondary index entry.
    pub fn delete_row(&mut self, rowid: i64) {
        // Snapshot the values we're about to delete so we can strip them
        // from secondary indexes by (value, rowid) before the row storage
        // disappears.
        let per_column_values: Vec<(String, Option<Value>)> = self
            .columns
            .iter()
            .map(|c| (c.column_name.clone(), self.get_value(&c.column_name, rowid)))
            .collect();

        // Remove from row storage.
        {
            let rows_clone = Arc::clone(&self.rows);
            let mut row_data = rows_clone.lock().expect("rows mutex poisoned");
            for col in &self.columns {
                if let Some(r) = row_data.get_mut(&col.column_name) {
                    match r {
                        Row::Integer(m) => {
                            m.remove(&rowid);
                        }
                        Row::Text(m) => {
                            m.remove(&rowid);
                        }
                        Row::Real(m) => {
                            m.remove(&rowid);
                        }
                        Row::Bool(m) => {
                            m.remove(&rowid);
                        }
                        Row::Vector(m) => {
                            m.remove(&rowid);
                        }
                        Row::None => {}
                    }
                }
            }
        }

        // Strip secondary-index entries. Non-indexed columns just don't
        // show up in secondary_indexes and are no-ops here.
        for (col_name, value) in per_column_values {
            if let Some(idx) = self.index_for_column_mut(&col_name) {
                if let Some(v) = value {
                    idx.remove(&v, rowid);
                }
            }
        }
    }

    /// Replays a single row at `rowid` when loading a table from disk. Takes
    /// one typed value per column (in declaration order); `None` means the
    /// stored cell carried a NULL for that column. Unlike `insert_row` this
    /// trusts the on-disk state and does *not* re-check UNIQUE — we're
    /// rebuilding a state that was already consistent when it was saved.
    pub fn restore_row(&mut self, rowid: i64, values: Vec<Option<Value>>) -> Result<()> {
        if values.len() != self.columns.len() {
            return Err(SQLRiteError::Internal(format!(
                "cell has {} values but table '{}' has {} columns",
                values.len(),
                self.tb_name,
                self.columns.len()
            )));
        }

        let column_names: Vec<String> =
            self.columns.iter().map(|c| c.column_name.clone()).collect();

        for (i, value) in values.into_iter().enumerate() {
            let col_name = &column_names[i];

            // Write into the per-column row storage first (scoped borrow so
            // the secondary-index update below doesn't fight over `self`).
            {
                let rows_clone = Arc::clone(&self.rows);
                let mut row_data = rows_clone.lock().expect("rows mutex poisoned");
                let cell = row_data.get_mut(col_name).ok_or_else(|| {
                    SQLRiteError::Internal(format!("Row storage missing for column '{col_name}'"))
                })?;

                match (cell, &value) {
                    (Row::Integer(map), Some(Value::Integer(v))) => {
                        map.insert(rowid, *v as i32);
                    }
                    (Row::Integer(_), None) => {
                        return Err(SQLRiteError::Internal(format!(
                            "Integer column '{col_name}' cannot store NULL — corrupt cell?"
                        )));
                    }
                    (Row::Text(map), Some(Value::Text(s))) => {
                        map.insert(rowid, s.clone());
                    }
                    (Row::Text(map), None) => {
                        // Matches the on-insert convention: NULL in Text
                        // storage is represented by the literal "Null"
                        // sentinel and not added to the index.
                        map.insert(rowid, "Null".to_string());
                    }
                    (Row::Real(map), Some(Value::Real(v))) => {
                        map.insert(rowid, *v as f32);
                    }
                    (Row::Real(_), None) => {
                        return Err(SQLRiteError::Internal(format!(
                            "Real column '{col_name}' cannot store NULL — corrupt cell?"
                        )));
                    }
                    (Row::Bool(map), Some(Value::Bool(v))) => {
                        map.insert(rowid, *v);
                    }
                    (Row::Bool(_), None) => {
                        return Err(SQLRiteError::Internal(format!(
                            "Bool column '{col_name}' cannot store NULL — corrupt cell?"
                        )));
                    }
                    (Row::Vector(map), Some(Value::Vector(v))) => {
                        map.insert(rowid, v.clone());
                    }
                    (Row::Vector(_), None) => {
                        return Err(SQLRiteError::Internal(format!(
                            "Vector column '{col_name}' cannot store NULL — corrupt cell?"
                        )));
                    }
                    (row, v) => {
                        return Err(SQLRiteError::Internal(format!(
                            "Type mismatch restoring column '{col_name}': storage {row:?} vs value {v:?}"
                        )));
                    }
                }
            }

            // Maintain the secondary index (if any). NULL values are skipped
            // by `insert`, matching the "NULL is not indexed" convention.
            if let Some(v) = &value {
                if let Some(idx) = self.index_for_column_mut(col_name) {
                    idx.insert(v, rowid)?;
                }
            }
        }

        if rowid > self.last_rowid {
            self.last_rowid = rowid;
        }
        Ok(())
    }

    /// Extracts a row as an ordered `Vec<Option<Value>>` matching the column
    /// declaration order. Returns `None` entries for columns that hold NULL.
    /// Used by `save_database` to turn a table's in-memory state into cells.
    pub fn extract_row(&self, rowid: i64) -> Vec<Option<Value>> {
        self.columns
            .iter()
            .map(|c| match self.get_value(&c.column_name, rowid) {
                Some(Value::Null) => None,
                Some(v) => Some(v),
                None => None,
            })
            .collect()
    }

    /// Overwrites the cell at `(column, rowid)` with `new_val`. Enforces the
    /// column's datatype and UNIQUE constraint, and updates any secondary
    /// index.
    ///
    /// Returns `Err` if the column doesn't exist, the value type is incompatible,
    /// or writing would violate UNIQUE.
    pub fn set_value(&mut self, column: &str, rowid: i64, new_val: Value) -> Result<()> {
        let col_index = self
            .columns
            .iter()
            .position(|c| c.column_name == column)
            .ok_or_else(|| SQLRiteError::General(format!("Column '{column}' not found")))?;

        // No-op write — keep storage exactly the same.
        let current = self.get_value(column, rowid);
        if current.as_ref() == Some(&new_val) {
            return Ok(());
        }

        // Enforce UNIQUE. Prefer an O(log N) index probe if we have one;
        // fall back to a full column scan otherwise (Real/Bool UNIQUE
        // columns, which don't get auto-indexed).
        if self.columns[col_index].is_unique && !matches!(new_val, Value::Null) {
            if let Some(idx) = self.index_for_column(column) {
                for other in idx.lookup(&new_val) {
                    if other != rowid {
                        return Err(SQLRiteError::General(format!(
                            "UNIQUE constraint violated for column '{column}'"
                        )));
                    }
                }
            } else {
                for other in self.rowids() {
                    if other == rowid {
                        continue;
                    }
                    if self.get_value(column, other).as_ref() == Some(&new_val) {
                        return Err(SQLRiteError::General(format!(
                            "UNIQUE constraint violated for column '{column}'"
                        )));
                    }
                }
            }
        }

        // Drop the old index entry before writing the new value, so the
        // post-write index insert doesn't clash with the previous state.
        if let Some(old) = current {
            if let Some(idx) = self.index_for_column_mut(column) {
                idx.remove(&old, rowid);
            }
        }

        // Write into the column's Row, type-checking against the declared DataType.
        let declared = &self.columns[col_index].datatype;
        {
            let rows_clone = Arc::clone(&self.rows);
            let mut row_data = rows_clone.lock().expect("rows mutex poisoned");
            let cell = row_data.get_mut(column).ok_or_else(|| {
                SQLRiteError::Internal(format!("Row storage missing for column '{column}'"))
            })?;

            match (cell, &new_val, declared) {
                (Row::Integer(m), Value::Integer(v), _) => {
                    m.insert(rowid, *v as i32);
                }
                (Row::Real(m), Value::Real(v), _) => {
                    m.insert(rowid, *v as f32);
                }
                (Row::Real(m), Value::Integer(v), _) => {
                    m.insert(rowid, *v as f32);
                }
                (Row::Text(m), Value::Text(v), _) => {
                    m.insert(rowid, v.clone());
                }
                (Row::Bool(m), Value::Bool(v), _) => {
                    m.insert(rowid, *v);
                }
                (Row::Vector(m), Value::Vector(v), DataType::Vector(declared_dim)) => {
                    if v.len() != *declared_dim {
                        return Err(SQLRiteError::General(format!(
                            "Vector dimension mismatch for column '{column}': declared {declared_dim}, got {}",
                            v.len()
                        )));
                    }
                    m.insert(rowid, v.clone());
                }
                // NULL writes: store the sentinel "Null" string for Text; for other
                // types we leave storage as-is since those BTreeMaps can't hold NULL today.
                (Row::Text(m), Value::Null, _) => {
                    m.insert(rowid, "Null".to_string());
                }
                (_, new, dt) => {
                    return Err(SQLRiteError::General(format!(
                        "Type mismatch: cannot assign {} to column '{column}' of type {dt}",
                        new.to_display_string()
                    )));
                }
            }
        }

        // Maintain the secondary index, if any. NULL values are skipped by
        // insert per convention.
        if !matches!(new_val, Value::Null) {
            if let Some(idx) = self.index_for_column_mut(column) {
                idx.insert(&new_val, rowid)?;
            }
        }

        Ok(())
    }

    /// Returns an immutable reference of `sql::db::table::Column` if the table contains a
    /// column with the specified key as a column name.
    ///
    #[allow(dead_code)]
    pub fn get_column(&mut self, column_name: String) -> Result<&Column> {
        if let Some(column) = self
            .columns
            .iter()
            .filter(|c| c.column_name == column_name)
            .collect::<Vec<&Column>>()
            .first()
        {
            Ok(column)
        } else {
            Err(SQLRiteError::General(String::from("Column not found.")))
        }
    }

    /// Validates if columns and values being inserted violate the UNIQUE constraint.
    /// PRIMARY KEY columns are automatically UNIQUE. Uses the corresponding
    /// secondary index when one exists (O(log N) lookup); falls back to a
    /// linear scan for indexable-but-not-indexed situations (e.g. a Real
    /// UNIQUE column — Real isn't in the auto-indexed set).
    pub fn validate_unique_constraint(
        &mut self,
        cols: &Vec<String>,
        values: &Vec<String>,
    ) -> Result<()> {
        for (idx, name) in cols.iter().enumerate() {
            let column = self
                .columns
                .iter()
                .find(|c| &c.column_name == name)
                .ok_or_else(|| SQLRiteError::General(format!("Column '{name}' not found")))?;
            if !column.is_unique {
                continue;
            }
            let datatype = &column.datatype;
            let val = &values[idx];

            // Parse the string value into a runtime Value according to the
            // declared column type. If parsing fails the caller's insert
            // would also fail with the same error; surface it here so we
            // don't emit a misleading "unique OK" on bad input.
            let parsed = match datatype {
                DataType::Integer => val.parse::<i64>().map(Value::Integer).map_err(|_| {
                    SQLRiteError::General(format!(
                        "Type mismatch: expected INTEGER for column '{name}', got '{val}'"
                    ))
                })?,
                DataType::Text => Value::Text(val.clone()),
                DataType::Real => val.parse::<f64>().map(Value::Real).map_err(|_| {
                    SQLRiteError::General(format!(
                        "Type mismatch: expected REAL for column '{name}', got '{val}'"
                    ))
                })?,
                DataType::Bool => val.parse::<bool>().map(Value::Bool).map_err(|_| {
                    SQLRiteError::General(format!(
                        "Type mismatch: expected BOOL for column '{name}', got '{val}'"
                    ))
                })?,
                DataType::Vector(declared_dim) => {
                    let parsed_vec = parse_vector_literal(val).map_err(|e| {
                        SQLRiteError::General(format!(
                            "Type mismatch: expected VECTOR({declared_dim}) for column '{name}', {e}"
                        ))
                    })?;
                    if parsed_vec.len() != *declared_dim {
                        return Err(SQLRiteError::General(format!(
                            "Vector dimension mismatch for column '{name}': declared {declared_dim}, got {}",
                            parsed_vec.len()
                        )));
                    }
                    Value::Vector(parsed_vec)
                }
                DataType::None | DataType::Invalid => {
                    return Err(SQLRiteError::Internal(format!(
                        "column '{name}' has an unsupported datatype"
                    )));
                }
            };

            if let Some(secondary) = self.index_for_column(name) {
                if secondary.would_violate_unique(&parsed) {
                    return Err(SQLRiteError::General(format!(
                        "UNIQUE constraint violated for column '{name}': value '{val}' already exists"
                    )));
                }
            } else {
                // No secondary index (Real / Bool UNIQUE). Linear scan.
                for other in self.rowids() {
                    if self.get_value(name, other).as_ref() == Some(&parsed) {
                        return Err(SQLRiteError::General(format!(
                            "UNIQUE constraint violated for column '{name}': value '{val}' already exists"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Inserts all VALUES in its approprieta COLUMNS, using the ROWID an embedded INDEX on all ROWS
    /// Every `Table` keeps track of the `last_rowid` in order to facilitate what the next one would be.
    /// One limitation of this data structure is that we can only have one write transaction at a time, otherwise
    /// we could have a race condition on the last_rowid.
    ///
    /// Since we are loosely modeling after SQLite, this is also a limitation of SQLite (allowing only one write transcation at a time),
    /// So we are good. :)
    ///
    /// Returns `Err` (leaving the table unchanged) when the user supplies an
    /// incompatibly-typed value — no more panics on bad input.
    pub fn insert_row(&mut self, cols: &Vec<String>, values: &Vec<String>) -> Result<()> {
        let mut next_rowid = self.last_rowid + 1;

        // Auto-assign INTEGER PRIMARY KEY when the user omits it; otherwise
        // adopt the supplied value as the new rowid.
        if self.primary_key != "-1" {
            if !cols.iter().any(|col| col == &self.primary_key) {
                // Write the auto-assigned PK into row storage, then sync
                // the secondary index.
                let val = next_rowid as i32;
                let wrote_integer = {
                    let rows_clone = Arc::clone(&self.rows);
                    let mut row_data = rows_clone.lock().expect("rows mutex poisoned");
                    let table_col_data = row_data.get_mut(&self.primary_key).ok_or_else(|| {
                        SQLRiteError::Internal(format!(
                            "Row storage missing for primary key column '{}'",
                            self.primary_key
                        ))
                    })?;
                    match table_col_data {
                        Row::Integer(tree) => {
                            tree.insert(next_rowid, val);
                            true
                        }
                        _ => false, // non-integer PK: auto-assign is a no-op
                    }
                };
                if wrote_integer {
                    let pk = self.primary_key.clone();
                    if let Some(idx) = self.index_for_column_mut(&pk) {
                        idx.insert(&Value::Integer(val as i64), next_rowid)?;
                    }
                }
            } else {
                for i in 0..cols.len() {
                    if cols[i] == self.primary_key {
                        let val = &values[i];
                        next_rowid = val.parse::<i64>().map_err(|_| {
                            SQLRiteError::General(format!(
                                "Type mismatch: PRIMARY KEY column '{}' expects INTEGER, got '{val}'",
                                self.primary_key
                            ))
                        })?;
                    }
                }
            }
        }

        // For every table column, either pick the supplied value or pad with NULL
        // so that every column's BTreeMap keeps the same rowid keyset.
        let column_names = self
            .columns
            .iter()
            .map(|col| col.column_name.to_string())
            .collect::<Vec<String>>();
        let mut j: usize = 0;
        for i in 0..column_names.len() {
            let mut val = String::from("Null");
            let key = &column_names[i];

            if let Some(supplied_key) = cols.get(j) {
                if supplied_key == &column_names[i] {
                    val = values[j].to_string();
                    j += 1;
                } else if self.primary_key == column_names[i] {
                    // PK already stored in the auto-assign branch above.
                    continue;
                }
            } else if self.primary_key == column_names[i] {
                continue;
            }

            // Step 1: write into row storage and compute the typed Value
            // we'll hand to the secondary index (if any).
            let typed_value: Option<Value> = {
                let rows_clone = Arc::clone(&self.rows);
                let mut row_data = rows_clone.lock().expect("rows mutex poisoned");
                let table_col_data = row_data.get_mut(key).ok_or_else(|| {
                    SQLRiteError::Internal(format!("Row storage missing for column '{key}'"))
                })?;

                match table_col_data {
                    Row::Integer(tree) => {
                        let parsed = val.parse::<i32>().map_err(|_| {
                            SQLRiteError::General(format!(
                                "Type mismatch: expected INTEGER for column '{key}', got '{val}'"
                            ))
                        })?;
                        tree.insert(next_rowid, parsed);
                        Some(Value::Integer(parsed as i64))
                    }
                    Row::Text(tree) => {
                        tree.insert(next_rowid, val.to_string());
                        // "Null" sentinel stays out of the index — it isn't a
                        // real user value.
                        if val != "Null" {
                            Some(Value::Text(val.to_string()))
                        } else {
                            None
                        }
                    }
                    Row::Real(tree) => {
                        let parsed = val.parse::<f32>().map_err(|_| {
                            SQLRiteError::General(format!(
                                "Type mismatch: expected REAL for column '{key}', got '{val}'"
                            ))
                        })?;
                        tree.insert(next_rowid, parsed);
                        Some(Value::Real(parsed as f64))
                    }
                    Row::Bool(tree) => {
                        let parsed = val.parse::<bool>().map_err(|_| {
                            SQLRiteError::General(format!(
                                "Type mismatch: expected BOOL for column '{key}', got '{val}'"
                            ))
                        })?;
                        tree.insert(next_rowid, parsed);
                        Some(Value::Bool(parsed))
                    }
                    Row::Vector(tree) => {
                        // The parser put a bracket-array literal into `val`
                        // (e.g. "[0.1,0.2,0.3]"). Parse it back here and
                        // dim-check against the column's declared
                        // DataType::Vector(N).
                        let parsed = parse_vector_literal(&val).map_err(|e| {
                            SQLRiteError::General(format!(
                                "Type mismatch: expected VECTOR for column '{key}', {e}"
                            ))
                        })?;
                        let declared_dim = match &self.columns[i].datatype {
                            DataType::Vector(d) => *d,
                            other => {
                                return Err(SQLRiteError::Internal(format!(
                                    "Row::Vector storage on non-Vector column '{key}' (declared as {other})"
                                )));
                            }
                        };
                        if parsed.len() != declared_dim {
                            return Err(SQLRiteError::General(format!(
                                "Vector dimension mismatch for column '{key}': declared {declared_dim}, got {}",
                                parsed.len()
                            )));
                        }
                        tree.insert(next_rowid, parsed.clone());
                        Some(Value::Vector(parsed))
                    }
                    Row::None => {
                        return Err(SQLRiteError::Internal(format!(
                            "Column '{key}' has no row storage"
                        )));
                    }
                }
            };

            // Step 2: maintain the secondary index (if any). insert() is a
            // no-op for Value::Null and cheap for other value kinds.
            if let Some(v) = typed_value.clone() {
                if let Some(idx) = self.index_for_column_mut(key) {
                    idx.insert(&v, next_rowid)?;
                }
            }

            // Step 3 (Phase 7d.2): maintain any HNSW indexes on this column.
            // The HNSW algorithm needs access to other rows' vectors when
            // wiring up neighbor edges, so build a get_vec closure that
            // pulls from the table's row storage (which we *just* updated
            // with the new value).
            if let Some(Value::Vector(new_vec)) = typed_value {
                self.maintain_hnsw_on_insert(key, next_rowid, &new_vec);
            }
        }
        self.last_rowid = next_rowid;
        Ok(())
    }

    /// After a row insert, push the new (rowid, vector) into every HNSW
    /// index whose column matches `column`. Split out of `insert_row` so
    /// the borrowing dance — we need both `&self.rows` (read other
    /// vectors) and `&mut self.hnsw_indexes` (insert into the graph) —
    /// stays localized.
    fn maintain_hnsw_on_insert(&mut self, column: &str, rowid: i64, new_vec: &[f32]) {
        // Snapshot the current vector storage so the get_vec closure
        // doesn't fight with `&mut self.hnsw_indexes`. For a typical
        // HNSW insert we touch ef_construction × log(N) other vectors,
        // so the snapshot cost is small relative to the graph wiring.
        let mut vec_snapshot: HashMap<i64, Vec<f32>> = HashMap::new();
        {
            let row_data = self.rows.lock().expect("rows mutex poisoned");
            if let Some(Row::Vector(map)) = row_data.get(column) {
                for (id, v) in map.iter() {
                    vec_snapshot.insert(*id, v.clone());
                }
            }
        }
        // The new row was just written into row storage — make sure the
        // snapshot reflects it (it should, but defensive).
        vec_snapshot.insert(rowid, new_vec.to_vec());

        for entry in &mut self.hnsw_indexes {
            if entry.column_name == column {
                entry.index.insert(rowid, new_vec, |id| {
                    vec_snapshot.get(&id).cloned().unwrap_or_default()
                });
            }
        }
    }

    /// Print the table schema to standard output in a pretty formatted way.
    ///
    /// # Example
    ///
    /// ```text
    /// let table = Table::new(payload);
    /// table.print_table_schema();
    ///
    /// Prints to standard output:
    ///    +-------------+-----------+-------------+--------+----------+
    ///    | Column Name | Data Type | PRIMARY KEY | UNIQUE | NOT NULL |
    ///    +-------------+-----------+-------------+--------+----------+
    ///    | id          | Integer   | true        | true   | true     |
    ///    +-------------+-----------+-------------+--------+----------+
    ///    | name        | Text      | false       | true   | false    |
    ///    +-------------+-----------+-------------+--------+----------+
    ///    | email       | Text      | false       | false  | false    |
    ///    +-------------+-----------+-------------+--------+----------+
    /// ```
    ///
    pub fn print_table_schema(&self) -> Result<usize> {
        let mut table = PrintTable::new();
        table.add_row(row![
            "Column Name",
            "Data Type",
            "PRIMARY KEY",
            "UNIQUE",
            "NOT NULL"
        ]);

        for col in &self.columns {
            table.add_row(row![
                col.column_name,
                col.datatype,
                col.is_pk,
                col.is_unique,
                col.not_null
            ]);
        }

        table.printstd();
        Ok(table.len() * 2 + 1)
    }

    /// Print the table data to standard output in a pretty formatted way.
    ///
    /// # Example
    ///
    /// ```text
    /// let db_table = db.get_table_mut(table_name.to_string()).unwrap();
    /// db_table.print_table_data();
    ///
    /// Prints to standard output:
    ///     +----+---------+------------------------+
    ///     | id | name    | email                  |
    ///     +----+---------+------------------------+
    ///     | 1  | "Jack"  | "jack@mail.com"        |
    ///     +----+---------+------------------------+
    ///     | 10 | "Bob"   | "bob@main.com"         |
    ///     +----+---------+------------------------+
    ///     | 11 | "Bill"  | "bill@main.com"        |
    ///     +----+---------+------------------------+
    /// ```
    ///
    pub fn print_table_data(&self) {
        let mut print_table = PrintTable::new();

        let column_names = self
            .columns
            .iter()
            .map(|col| col.column_name.to_string())
            .collect::<Vec<String>>();

        let header_row = PrintRow::new(
            column_names
                .iter()
                .map(|col| PrintCell::new(col))
                .collect::<Vec<PrintCell>>(),
        );

        let rows_clone = Arc::clone(&self.rows);
        let row_data = rows_clone.lock().expect("rows mutex poisoned");
        let first_col_data = row_data
            .get(&self.columns.first().unwrap().column_name)
            .unwrap();
        let num_rows = first_col_data.count();
        let mut print_table_rows: Vec<PrintRow> = vec![PrintRow::new(vec![]); num_rows];

        for col_name in &column_names {
            let col_val = row_data
                .get(col_name)
                .expect("Can't find any rows with the given column");
            let columns: Vec<String> = col_val.get_serialized_col_data();

            for i in 0..num_rows {
                if let Some(cell) = &columns.get(i) {
                    print_table_rows[i].add_cell(PrintCell::new(cell));
                } else {
                    print_table_rows[i].add_cell(PrintCell::new(""));
                }
            }
        }

        print_table.add_row(header_row);
        for row in print_table_rows {
            print_table.add_row(row);
        }

        print_table.printstd();
    }
}

/// The schema for each SQL column in every table.
///
/// Per-column index state moved to `Table::secondary_indexes` in Phase 3e —
/// a single `Column` describes the declared schema (name, type, constraints)
/// and nothing more.
#[derive(PartialEq, Debug, Clone)]
pub struct Column {
    pub column_name: String,
    pub datatype: DataType,
    pub is_pk: bool,
    pub not_null: bool,
    pub is_unique: bool,
}

impl Column {
    pub fn new(
        name: String,
        datatype: String,
        is_pk: bool,
        not_null: bool,
        is_unique: bool,
    ) -> Self {
        let dt = DataType::new(datatype);
        Column {
            column_name: name,
            datatype: dt,
            is_pk,
            not_null,
            is_unique,
        }
    }
}

/// The schema for each SQL row in every table is represented in memory
/// by following structure
///
/// This is an enum representing each of the available types organized in a BTreeMap
/// data structure, using the ROWID and key and each corresponding type as value
#[derive(PartialEq, Debug, Clone)]
pub enum Row {
    Integer(BTreeMap<i64, i32>),
    Text(BTreeMap<i64, String>),
    Real(BTreeMap<i64, f32>),
    Bool(BTreeMap<i64, bool>),
    /// Phase 7a: dense f32 vector storage. Each `Vec<f32>` should have
    /// length matching the column's declared `DataType::Vector(dim)`,
    /// enforced at INSERT time. The Row variant doesn't carry the dim —
    /// it lives in the column metadata.
    Vector(BTreeMap<i64, Vec<f32>>),
    None,
}

impl Row {
    fn get_serialized_col_data(&self) -> Vec<String> {
        match self {
            Row::Integer(cd) => cd.values().map(|v| v.to_string()).collect(),
            Row::Real(cd) => cd.values().map(|v| v.to_string()).collect(),
            Row::Text(cd) => cd.values().map(|v| v.to_string()).collect(),
            Row::Bool(cd) => cd.values().map(|v| v.to_string()).collect(),
            Row::Vector(cd) => cd.values().map(format_vector_for_display).collect(),
            Row::None => panic!("Found None in columns"),
        }
    }

    fn count(&self) -> usize {
        match self {
            Row::Integer(cd) => cd.len(),
            Row::Real(cd) => cd.len(),
            Row::Text(cd) => cd.len(),
            Row::Bool(cd) => cd.len(),
            Row::Vector(cd) => cd.len(),
            Row::None => panic!("Found None in columns"),
        }
    }

    /// Every column's BTreeMap is keyed by ROWID. All columns share the same keyset
    /// after an INSERT (missing columns are padded), so any column's keys are a valid
    /// iteration of the table's rowids.
    pub fn rowids(&self) -> Vec<i64> {
        match self {
            Row::Integer(m) => m.keys().copied().collect(),
            Row::Text(m) => m.keys().copied().collect(),
            Row::Real(m) => m.keys().copied().collect(),
            Row::Bool(m) => m.keys().copied().collect(),
            Row::Vector(m) => m.keys().copied().collect(),
            Row::None => vec![],
        }
    }

    pub fn get(&self, rowid: i64) -> Option<Value> {
        match self {
            Row::Integer(m) => m.get(&rowid).map(|v| Value::Integer(i64::from(*v))),
            // INSERT stores the literal string "Null" in Text columns that were omitted
            // from the query — re-map that back to a real NULL on read.
            Row::Text(m) => m.get(&rowid).map(|v| {
                if v == "Null" {
                    Value::Null
                } else {
                    Value::Text(v.clone())
                }
            }),
            Row::Real(m) => m.get(&rowid).map(|v| Value::Real(f64::from(*v))),
            Row::Bool(m) => m.get(&rowid).map(|v| Value::Bool(*v)),
            Row::Vector(m) => m.get(&rowid).map(|v| Value::Vector(v.clone())),
            Row::None => None,
        }
    }
}

/// Render a vector for human display. Used by both `Row::get_serialized_col_data`
/// (for the REPL's print-table path) and `Value::to_display_string`.
///
/// Format: `[0.1, 0.2, 0.3]` — JSON-like, decimal-minimal via `{}` Display.
/// For high-dimensional vectors (e.g. 384 elements) this produces a long
/// line; truncation ellipsis is a future polish (see Phase 7 plan, "What
/// this proposal does NOT commit to").
fn format_vector_for_display(v: &Vec<f32>) -> String {
    let mut s = String::with_capacity(v.len() * 6 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        // Default f32 Display picks the minimal-roundtrip representation,
        // so 0.1f32 prints as "0.1" not "0.10000000149011612". Good enough.
        s.push_str(&x.to_string());
    }
    s.push(']');
    s
}

/// Runtime value produced by query execution. Separate from the on-disk `Row` enum
/// so the executor can carry typed values (including NULL) across operators.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Integer(i64),
    Text(String),
    Real(f64),
    Bool(bool),
    /// Phase 7a: dense f32 vector as a runtime value. Carries its own
    /// dimension implicitly via `Vec::len`; the column it's being
    /// assigned to has a declared `DataType::Vector(N)` that's checked
    /// at INSERT/UPDATE time.
    Vector(Vec<f32>),
    Null,
}

impl Value {
    pub fn to_display_string(&self) -> String {
        match self {
            Value::Integer(v) => v.to_string(),
            Value::Text(s) => s.clone(),
            Value::Real(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Vector(v) => format_vector_for_display(v),
            Value::Null => String::from("NULL"),
        }
    }
}

/// Parse a bracket-array literal like `"[0.1, 0.2, 0.3]"` (or `"[1, 2, 3]"`)
/// into a `Vec<f32>`. The parser/insert pipeline stores vector literals as
/// strings in `InsertQuery::rows` (a `Vec<Vec<String>>`); this helper is
/// the inverse — turn the string back into a typed vector at the boundary
/// where we actually need element-typed data.
///
/// Accepts:
/// - `[]` → empty vector (caller's dimension check rejects it for VECTOR(N≥1))
/// - `[0.1, 0.2, 0.3]` → standard float syntax
/// - `[1, 2, 3]` → integers, coerced to f32 (matches `VALUES (1, 2)` for
///   `REAL` columns; we widen ints to floats automatically)
/// - whitespace tolerated everywhere (Python/JSON/pgvector convention)
///
/// Rejects with a descriptive message:
/// - missing `[` / `]`
/// - non-numeric elements (`['foo', 0.1]`)
/// - NaN / Inf literals (we accept them via `f32::from_str` but caller can
///   reject if undesired — for now we let them through; HNSW etc. will
///   reject NaN at index time)
pub fn parse_vector_literal(s: &str) -> Result<Vec<f32>> {
    let trimmed = s.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return Err(SQLRiteError::General(format!(
            "expected bracket-array literal `[...]`, got `{s}`"
        )));
    }
    let inner = &trimmed[1..trimmed.len() - 1].trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for (i, part) in inner.split(',').enumerate() {
        let element = part.trim();
        let parsed: f32 = element.parse().map_err(|_| {
            SQLRiteError::General(format!("vector element {i} (`{element}`) is not a number"))
        })?;
        out.push(parsed);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::SQLiteDialect;
    use sqlparser::parser::Parser;

    #[test]
    fn datatype_display_trait_test() {
        let integer = DataType::Integer;
        let text = DataType::Text;
        let real = DataType::Real;
        let boolean = DataType::Bool;
        let vector = DataType::Vector(384);
        let none = DataType::None;
        let invalid = DataType::Invalid;

        assert_eq!(format!("{}", integer), "Integer");
        assert_eq!(format!("{}", text), "Text");
        assert_eq!(format!("{}", real), "Real");
        assert_eq!(format!("{}", boolean), "Boolean");
        assert_eq!(format!("{}", vector), "Vector(384)");
        assert_eq!(format!("{}", none), "None");
        assert_eq!(format!("{}", invalid), "Invalid");
    }

    // -----------------------------------------------------------------
    // Phase 7a — VECTOR(N) column type
    // -----------------------------------------------------------------

    #[test]
    fn datatype_new_parses_vector_dim() {
        // Standard cases.
        assert_eq!(DataType::new("vector(1)".to_string()), DataType::Vector(1));
        assert_eq!(
            DataType::new("vector(384)".to_string()),
            DataType::Vector(384)
        );
        assert_eq!(
            DataType::new("vector(1536)".to_string()),
            DataType::Vector(1536)
        );

        // Case-insensitive on the keyword.
        assert_eq!(
            DataType::new("VECTOR(384)".to_string()),
            DataType::Vector(384)
        );

        // Whitespace inside parens tolerated (the create-parser strips it
        // but the string-based round-trip in DataType::new is the one place
        // we don't fully control input formatting).
        assert_eq!(
            DataType::new("vector( 64 )".to_string()),
            DataType::Vector(64)
        );
    }

    #[test]
    fn datatype_new_rejects_bad_vector_strings() {
        // dim = 0 is rejected (Q2: VECTOR(N≥1)).
        assert_eq!(DataType::new("vector(0)".to_string()), DataType::Invalid);
        // Non-numeric dim.
        assert_eq!(DataType::new("vector(abc)".to_string()), DataType::Invalid);
        // Empty parens.
        assert_eq!(DataType::new("vector()".to_string()), DataType::Invalid);
        // Negative dim wouldn't even parse as usize, so falls into Invalid.
        assert_eq!(DataType::new("vector(-3)".to_string()), DataType::Invalid);
    }

    #[test]
    fn datatype_to_wire_string_round_trips_vector() {
        let dt = DataType::Vector(384);
        let wire = dt.to_wire_string();
        assert_eq!(wire, "vector(384)");
        // And feeds back through DataType::new losslessly — this is the
        // round-trip the ParsedColumn pipeline relies on.
        assert_eq!(DataType::new(wire), DataType::Vector(384));
    }

    #[test]
    fn parse_vector_literal_accepts_floats() {
        let v = parse_vector_literal("[0.1, 0.2, 0.3]").expect("parse");
        assert_eq!(v, vec![0.1f32, 0.2, 0.3]);
    }

    #[test]
    fn parse_vector_literal_accepts_ints_widening_to_f32() {
        let v = parse_vector_literal("[1, 2, 3]").expect("parse");
        assert_eq!(v, vec![1.0f32, 2.0, 3.0]);
    }

    #[test]
    fn parse_vector_literal_handles_negatives_and_whitespace() {
        let v = parse_vector_literal("[ -1.5 ,  2.0,  -3.5 ]").expect("parse");
        assert_eq!(v, vec![-1.5f32, 2.0, -3.5]);
    }

    #[test]
    fn parse_vector_literal_empty_brackets_is_empty_vec() {
        let v = parse_vector_literal("[]").expect("parse");
        assert!(v.is_empty());
    }

    #[test]
    fn parse_vector_literal_rejects_non_bracketed() {
        assert!(parse_vector_literal("0.1, 0.2").is_err());
        assert!(parse_vector_literal("(0.1, 0.2)").is_err());
        assert!(parse_vector_literal("[0.1, 0.2").is_err()); // missing ]
        assert!(parse_vector_literal("0.1, 0.2]").is_err()); // missing [
    }

    #[test]
    fn parse_vector_literal_rejects_non_numeric_elements() {
        let err = parse_vector_literal("[1.0, 'foo', 3.0]").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("vector element 1") && msg.contains("'foo'"),
            "error message should pinpoint the bad element: got `{msg}`"
        );
    }

    #[test]
    fn value_vector_display_format() {
        let v = Value::Vector(vec![0.1, 0.2, 0.3]);
        assert_eq!(v.to_display_string(), "[0.1, 0.2, 0.3]");

        // Empty vector displays as `[]`.
        let empty = Value::Vector(vec![]);
        assert_eq!(empty.to_display_string(), "[]");
    }

    #[test]
    fn create_new_table_test() {
        let query_statement = "CREATE TABLE contacts (
            id INTEGER PRIMARY KEY,
            first_name TEXT NOT NULL,
            last_name TEXT NOT NULl,
            email TEXT NOT NULL UNIQUE,
            active BOOL,
            score REAL
        );";
        let dialect = SQLiteDialect {};
        let mut ast = Parser::parse_sql(&dialect, query_statement).unwrap();
        if ast.len() > 1 {
            panic!("Expected a single query statement, but there are more then 1.")
        }
        let query = ast.pop().unwrap();

        let create_query = CreateQuery::new(&query).unwrap();

        let table = Table::new(create_query);

        assert_eq!(table.columns.len(), 6);
        assert_eq!(table.last_rowid, 0);

        let id_column = "id".to_string();
        if let Some(column) = table
            .columns
            .iter()
            .filter(|c| c.column_name == id_column)
            .collect::<Vec<&Column>>()
            .first()
        {
            assert!(column.is_pk);
            assert_eq!(column.datatype, DataType::Integer);
        } else {
            panic!("column not found");
        }
    }

    #[test]
    fn print_table_schema_test() {
        let query_statement = "CREATE TABLE contacts (
            id INTEGER PRIMARY KEY,
            first_name TEXT NOT NULL,
            last_name TEXT NOT NULl
        );";
        let dialect = SQLiteDialect {};
        let mut ast = Parser::parse_sql(&dialect, query_statement).unwrap();
        if ast.len() > 1 {
            panic!("Expected a single query statement, but there are more then 1.")
        }
        let query = ast.pop().unwrap();

        let create_query = CreateQuery::new(&query).unwrap();

        let table = Table::new(create_query);
        let lines_printed = table.print_table_schema();
        assert_eq!(lines_printed, Ok(9));
    }
}
