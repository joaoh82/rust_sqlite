use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::error::{Result, SQLRiteError};
use crate::sql::db::table::Table;

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct Database {
    pub tables: HashMap<String, Table>,
}

impl Database {
    /// Creates an empty `Database`
    ///
    /// # Examples
    ///
    /// ```
    /// let db = sql::db::database::Database::new();
    /// ``` 
    pub fn new() -> Database {
        Database {
            tables: HashMap::new(),
        }
    }

    /// Returns true if the database contains a table with the specified key as a table name.
    ///
    /// # Examples
    ///
    /// ```
    /// let db = sql::db::database::Database::new();
    /// db.tables.insert("users", sql::db::database::Database::new());
    /// assert_eq!(db.contains_table("users".to_string(), true);
    /// assert_eq!(db.contains_table("contacts".to_string(), false);
    /// ``` 
    pub fn contains_table(&self, table_name: String) -> bool {
        self.tables.contains_key(&table_name)
    }

    pub fn get_table(&self, table_name: String) -> Result<&Table> {
        if let Some(table) = self.tables.get(&table_name) {
            Ok(table)
        } else {
            Err(SQLRiteError::General(String::from("Table not found.")))
        }
    }

    pub fn get_table_mut(&mut self, table_name: String) -> Result<&mut Table> {
        if let Some(table) = self.tables.get_mut(&table_name) {
            Ok(table)
        } else {
            Err(SQLRiteError::General(String::from("Table not found.")))
        }
    }
}
