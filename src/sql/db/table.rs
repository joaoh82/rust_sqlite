use std::collections::{BTreeMap, HashMap};
use serde::{Deserialize, Serialize};
use std::fmt;
// use crate::error::{Result};
use crate::sql::parser::create::{CreateQuery};


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
    pub rows: HashMap<String, Row>, 
    /// HashMap of SQL indexes on this table.
    pub indexes: HashMap<String, String>, 
    /// ROWID of most recent insert
    pub last_rowid: i64 
}

impl Table {
    pub fn new (create_query: CreateQuery) -> Self {
        let table_name = create_query.table_name;
        let columns = create_query.columns;

        let mut table_cols: HashMap<String, Column> = HashMap::new();
        let mut table_rows: HashMap<String, Row> = HashMap::new();
        for col in &columns {
            let col_name = &col.name;
            table_cols.insert(col_name.clone(), Column::new(
                col_name.to_string(),
                col.datatype.to_string(),
                col.is_pk,
                col.not_null,
                col.is_unique,
            ));

            match DataType::new(col.datatype.to_string()) {
                DataType::Integer => table_rows.insert(col.name.to_string(), Row::Integer(BTreeMap::new())),
                DataType::Real => table_rows.insert(col.name.to_string(), Row::Real(BTreeMap::new())),
                DataType::Text => table_rows.insert(col.name.to_string(), Row::Text(BTreeMap::new())),
                DataType::Bool => table_rows.insert(col.name.to_string(), Row::Bool(BTreeMap::new())),
                DataType::Invalid => table_rows.insert(col.name.to_string(), Row::None),
                DataType::None => table_rows.insert(col.name.to_string(), Row::None),
            };
        }

        Table {
            tb_name: table_name,
            columns: table_cols,
            rows: table_rows,
            indexes: HashMap::new(),
            last_rowid: 0, 
        }
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
    pub fn new(name: String, datatype: String, is_pk: bool, not_null: bool, is_unique: bool) -> Self {
        let dt = DataType::new(datatype);
        let index = match dt {
            DataType::Integer => Index::Integer(BTreeMap::new()),
            DataType::Bool => Index::None,
            DataType::Text => Index::Text(BTreeMap::new()),
            DataType::Real => Index::Real(BTreeMap::new()),
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
}

/// The schema for each SQL column index in every table is represented in memory 
/// by following structure
#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub enum Index {
    Integer(BTreeMap<i32, i64>),
    Text(BTreeMap<String, i64>),
    Real(BTreeMap<bool, f32>),
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
    use sqlparser::parser::{Parser};

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