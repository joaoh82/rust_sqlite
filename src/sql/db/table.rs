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
            table_cols.push(
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
        self.columns.iter().any(|col| col.column_name == column)
    }

    /// Returns an immutable reference of `sql::db::table::Column` if the table contains a
    /// column with the specified key as a column name.
    ///
    pub fn get_column(&mut self, column_name: String) -> Result<&Column> {
        if let Some(column) = self.columns
            .iter()
            .filter(|c| c.column_name == column_name)
            .collect::<Vec<&Column>>()
            .first(){
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
                if elem.column_name == column_name{
                    return Ok(elem)
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

        // Checks if table has a PRIMARY KEY
        if self.primary_key != "-1"{
            // Checking if primary key is in INSERT QUERY columns
            // If it is not, assign the next_rowid to it
            if !cols.iter().any(|col| col == &self.primary_key) {
                let rows_clone = Rc::clone(&self.rows);
                let mut row_data = rows_clone.as_ref().borrow_mut();
                let mut table_col_data = row_data.get_mut(&self.primary_key).unwrap();

                // Getting the header based on the column name
                let column_headers = self.get_column_mut(self.primary_key.to_string()).unwrap();

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
        }

        // This block checks if there are any columns from table missing
        // from INSERT statement. If there are, we add "Null" to the column.
        // We do this because otherwise the ROWID reference for each value would be wrong
        // Since rows not always have the same length.
        let column_names = self
            .columns
            .iter()
            .map(|col| col.column_name.to_string())
            .collect::<Vec<String>>();
        let mut j: usize = 0;
        // For every column in the INSERT statement
        for i in 0..column_names.len() {
            let mut val = String::from("Null");
            let mut key = &column_names[i];

            if let Some(key) = &cols.get(j){
                if &key.to_string() == &column_names[i] {
                    // Getting column name
                    val = values[j].to_string();
                    j += 1;
                } else{
                    if &self.primary_key == &column_names[i]{
                        continue
                    }
                }
            }else{
                if &self.primary_key == &column_names[i]{
                    continue
                }
            }

            // Getting the rows from the column name
            let rows_clone = Rc::clone(&self.rows);
            let mut row_data = rows_clone.as_ref().borrow_mut();
            let mut table_col_data = row_data.get_mut(key).unwrap();

            // Getting the header based on the column name
            let column_headers = self.get_column_mut(key.to_string()).unwrap();

            // Getting index for column, if it exist
            let col_index = column_headers.get_mut_index();

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
        table.add_row(row!["Column Name", "Data Type", "PRIMARY KEY", "UNIQUE", "NOT NULL"]);

        for col in &self.columns {
            table.add_row(row![col.column_name, col.datatype, col.is_pk, col.is_unique, col.not_null]);
        }

        let lines = table.printstd();
        Ok(lines)
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
        let first_col_data = row_data.get(&self.columns.first().unwrap().column_name).unwrap();
        let num_rows = first_col_data.count();
        let mut print_table_rows: Vec<PrintRow> = vec![PrintRow::new(vec![]); num_rows];

        for col_name in &column_names {
            let col_val = row_data
                .get(col_name)
                .expect("Can't find any rows with the given column");
            let columns: Vec<String> = col_val.get_serialized_col_data();

            for i in 0..num_rows {
                if let Some(cell) = &columns.get(i){
                    print_table_rows[i].add_cell(PrintCell::new(cell));
                }else{
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
            Row::Integer(cd) => cd.iter().map(|(i, v)| v.to_string()).collect(),
            Row::Real(cd) => cd.iter().map(|(i, v)| v.to_string()).collect(),
            Row::Text(cd) => cd.iter().map(|(i, v)| v.to_string()).collect(),
            Row::Bool(cd) => cd.iter().map(|(i, v)| v.to_string()).collect(),
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
        if let Some(column) = table.columns
            .iter()
            .filter(|c| c.column_name == id_column)
            .collect::<Vec<&Column>>()
            .first() {
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
