use sqlparser::ast::{Expr as ASTNode, *};
use crate::error::{Result};

/// Responsible for parse each os the SQL Statements Components
pub fn parse_statement(statement: Statement) -> Result<Statement> {
    unimplemented!();
}