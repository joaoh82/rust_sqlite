use thiserror::Error;

use std::result;

use sqlparser::parser::ParserError;

/// This is a type that encapsulated the `std::result` with the enum `SQLRiteError`
/// and makes function signatures easier to read.
pub type Result<T> = result::Result<T, SQLRiteError>;

/// SQLRiteError is an enum with all the standardized errors available for returning
///
#[derive(Error, Debug)]
pub enum SQLRiteError {
    #[error("Not Implemented error: {0}")]
    NotImplemented(String),
    #[error("General error: {0}")]
    General(String),
    #[error("Internal error: {0}")]
    Internal(String),
    #[error("Unknown command error: {0}")]
    UnknownCommand(String),
    #[error("SQL error: {0:?}")]
    SqlError(#[from] ParserError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// Phase 11.4 — `BEGIN CONCURRENT` commit hit a write-write
    /// conflict. Some other transaction committed a newer version
    /// of a row in this transaction's write-set after this
    /// transaction's `begin_ts`. Caller should `ROLLBACK` (already
    /// implicitly performed) and retry the transaction with a
    /// fresh `begin_ts`.
    #[error("Busy: {0}")]
    Busy(String),
    /// Phase 11.4 — same shape as [`SQLRiteError::Busy`] but
    /// surfaces the snapshot-isolation specific case: a row in
    /// the read-set changed under us. Distinguished from `Busy`
    /// so SDKs can map it to a per-language exception that the
    /// caller's retry helper recognizes (mirrors Turso /
    /// libSQL's `BUSY` vs `BUSY_SNAPSHOT` split). v0 only emits
    /// `Busy` from the write-write validation loop; the
    /// read-anomaly variant is reserved for the snapshot-read
    /// integration that follows.
    #[error("BusySnapshot: {0}")]
    BusySnapshot(String),
}

// `std::io::Error` has no `PartialEq`, so we implement one by value-of-message.
// Used by existing tests that compare error variants.
impl PartialEq for SQLRiteError {
    fn eq(&self, other: &Self) -> bool {
        use SQLRiteError::*;
        match (self, other) {
            (NotImplemented(a), NotImplemented(b)) => a == b,
            (General(a), General(b)) => a == b,
            (Internal(a), Internal(b)) => a == b,
            (UnknownCommand(a), UnknownCommand(b)) => a == b,
            (SqlError(a), SqlError(b)) => format!("{a:?}") == format!("{b:?}"),
            (Io(a), Io(b)) => a.kind() == b.kind() && a.to_string() == b.to_string(),
            (Busy(a), Busy(b)) => a == b,
            (BusySnapshot(a), BusySnapshot(b)) => a == b,
            _ => false,
        }
    }
}

impl SQLRiteError {
    /// Phase 11.4 — true for `Busy` and `BusySnapshot`. SDK retry
    /// helpers branch on this rather than matching the variants
    /// individually so adding a third "retryable" variant later
    /// doesn't break callers.
    pub fn is_retryable(&self) -> bool {
        matches!(self, SQLRiteError::Busy(_) | SQLRiteError::BusySnapshot(_))
    }
}

/// Returns SQLRiteError::General error from String
#[allow(dead_code)]
pub fn sqlrite_error(message: &str) -> SQLRiteError {
    SQLRiteError::General(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlrite_error_test() {
        let input = String::from("test error");
        let expected = SQLRiteError::General("test error".to_string());

        let result = sqlrite_error(&input);
        assert_eq!(result, expected);
    }

    #[test]
    fn sqlrite_display_not_implemented_test() {
        let error_string = String::from("Feature not implemented.");
        let input = SQLRiteError::NotImplemented(error_string.clone());

        let expected = format!("Not Implemented error: {}", error_string);
        let result = format!("{}", input);
        assert_eq!(result, expected);
    }

    #[test]
    fn sqlrite_display_general_test() {
        let error_string = String::from("General error.");
        let input = SQLRiteError::General(error_string.clone());

        let expected = format!("General error: {}", error_string);
        let result = format!("{}", input);
        assert_eq!(result, expected);
    }

    #[test]
    fn sqlrite_display_internal_test() {
        let error_string = String::from("Internet error.");
        let input = SQLRiteError::Internal(error_string.clone());

        let expected = format!("Internal error: {}", error_string);
        let result = format!("{}", input);
        assert_eq!(result, expected);
    }

    #[test]
    fn sqlrite_display_sqlrite_test() {
        let error_string = String::from("SQL error.");
        let input = SQLRiteError::SqlError(ParserError::ParserError(error_string.clone()));

        let expected = format!("SQL error: ParserError(\"{}\")", error_string);
        let result = format!("{}", input);
        assert_eq!(result, expected);
    }

    #[test]
    fn sqlrite_unknown_test() {
        let error_string = String::from("Unknown error.");
        let input = SQLRiteError::UnknownCommand(error_string.clone());

        let expected = format!("Unknown command error: {}", error_string);
        let result = format!("{}", input);
        assert_eq!(result, expected);
    }
}
