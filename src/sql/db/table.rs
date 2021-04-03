use crate::error::{Result, SQLRiteError};
use crate::sql::parser::create::CreateQuery;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::rc::Rc;

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
    pub columns: HashMap<String, Column>,
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

        let mut table_cols: HashMap<String, Column> = HashMap::new();
        let table_rows: Rc<RefCell<HashMap<String, Row>>> = Rc::new(RefCell::new(HashMap::new()));
        for col in &columns {
            let col_name = &col.name;
            if col.is_pk {
                primary_key = col_name.to_string();
            }
            table_cols.insert(
                col_name.clone(),
                Column::new(
                    col_name.to_string(),
                    col.datatype.to_string(),
                    col.is_pk,
                    col.not_null,
                    col.is_unique,
                ),
            );

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
        self.columns.contains_key(&column)
    }

    /// Returns an immutable reference of `sql::db::table::Column` if the table contains a
    /// column with the specified key as a column name.
    ///
    pub fn get_column(&mut self, column_name: String) -> Result<&Column> {
        if let Some(column) = self.columns.get(&column_name) {
            Ok(column)
        } else {
            Err(SQLRiteError::General(String::from("Column not found.")))
        }
    }

    /// Returns an mutable reference of `sql::db::table::Column` if the table contains a
    /// column with the specified key as a column name.
    ///
    pub fn get_column_mut(&mut self, column_name: String) -> Result<&mut Column> {
        if let Some(column) = self.columns.get_mut(&column_name) {
            Ok(column)
        } else {
            Err(SQLRiteError::General(String::from("Column not found.")))
        }
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
            let column = self.get_column_mut(name.to_string()).unwrap();
            // println!(
            //     "name: {} | is_pk: {} | is_unique: {}, not_null: {}",
            //     name, column.is_pk, column.is_unique, column.not_null
            // );
            if column.is_unique {
                let col_idx = &column.index;
                if *name == *column.column_name {
                    let val = &values[idx];
                    match col_idx {
                        Index::Integer(index) => {
                            if index.contains_key(&val.parse::<i32>().unwrap()) {
                                return Err(SQLRiteError::General(format!(
                                    "Error: unique constraint violation for column {}.
                        Value {} already exists for column {}",
                                    *name, val, *name
                                )));
                            }
                        }
                        Index::Text(index) => {
                            if index.contains_key(val) {
                                return Err(SQLRiteError::General(format!(
                                    "Error: unique constraint violation for column {}.
                        Value {} already exists for column {}",
                                    *name, val, *name
                                )));
                            }
                        }
                        Index::None => {
                            return Err(SQLRiteError::General(format!(
                                "Error: cannot find index for column {}",
                                name
                            )));
                        }
                    };
                }
            }
        }
        return Ok(());
    }

    /// Inserts all VALUES in its approprieta COLUMNS, using the ROWID an embedded INDEX on all ROWS
    /// Every `Table` keeps track of the `last_rowid` in order to facilitate what the next one would be.
    /// One limitation of this data structure is that we can only have one write transaction at a time, otherwise
    /// we could have a race condition on the last_rowid.println!
    ///
    /// Since we are loosely modeling after SQLite, this is also a limitation of SQLite (allowing only one write transcation at a time),
    /// So we are good. :)
    ///
    pub fn insert_row(&mut self, cols: &Vec<String>, values: &Vec<String>) {
        let mut next_rowid = self.last_rowid + i64::from(1);

        // Checking if primary key is in INSERT QUERY columns
        // If it is not, assign the next_rowid to it.
        if !cols.iter().any(|col| col == &self.primary_key) {
            let rows_clone = Rc::clone(&self.rows);
            let mut row_data = rows_clone.as_ref().borrow_mut();
            let mut table_col_data = row_data.get_mut(&self.primary_key).unwrap();

            // Getting the header based on the column name
            let column_headers = self.columns.get_mut(&self.primary_key).unwrap();

            // Getting index for column, if it exist
            let col_index = column_headers.get_mut_index();

            // We only AUTO ASSIGN in case the ROW is a PRIMARY KEY and INTEGER type
            match &mut table_col_data {
                Row::Integer(tree) => {
                    let val = next_rowid as i32;
                    tree.insert(next_rowid.clone(), val);
                    if let Index::Integer(index) = col_index {
                        index.insert(val, next_rowid.clone());
                    }
                }
                _ => (),
            }
        } else {
            // If PRIMARY KEY Column is in the Column list from INSERT Query,
            // We get the value assigned to it in the VALUES part of the query
            // and assign it to next_rowid, so every value if indexed by same rowid
            // Also, next ROWID should keep AUTO INCREMENTING from last ROWID
            let rows_clone = Rc::clone(&self.rows);
            let mut row_data = rows_clone.as_ref().borrow_mut();
            let mut table_col_data = row_data.get_mut(&self.primary_key).unwrap();

            // Again, this is only valid for PRIMARY KEYs of INTEGER type
            match &mut table_col_data {
                Row::Integer(_) => {
                    for i in 0..cols.len() {
                        // Getting column name
                        let key = &cols[i];
                        if key == &self.primary_key {
                            let val = &values[i];
                            next_rowid = val.parse::<i64>().unwrap();
                        }
                    }
                }
                _ => (),
            }
        }

        // For every column in the INSERT statement
        for i in 0..cols.len() {
            // Getting column name
            let key = &cols[i];

            // Getting the rows from the column name
            let rows_clone = Rc::clone(&self.rows);
            let mut row_data = rows_clone.as_ref().borrow_mut();
            let mut table_col_data = row_data.get_mut(key).unwrap();

            // Getting the header based on the column name
            let column_headers = self.columns.get_mut(&key.to_string()).unwrap();

            // Getting index for column, if it exist
            let col_index = column_headers.get_mut_index();

            let val = &values[i];
            match &mut table_col_data {
                Row::Integer(tree) => {
                    let val = val.parse::<i32>().unwrap();
                    tree.insert(next_rowid.clone(), val);
                    if let Index::Integer(index) = col_index {
                        index.insert(val, next_rowid.clone());
                    }
                }
                Row::Text(tree) => {
                    tree.insert(next_rowid.clone(), val.to_string());
                    if let Index::Text(index) = col_index {
                        index.insert(val.to_string(), next_rowid.clone());
                    }
                }
                Row::Real(tree) => {
                    let val = val.parse::<f32>().unwrap();
                    tree.insert(next_rowid.clone(), val);
                }
                Row::Bool(tree) => {
                    let val = val.parse::<bool>().unwrap();
                    tree.insert(next_rowid.clone(), val);
                }
                Row::None => panic!("None data Found"),
            }
        }
        self.last_rowid = next_rowid;
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
            email TEXT NOT NULL UNIQUE
        );";
        let dialect = SQLiteDialect {};
        let mut ast = Parser::parse_sql(&dialect, &query_statement).unwrap();
        if ast.len() > 1 {
            panic!("Expected a single query statement, but there are more then 1.")
        }
        let query = ast.pop().unwrap();

        let create_query = CreateQuery::new(&query).unwrap();

        let table = Table::new(create_query);

        assert_eq!(table.columns.len(), 4);
        assert_eq!(table.last_rowid, 0);

        let id_column = "id".to_string();
        if let Some(ok) = table.columns.get(&id_column) {
            assert_eq!(ok.is_pk, true);
            assert_eq!(ok.datatype, DataType::Integer);
        } else {
            panic!("column not found");
        }
    }
}
