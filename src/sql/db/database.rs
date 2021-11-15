use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::error::{Result, SQLRiteError};
use crate::sql::db::table::Table;

/// The database is represented by this structure.assert_eq!
#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct Database {
    /// Name of this database. (schema name, not filename)
    pub db_name: String,
    /// HashMap of tables in this database
    pub tables: HashMap<String, Table>,
}

impl Database {
    /// Creates an empty `Database`
    ///
    /// # Examples
    ///
    /// ```
    /// let mut db = sql::db::database::Database::new("my_db".to_string());
    /// ```
    pub fn new(db_name: String) -> Self {
        Database {
            db_name,
            tables: HashMap::new(),
        }
    }

    /// Returns true if the database contains a table with the specified key as a table name.
    ///
    pub fn contains_table(&self, table_name: String) -> bool {
        self.tables.contains_key(&table_name)
    }

    /// Returns an immutable reference of `sql::db::table::Table` if the database contains a
    /// table with the specified key as a table name.
    ///
    pub fn get_table(&self, table_name: String) -> Result<&Table> {
        if let Some(table) = self.tables.get(&table_name) {
            Ok(table)
        } else {
            Err(SQLRiteError::General(String::from("Table not found.")))
        }
    }

    /// Returns an mutable reference of `sql::db::table::Table` if the database contains a
    /// table with the specified key as a table name.
    ///
    pub fn get_table_mut(&mut self, table_name: String) -> Result<&mut Table> {
        if let Some(table) = self.tables.get_mut(&table_name) {
            Ok(table)
        } else {
            Err(SQLRiteError::General(String::from("Table not found.")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::parser::create::CreateQuery;
    use sqlparser::dialect::SQLiteDialect;
    use sqlparser::parser::{Parser};

    #[test]
    fn new_database_create_test() {
        let db_name = String::from("my_db");
        let db = Database::new(db_name.to_string());
        assert_eq!(db.db_name, db_name);
    }

    #[test]
    fn contains_table_test() {
        let db_name = String::from("my_db");
        let mut db = Database::new(db_name.to_string());

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
        let table_name = &create_query.table_name;
        db.tables.insert(table_name.to_string(), Table::new(create_query));

        assert!(db.contains_table("contacts".to_string()));
    }

    #[test]
    fn get_table_test() {
        let db_name = String::from("my_db");
        let mut db = Database::new(db_name.to_string());

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
        let table_name = &create_query.table_name;
        db.tables.insert(table_name.to_string(), Table::new(create_query));

        let table = db.get_table(String::from("contacts")).unwrap();
        assert_eq!(table.columns.len(), 4);

        let mut table = db.get_table_mut(String::from("contacts")).unwrap();
        table.last_rowid += 1;
        assert_eq!(table.columns.len(), 4); 
        assert_eq!(table.last_rowid, 1); 
    }

}