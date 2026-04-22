use crate::error::{Result, SQLRiteError};
use crate::sql::parser::create::CreateQuery;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::rc::Rc;

use prettytable::{Cell as PrintCell, Row as PrintRow, Table as PrintTable};

/// SQLRite data types
/// Mapped after SQLite Data Type Storage Classes and SQLite Affinity Type
/// (Datatypes In SQLite Version 3)[https://www.sqlite.org/datatype3.html]
#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub enum DataType {
    Integer,
    Text,
    Real,
    Bool,
    None,
    Invalid,
}

impl DataType {
    pub fn new(cmd: String) -> DataType {
        match cmd.to_lowercase().as_ref() {
            "integer" => DataType::Integer,
            "text" => DataType::Text,
            "real" => DataType::Real,
            "bool" => DataType::Bool,
            "none" => DataType::None,
            _ => {
                eprintln!("Invalid data type given {}", cmd);
                return DataType::Invalid;
            }
        }
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            DataType::Integer => f.write_str("Integer"),
            DataType::Text => f.write_str("Text"),
            DataType::Real => f.write_str("Real"),
            DataType::Bool => f.write_str("Boolean"),
            DataType::None => f.write_str("None"),
            DataType::Invalid => f.write_str("Invalid"),
        }
    }
}

/// The schema for each SQL Table is represented in memory by
/// following structure
#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct Table {
    /// Name of the table
    pub tb_name: String,
    /// HashMap with information about each column
    pub columns: Vec<Column>,
    /// HashMap with information about each row
    pub rows: Rc<RefCell<HashMap<String, Row>>>,
    /// HashMap of SQL indexes on this table.
    pub indexes: HashMap<String, String>,
    /// ROWID of most recent insert
    pub last_rowid: i64,
    /// PRIMARY KEY Column name, if table does not have PRIMARY KEY this would be -1
    pub primary_key: String,
}

impl Table {
    pub fn new(create_query: CreateQuery) -> Self {
        let table_name = create_query.table_name;
        let mut primary_key: String = String::from("-1");
        let columns = create_query.columns;

        let mut table_cols: Vec<Column> = vec![];
        let table_rows: Rc<RefCell<HashMap<String, Row>>> = Rc::new(RefCell::new(HashMap::new()));
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

            match DataType::new(col.datatype.to_string()) {
                DataType::Integer => table_rows
                    .clone()
                    .borrow_mut()
                    .insert(col.name.to_string(), Row::Integer(BTreeMap::new())),
                DataType::Real => table_rows
                    .clone()
                    .borrow_mut()
                    .insert(col.name.to_string(), Row::Real(BTreeMap::new())),
                DataType::Text => table_rows
                    .clone()
                    .borrow_mut()
                    .insert(col.name.to_string(), Row::Text(BTreeMap::new())),
                DataType::Bool => table_rows
                    .clone()
                    .borrow_mut()
                    .insert(col.name.to_string(), Row::Bool(BTreeMap::new())),
                DataType::Invalid => table_rows
                    .clone()
                    .borrow_mut()
                    .insert(col.name.to_string(), Row::None),
                DataType::None => table_rows
                    .clone()
                    .borrow_mut()
                    .insert(col.name.to_string(), Row::None),
            };
        }

