//! SQLRite — a small SQLite clone written in Rust, as a library.
//!
//! The REPL binary (`src/main.rs`) uses this library. The Tauri desktop
//! app under `desktop/src-tauri/` uses it too. Future consumers — a
//! `Connection` API split, a WASM build, a C FFI shim — all grow out of
//! this same surface.
//!
//! **Scope right now.** The library surfaces two layers:
//!
//! **1. High-level public API (Phase 5a).** The shape most callers
//! want — stable, documented, and the same surface that the C FFI
//! shim (Phase 5b) and every language SDK (Python / Node / Go /
//! WASM) binds against:
//!
//! - [`Connection`] — open a file, in-memory DB, or read-only view
//! - [`Statement`] — prepared SQL with typed row iteration
//! - [`Rows`] / [`Row`] / [`OwnedRow`] — streaming typed result rows
//! - [`FromValue`] — pluggable row-to-Rust conversion (`i64`,
//!   `f64`, `String`, `bool`, `Option<T>`, plus raw `Value`)
//!
//! ```no_run
//! use sqlrite::Connection;
//! let mut conn = Connection::open("foo.sqlrite")?;
//! conn.execute("INSERT INTO users (name) VALUES ('alice')")?;
//! let mut stmt = conn.prepare("SELECT id, name FROM users")?;
//! let mut rows = stmt.query()?;
//! while let Some(row) = rows.next()? {
//!     let (id, name): (i64, String) = (row.get(0)?, row.get(1)?);
//!     println!("{id}: {name}");
//! }
//! # Ok::<(), sqlrite::SQLRiteError>(())
//! ```
//!
//! **2. Lower-level engine surface.** Accessible via `sqlrite::sql::…`
//! for the REPL, the Tauri desktop app, and the engine's own tests:
//!
//! - [`Database`] — the in-memory state owning all tables
//! - [`process_command`] — parse + execute one SQL statement (returns
//!   the rendered status string the REPL prints)
//! - [`open_database`] / [`open_database_read_only`] / [`save_database`] —
//!   file I/O primitives (shared-lock read-only variant added in Phase 4e)
//! - [`AccessMode`] — the enum driving exclusive vs shared lock acquisition
//! - [`Result`] / [`SQLRiteError`] — the error surface
//!
//! Lower-level modules (`sql::pager`, `sql::executor`, etc.) remain
//! accessible through `sqlrite::sql::…` for tests and tooling, but
//! aren't considered public API — their shapes will change as Phase 4
//! (WAL + locks) and Phase 5 (cursor / lazy-load) land.

#[macro_use]
extern crate prettytable;

// `sqlrite::ask` is always available — its `schema` submodule (the
// CREATE TABLE dump used to ground the LLM's prompt) is pure-engine
// and useful even for builds that don't enable the `ask` feature.
// The `ConnectionAskExt` trait + `ask` / `ask_with_database`
// helpers inside the module are gated under the feature, since
// they pull in `sqlrite-ask`. The WASM SDK uses
// `sqlrite::ask::schema::dump_schema_for_database` to introspect a
// browser-side `Connection` without needing the HTTP transport.
pub mod ask;
pub mod connection;
pub mod error;
pub mod sql;

// Phase 5a public API.
pub use connection::{Connection, FromValue, OwnedRow, Row, Rows, Statement};

// Phase 7g.2: re-export the `Connection::ask` extension trait + the
// public `sqlrite_ask` types at the crate root when the `ask`
// feature is on. Lets callers write `use sqlrite::{Connection,
// ConnectionAskExt, ask::AskConfig};` rather than dragging in
// `sqlrite-ask` as a separate dep just to reach `AskConfig`. Only
// available with the `ask` feature.
#[cfg(feature = "ask")]
pub use ask::ConnectionAskExt;

// Underlying types useful to public-API callers (Value for typed
// comparisons / untyped `row.get::<Value>(0)` access).
pub use sql::db::table::Value;

// Lower-level engine surface — public for the REPL, Tauri app, and
// the engine's own tests. Not part of the stable public contract;
// layered above these for most users is `Connection`.
pub use error::{Result, SQLRiteError};
pub use sql::db::database::Database;
pub use sql::pager::{
    AccessMode, MASTER_TABLE_NAME, open_database, open_database_read_only, open_database_with_mode,
    save_database,
};
pub use sql::process_command;

// Re-export sqlparser so downstream crates (the Tauri desktop app, the
// eventual WASM bindings) can reach into the AST without pulling a
// second copy as a direct dep. Using `pub extern crate` so it's
// namespaced under `sqlrite::sqlparser::…`.
pub use ::sqlparser;
