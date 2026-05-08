//! SQLRite SQL dialect.
//!
//! Wraps sqlparser's `SQLiteDialect` so we get every SQLite-specific
//! tokenizer/parser quirk (delimited identifiers, NOTNULL operator,
//! `LIMIT a, b`, `MATCH`/`REGEXP` infix, …) and overrides only what we
//! need for SQLRite's vector extensions:
//!
//! - `supports_create_index_with_clause = true` — lets the parser
//!   accept `CREATE INDEX … USING hnsw (col) WITH (metric = 'cosine')`.
//!   sqlparser's `SQLiteDialect` returns `false` from this method, so
//!   the WITH clause would otherwise be parked in `index_options` (or
//!   error). The PostgreSQL dialect already turns it on; we copy that
//!   behaviour here without taking the rest of the pgsql parser
//!   divergences.
//!
//! Add new dialect overrides here as the surface grows; everything not
//! explicitly listed defers to the base SQLite dialect.
use sqlparser::ast::{Expr, Statement};
use sqlparser::dialect::{Dialect, SQLiteDialect};
use sqlparser::parser::{Parser, ParserError};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SqlriteDialect {
    inner: SQLiteDialect,
}

impl SqlriteDialect {
    pub const fn new() -> Self {
        Self {
            inner: SQLiteDialect {},
        }
    }
}

impl Dialect for SqlriteDialect {
    fn is_delimited_identifier_start(&self, ch: char) -> bool {
        self.inner.is_delimited_identifier_start(ch)
    }

    fn identifier_quote_style(&self, identifier: &str) -> Option<char> {
        self.inner.identifier_quote_style(identifier)
    }

    fn is_identifier_start(&self, ch: char) -> bool {
        self.inner.is_identifier_start(ch)
    }

    fn is_identifier_part(&self, ch: char) -> bool {
        self.inner.is_identifier_part(ch)
    }

    fn supports_filter_during_aggregation(&self) -> bool {
        self.inner.supports_filter_during_aggregation()
    }

    fn supports_start_transaction_modifier(&self) -> bool {
        self.inner.supports_start_transaction_modifier()
    }

    fn supports_in_empty_list(&self) -> bool {
        self.inner.supports_in_empty_list()
    }

    fn supports_limit_comma(&self) -> bool {
        self.inner.supports_limit_comma()
    }

    fn supports_asc_desc_in_column_definition(&self) -> bool {
        self.inner.supports_asc_desc_in_column_definition()
    }

    fn supports_dollar_placeholder(&self) -> bool {
        self.inner.supports_dollar_placeholder()
    }

    fn supports_notnull_operator(&self) -> bool {
        self.inner.supports_notnull_operator()
    }

    fn parse_statement(&self, parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
        self.inner.parse_statement(parser)
    }

    fn parse_infix(
        &self,
        parser: &mut Parser,
        expr: &Expr,
        precedence: u8,
    ) -> Option<Result<Expr, ParserError>> {
        self.inner.parse_infix(parser, expr, precedence)
    }

    /// SQLRite-specific extension: `CREATE INDEX … USING hnsw (col)
    /// WITH (metric = 'cosine')` is the canonical way to pick a
    /// non-L2 distance metric for an HNSW index. See
    /// `docs/supported-sql.md` and `try_hnsw_probe`.
    fn supports_create_index_with_clause(&self) -> bool {
        true
    }
}