        Table {
            tb_name: table_name,
            columns: table_cols,
            rows: table_rows,
            indexes: HashMap::new(),
            last_rowid: 0,
            primary_key: primary_key,
        }
    }

    /// Returns a `bool` informing if a `Column` with a specific name exists or not
    ///
    pub fn contains_column(&self, column: String) -> bool {
        self.columns.iter().any(|col| col.column_name == column)
    }

    /// Returns the list of column names in declaration order.
    pub fn column_names(&self) -> Vec<String> {
        self.columns
            .iter()
            .map(|c| c.column_name.clone())
            .collect()
    }

    /// Returns all rowids currently stored in the table, in ascending order.
    /// Every column's BTreeMap has the same keyset, so we just read from the first column.
    pub fn rowids(&self) -> Vec<i64> {
        let Some(first) = self.columns.first() else {
            return vec![];
        };
        let rows = self.rows.borrow();
        rows.get(&first.column_name)
            .map(|r| r.rowids())
            .unwrap_or_default()
    }

    /// Reads a single cell at `(column, rowid)`.
    pub fn get_value(&self, column: &str, rowid: i64) -> Option<Value> {
        let rows = self.rows.borrow();
        rows.get(column).and_then(|r| r.get(rowid))
    }

    /// Removes the row identified by `rowid` from every column's storage and
    /// from every column's index.
    pub fn delete_row(&mut self, rowid: i64) {
        {
            let rows_clone = Rc::clone(&self.rows);
            let mut row_data = rows_clone.as_ref().borrow_mut();
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
                        Row::None => {}
                    }
                }
            }
        }
        // Drop any index entries that pointed at this rowid.
        for col in &mut self.columns {
            match &mut col.index {
                Index::Integer(idx) => {
                    idx.retain(|_k, v| *v != rowid);
                }
                Index::Text(idx) => {
                    idx.retain(|_k, v| *v != rowid);
                }
                Index::None => {}
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

        let column_names: Vec<String> = self
            .columns
            .iter()
            .map(|c| c.column_name.clone())
            .collect();

        for (i, value) in values.into_iter().enumerate() {
            let col_name = &column_names[i];
            let rows_clone = Rc::clone(&self.rows);
            let mut row_data = rows_clone.as_ref().borrow_mut();
            let cell = row_data.get_mut(col_name).ok_or_else(|| {
                SQLRiteError::Internal(format!("Row storage missing for column '{col_name}'"))
            })?;

            let column = self
                .columns
                .iter_mut()
                .find(|c| c.column_name == *col_name)
                .ok_or_else(|| {
                    SQLRiteError::Internal(format!("Column '{col_name}' not found"))
                })?;

            match (cell, value) {
                (Row::Integer(map), Some(Value::Integer(v))) => {
                    map.insert(rowid, v as i32);
                    if let Index::Integer(idx) = &mut column.index {
                        idx.insert(v as i32, rowid);
                    }
                }
                (Row::Integer(_), None) => {
                    return Err(SQLRiteError::Internal(format!(
                        "Integer column '{col_name}' cannot store NULL — corrupt cell?"
                    )));
                }
                (Row::Text(map), Some(Value::Text(s))) => {
                    if let Index::Text(idx) = &mut column.index {
                        idx.insert(s.clone(), rowid);
                    }
                    map.insert(rowid, s);
                }
                (Row::Text(map), None) => {
                    // Matches the on-insert convention: NULL in Text storage
                    // is represented by the literal "Null" sentinel and not
                    // added to the index.
                    map.insert(rowid, "Null".to_string());
                }
                (Row::Real(map), Some(Value::Real(v))) => {
                    map.insert(rowid, v as f32);
                }
                (Row::Real(_), None) => {
                    return Err(SQLRiteError::Internal(format!(
                        "Real column '{col_name}' cannot store NULL — corrupt cell?"
                    )));
                }
                (Row::Bool(map), Some(Value::Bool(v))) => {
                    map.insert(rowid, v);
                }
                (Row::Bool(_), None) => {
                    return Err(SQLRiteError::Internal(format!(
                        "Bool column '{col_name}' cannot store NULL — corrupt cell?"
                    )));
                }
                (row, value) => {
                    return Err(SQLRiteError::Internal(format!(
                        "Type mismatch restoring column '{col_name}': storage {row:?} vs value {value:?}"
                    )));
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
    /// column's datatype and UNIQUE constraint, and updates any index.
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

        // Enforce UNIQUE: scan other rows for the same new value.
        if self.columns[col_index].is_unique && !matches!(new_val, Value::Null) {
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

        // Drop the old index entry (if any) for this rowid.
        match &mut self.columns[col_index].index {
            Index::Integer(idx) => {
                idx.retain(|_k, v| *v != rowid);
            }
            Index::Text(idx) => {
                idx.retain(|_k, v| *v != rowid);
            }
            Index::None => {}
        }

        // Write into the column's Row, type-checking against the declared DataType.
        let declared = &self.columns[col_index].datatype;
        let rows_clone = Rc::clone(&self.rows);
        let mut row_data = rows_clone.as_ref().borrow_mut();
        let cell = row_data.get_mut(column).ok_or_else(|| {
            SQLRiteError::Internal(format!("Row storage missing for column '{column}'"))
        })?;

        match (cell, &new_val, declared) {
            (Row::Integer(m), Value::Integer(v), _) => {
                m.insert(rowid, *v as i32);
                if let Index::Integer(idx) = &mut self.columns[col_index].index {
                    idx.insert(*v as i32, rowid);
                }
            }
            (Row::Real(m), Value::Real(v), _) => {
                m.insert(rowid, *v as f32);
            }
            (Row::Real(m), Value::Integer(v), _) => {
                m.insert(rowid, *v as f32);
            }
            (Row::Text(m), Value::Text(v), _) => {
                m.insert(rowid, v.clone());
                if let Index::Text(idx) = &mut self.columns[col_index].index {
                    idx.insert(v.clone(), rowid);
                }
            }
            (Row::Bool(m), Value::Bool(v), _) => {
                m.insert(rowid, *v);
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

    /// Returns an mutable reference of `sql::db::table::Column` if the table contains a
    /// column with the specified key as a column name.
    ///
    pub fn get_column_mut<'a>(&mut self, column_name: String) -> Result<&mut Column> {
        for elem in self.columns.iter_mut() {
            if elem.column_name == column_name {
                return Ok(elem);
            }
        }
        Err(SQLRiteError::General(String::from("Column not found.")))
    }

    /// Validates if columns and values being inserted violate the UNIQUE constraint
    /// As a reminder the PRIMARY KEY column automatically also is a UNIQUE column.
    ///
    pub fn validate_unique_constraint(
        &mut self,
        cols: &Vec<String>,
        values: &Vec<String>,
    ) -> Result<()> {
        for (idx, name) in cols.iter().enumerate() {
            let column = self.get_column_mut(name.to_string())?;
            if !column.is_unique {
                continue;
            }
            let val = &values[idx];
            match &column.index {
                Index::Integer(index) => {
                    let parsed = val.parse::<i32>().map_err(|_| {
                        SQLRiteError::General(format!(
                            "Type mismatch: expected INTEGER for column '{name}', got '{val}'"
                        ))
                    })?;
                    if index.contains_key(&parsed) {
                        return Err(SQLRiteError::General(format!(
                            "UNIQUE constraint violated for column '{name}': value '{val}' already exists"
                        )));
                    }
                }
                Index::Text(index) => {
                    if index.contains_key(val) {
                        return Err(SQLRiteError::General(format!(
                            "UNIQUE constraint violated for column '{name}': value '{val}' already exists"
                        )));
                    }
                }
                Index::None => {
                    return Err(SQLRiteError::General(format!(
                        "UNIQUE column '{name}' has no index"
                    )));
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

        // Auto-assign INTEGER PRIMARY KEY when it's not supplied by the user;
        // otherwise use the supplied value as the new rowid (and reject non-ints).
        if self.primary_key != "-1" {
            if !cols.iter().any(|col| col == &self.primary_key) {
                let rows_clone = Rc::clone(&self.rows);
                let mut row_data = rows_clone.as_ref().borrow_mut();
                let table_col_data = row_data.get_mut(&self.primary_key).ok_or_else(|| {
                    SQLRiteError::Internal(format!(
                        "Row storage missing for primary key column '{}'",
                        self.primary_key
                    ))
                })?;

                let column_headers = self.get_column_mut(self.primary_key.to_string())?;
                let col_index = column_headers.get_mut_index();

                if let Row::Integer(tree) = table_col_data {
                    let val = next_rowid as i32;
                    tree.insert(next_rowid, val);
                    if let Index::Integer(index) = col_index {
                        index.insert(val, next_rowid);
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

            let rows_clone = Rc::clone(&self.rows);
            let mut row_data = rows_clone.as_ref().borrow_mut();
            let table_col_data = row_data.get_mut(key).ok_or_else(|| {
                SQLRiteError::Internal(format!("Row storage missing for column '{key}'"))
            })?;

            let column_headers = self.get_column_mut(key.to_string())?;
            let col_index = column_headers.get_mut_index();

            match table_col_data {
                Row::Integer(tree) => {
                    let parsed = val.parse::<i32>().map_err(|_| {
                        SQLRiteError::General(format!(
                            "Type mismatch: expected INTEGER for column '{key}', got '{val}'"
                        ))
                    })?;
                    tree.insert(next_rowid, parsed);
                    if let Index::Integer(index) = col_index {
                        index.insert(parsed, next_rowid);
                    }
                }
                Row::Text(tree) => {
                    tree.insert(next_rowid, val.to_string());
                    if let Index::Text(index) = col_index {
                        // Never index the NULL sentinel — it isn't a real value and
                        // would collide across rows with missing data.
                        if val != "Null" {
                            index.insert(val.to_string(), next_rowid);
                        }
                    }
                }
                Row::Real(tree) => {
                    let parsed = val.parse::<f32>().map_err(|_| {
                        SQLRiteError::General(format!(
                            "Type mismatch: expected REAL for column '{key}', got '{val}'"
                        ))
                    })?;
                    tree.insert(next_rowid, parsed);
                }
                Row::Bool(tree) => {
                    let parsed = val.parse::<bool>().map_err(|_| {
                        SQLRiteError::General(format!(
                            "Type mismatch: expected BOOL for column '{key}', got '{val}'"
                        ))
                    })?;
                    tree.insert(next_rowid, parsed);
                }
                Row::None => {
                    return Err(SQLRiteError::Internal(format!(
                        "Column '{key}' has no row storage"
                    )));
                }
            }
        }
        self.last_rowid = next_rowid;
        Ok(())
    }

    /// Print the table schema to standard output in a pretty formatted way
    ///
    /// # Example
    ///
    /// ```
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

    /// Print the table data to standard output in a pretty formatted way
    ///
    /// # Example
    ///
    /// ```
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
                .map(|col| PrintCell::new(&col))
                .collect::<Vec<PrintCell>>(),
        );

        let rows_clone = Rc::clone(&self.rows);
        let row_data = rows_clone.as_ref().borrow();
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

/// The schema for each SQL column in every table is represented in memory
/// by following structure
#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct Column {
    /// Name of the column
    pub column_name: String,
    /// Datatype of column
    pub datatype: DataType,
    /// Value representing if column is PRIMARY KEY
    pub is_pk: bool,
    /// Value representing if column was declared with the NOT NULL Constraint
    pub not_null: bool,
    /// Value representing if column was declared with the UNIQUE Constraint
    pub is_unique: bool,
    /// Value representing if column is Indexed or not
    pub is_indexed: bool,
    /// BtreeMap mapping the index to a payload value on the corresponding Row
    /// Mapped using a ROWID
    pub index: Index,
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
        let index = match dt {
            DataType::Integer => Index::Integer(BTreeMap::new()),
            DataType::Bool => Index::None,
            DataType::Text => Index::Text(BTreeMap::new()),
            DataType::Real => Index::None,
            DataType::Invalid => Index::None,
            DataType::None => Index::None,
        };

        Column {
            column_name: name,
            datatype: dt,
            is_pk,
            not_null,
            is_unique,
            is_indexed: if is_pk { true } else { false },
            index,
        }
    }

    pub fn get_mut_index(&mut self) -> &mut Index {
        return &mut self.index;
    }
}

/// The schema for each SQL column index in every table is represented in memory
/// by following structure
#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub enum Index {
    Integer(BTreeMap<i32, i64>),
    Text(BTreeMap<String, i64>),
    None,
}

/// The schema for each SQL row in every table is represented in memory
/// by following structure
///
/// This is an enum representing each of the available types organized in a BTreeMap
/// data structure, using the ROWID and key and each corresponding type as value
#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub enum Row {
    Integer(BTreeMap<i64, i32>),
    Text(BTreeMap<i64, String>),
    Real(BTreeMap<i64, f32>),
    Bool(BTreeMap<i64, bool>),
    None,
}

impl Row {
    fn get_serialized_col_data(&self) -> Vec<String> {
        match self {
            Row::Integer(cd) => cd.iter().map(|(_i, v)| v.to_string()).collect(),
            Row::Real(cd) => cd.iter().map(|(_i, v)| v.to_string()).collect(),
            Row::Text(cd) => cd.iter().map(|(_i, v)| v.to_string()).collect(),
            Row::Bool(cd) => cd.iter().map(|(_i, v)| v.to_string()).collect(),
            Row::None => panic!("Found None in columns"),
        }
    }

    fn count(&self) -> usize {
        match self {
            Row::Integer(cd) => cd.len(),
            Row::Real(cd) => cd.len(),
            Row::Text(cd) => cd.len(),
            Row::Bool(cd) => cd.len(),
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
            Row::None => None,
        }
    }
}

/// Runtime value produced by query execution. Separate from the on-disk `Row` enum
/// so the executor can carry typed values (including NULL) across operators.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Integer(i64),
    Text(String),
    Real(f64),
    Bool(bool),
    Null,
}

impl Value {
    pub fn to_display_string(&self) -> String {
        match self {
            Value::Integer(v) => v.to_string(),
            Value::Text(s) => s.clone(),
            Value::Real(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => String::from("NULL"),
        }
    }
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
        let none = DataType::None;
        let invalid = DataType::Invalid;

        assert_eq!(format!("{}", integer), "Integer");
        assert_eq!(format!("{}", text), "Text");
        assert_eq!(format!("{}", real), "Real");
        assert_eq!(format!("{}", boolean), "Boolean");
        assert_eq!(format!("{}", none), "None");
        assert_eq!(format!("{}", invalid), "Invalid");
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
        let mut ast = Parser::parse_sql(&dialect, &query_statement).unwrap();
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
            assert_eq!(column.is_pk, true);
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
        let mut ast = Parser::parse_sql(&dialect, &query_statement).unwrap();
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
