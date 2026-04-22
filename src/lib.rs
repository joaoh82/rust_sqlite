//! SQLRite — a small SQLite clone written in Rust, as a library.
//!
//! The REPL binary (`src/main.rs`) uses this library. The Tauri desktop
//! app under `desktop/src-tauri/` uses it too. Future consumers — a
//! `Connection` API split, a WASM build, a C FFI shim — all grow out of
//! this same surface.
//!
//! **Scope right now.** The library re-exports only what an external
//! consumer needs to open a database and run statements:
//!
//! - [`Database`] — the in-memory state owning all tables
//! - [`process_command`] — parse + execute one SQL statement
//! - [`open_database`] / [`save_database`] — read from / write to a `.sqlrite` file
//! - [`Result`] / [`SQLRiteError`] — the error surface
//!
//! Lower-level modules (`sql::pager`, `sql::executor`, etc.) remain
//! accessible through `sqlrite::sql::…` for tests and tooling, but
//! aren't considered public API — their shapes will change as Phase 4
//! (WAL + locks) and Phase 5 (cursor / lazy-load) land.

#[macro_use]
extern crate prettytable;

pub mod error;
pub mod sql;

pub use error::{Result, SQLRiteError};
pub use sql::db::database::Database;
pub use sql::pager::{MASTER_TABLE_NAME, open_database, save_database};
pub use sql::process_command;
