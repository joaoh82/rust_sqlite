use std::collections::{BTreeMap, HashMap};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub enum DataType {
    Int,
    Str,
    Float,
    Bool,
    Invalid,
}

impl DataType {
    pub fn new(cmd: String) -> DataType {
        match cmd.to_lowercase().as_ref() {
            "int" => DataType::Int,
            "string" => DataType::Str,
            "float" => DataType::Float,
            "double" => DataType::Float,
            "bool" => DataType::Bool,
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
            DataType::Int => f.write_str("Int"),
            DataType::Str => f.write_str("Str"),
            DataType::Float => f.write_str("Float"),
            DataType::Bool => f.write_str("Boolean"),
            DataType::Invalid => f.write_str("Invalid"),
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct ColumnHeader {
    pub name: String,
    pub datatype: DataType,
    pub is_indexed: bool,
    pub index: ColumnIndex,
    pub is_primary_key: bool,
}

impl ColumnHeader {
    pub fn new(name: String, datatype: String, is_primary_key: bool) -> ColumnHeader {
        let dt = DataType::new(datatype);
        let index = match dt {
            DataType::Int => ColumnIndex::Int(BTreeMap::new()),
            DataType::Float => ColumnIndex::None,
            DataType::Str => ColumnIndex::Str(BTreeMap::new()),
            DataType::Bool => ColumnIndex::Bool(BTreeMap::new()),
            DataType::Invalid => ColumnIndex::None,
        };

        ColumnHeader {
            name: name,
            datatype: dt,
            is_indexed: if is_primary_key { true } else { false },
            index,
            is_primary_key,
        }
    }

    pub fn get_mut_index(&mut self) -> &mut ColumnIndex {
        return &mut self.index;
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub enum ColumnData {
    Int(Vec<i32>),
    Str(Vec<String>),
    Float(Vec<f32>),
    Bool(Vec<bool>),
    None,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct Table {
    pub columns: Vec<ColumnHeader>,
    pub name: String,
    pub rows: BTreeMap<String, ColumnData>,
}