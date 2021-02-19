//! Error Types

use std::{
    error::Error, 
    fmt::{Display, Formatter},
    io, result,
};

use sqlparser::parser;

pub type Result<T> = result::Result<T, SQLRiteError>;

#[derive(Debug)]
pub enum SQLRiteError {
    NotImplemented(String),
    General(String),
    Internal(String),
    SqlError(parser::ParserError),
    IoError(io::Error),
}

impl<T> Into<Result<T>> for SQLRiteError {
    fn into(self) -> Result<T> {
        Err(self)
    }
}

pub fn sqlrite_error(message: &str) -> SQLRiteError {
    SQLRiteError::General(message.to_owned())
}

impl From<String> for SQLRiteError {
    fn from(e: String) -> Self {
        SQLRiteError::General(e)
    }
}

impl From<parser::ParserError> for SQLRiteError {
    fn from(e: parser::ParserError) -> Self {
        SQLRiteError::SqlError(e)
    }
}

impl From<io::Error> for SQLRiteError {
    fn from(e: io::Error) -> Self {
        SQLRiteError::IoError(e)
    }
}

impl Display for SQLRiteError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            SQLRiteError::NotImplemented(ref desc) => write!(f, "Not implemented: {}", desc),
            SQLRiteError::General(ref desc) => write!(f, "General error: {}", desc),
            SQLRiteError::SqlError(ref desc) => write!(f, "SQL error: {:?}", desc),
            SQLRiteError::IoError(ref desc) => write!(f, "IO error: {}", desc),
            SQLRiteError::Internal(desc) => write!(f, "Internal SQLRite error: {}", desc),
        }
    }
}

impl Error for SQLRiteError {}