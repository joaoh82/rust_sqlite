use std::collections::{BTreeMap, HashMap};
use serde::{Deserialize, Serialize};
use std::fmt;

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

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct Table {
    // pub columns: Vec<ColumnHeader>,
    pub name: String,
    // pub rows: BTreeMap<String, ColumnData>,
}