//! Engine-side glue for the [`sqlrite-ask`](https://crates.io/crates/sqlrite-ask)
//! crate — natural-language → SQL.
//!
//! Compiled only when the `ask` feature is enabled (default-on for
//! the CLI binary, off for the WASM SDK and any
//! `default-features = false` library embedding).
//!
//! ## Why this lives in the engine, not in `sqlrite-ask`
//!
//! Earlier (v0.1.18) `sqlrite-ask` itself owned the `Connection`
//! integration — it imported `sqlrite-engine` and exposed
//! `ConnectionAskExt`. That worked for library callers, but when
//! the engine's REPL binary tried to depend on `sqlrite-ask` to
//! wire up the `.ask` meta-command we hit a hard cargo error:
//!
//! ```text
//! cyclic package dependency: package `sqlrite-ask` depends on itself.
//! Cycle: sqlrite-ask → sqlrite-engine → sqlrite-ask
//! ```
//!
//! Optional / feature-gated deps don't escape this — cargo's static
//! cycle detection counts every potential edge in the graph. The
//! structural fix was to flip the dep direction: keep `sqlrite-ask`
//! pure (operates on `&str` schemas), put the engine integration
//! here. The dep flow is now one-way: `sqlrite-engine[ask]` →
//! `sqlrite-ask`. No cycle.
//!
//! ## What's here
//!
//! - [`schema::dump_schema_for_database`] — walks `Database.tables`
//!   alphabetically, emits `CREATE TABLE … (…);` text the LLM grounds
//!   on. Determinism matters for prompt caching.
//! - [`ConnectionAskExt`] — extension trait adding `Connection::ask`
//!   that handles schema introspection + `sqlrite_ask::ask_with_schema`
//!   in one call.
//! - Free functions [`ask`] / [`ask_with_database`] /
//!   [`ask_with_provider`] / [`ask_with_database_and_provider`] —
//!   for callers who don't want to bring the trait into scope, or
//!   who hold a `&Database` directly (the REPL binary does this).

use sqlrite_ask::{
    AskConfig, AskError, AskResponse, Provider, ask_with_schema, ask_with_schema_and_provider,
};

use crate::Connection;
use crate::sql::db::database::Database;

pub mod schema;

/// Extension trait adding `Connection::ask` to
/// [`crate::Connection`]. Bring it into scope with
/// `use sqlrite::ConnectionAskExt;` (the engine re-exports it at
/// the crate root).
pub trait ConnectionAskExt {
    /// Generate SQL from a natural-language question.
    ///
    /// Internally: dump the schema, build the cache-friendly prompt,
    /// POST to the configured LLM provider, parse the JSON-shaped
    /// reply.
    ///
    /// ```no_run
    /// use sqlrite::{Connection, ConnectionAskExt};
    /// use sqlrite_ask::AskConfig;
    ///
    /// let conn = Connection::open("foo.sqlrite")?;
    /// let cfg  = AskConfig::from_env()?;          // SQLRITE_LLM_API_KEY etc.
    /// let resp = conn.ask("how many users are over 30?", &cfg)?;
    /// println!("{}", resp.sql);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    fn ask(&self, question: &str, config: &AskConfig) -> Result<AskResponse, AskError>;
}

impl ConnectionAskExt for Connection {
    fn ask(&self, question: &str, config: &AskConfig) -> Result<AskResponse, AskError> {
        ask(self, question, config)
    }
}

/// Free-function form of [`ConnectionAskExt::ask`]. Equivalent —
/// pick whichever shape reads better at the call site.
pub fn ask(conn: &Connection, question: &str, config: &AskConfig) -> Result<AskResponse, AskError> {
    ask_with_database(conn.database(), question, config)
}

/// Same as [`ask`], but takes the engine's `&Database` directly.
///
/// Used by the REPL binary's `.ask` meta-command, which holds a
/// `&mut Database` rather than a `&Connection`.
pub fn ask_with_database(
    db: &Database,
    question: &str,
    config: &AskConfig,
) -> Result<AskResponse, AskError> {
    let schema_dump = schema::dump_schema_for_database(db);
    ask_with_schema(&schema_dump, question, config)
}

/// Lower-level entry — same flow as [`ask`] but you supply the
/// provider. For test harnesses + advanced callers driving custom
/// backends.
pub fn ask_with_provider<P: Provider>(
    conn: &Connection,
    question: &str,
    config: &AskConfig,
    provider: &P,
) -> Result<AskResponse, AskError> {
    ask_with_database_and_provider(conn.database(), question, config, provider)
}

/// Lower-level entry taking `&Database` and a provider. Canonical
/// inner function — the others reduce to this one.
pub fn ask_with_database_and_provider<P: Provider>(
    db: &Database,
    question: &str,
    config: &AskConfig,
    provider: &P,
) -> Result<AskResponse, AskError> {
    let schema_dump = schema::dump_schema_for_database(db);
    ask_with_schema_and_provider(&schema_dump, question, config, provider)
}
