//! SQLRite desktop — Tauri 2 shell around the sqlrite engine.
//!
//! The app owns a single `Database` in shared state behind a `Mutex`.
//! Every Tauri command borrows that mutex, runs against the engine, and
//! returns a serde-serializable result to the frontend. There's no
//! session concept beyond "the currently open DB" — matches the REPL's
//! model.
//!
//! **Why clone results instead of streaming.** For Phase 2.5 MVP every
//! command returns an owned `QueryResult` (or similar). A streaming
//! cursor API is on the Phase 5 roadmap and will pair with the library
//! crate's `Connection` / `Statement` surface; the desktop app will
//! plumb it through later.

// Prevent a second console window on Windows in release mode.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use sqlrite::sql::db::table::Value;
use sqlrite::{Database, SQLRiteError, process_command};
use tauri::{Manager, State};

/// Holds the single active database for the app. A `None` means "no
/// database open yet" — the frontend should nudge the user toward
/// `.open`.
struct AppState {
    db: Mutex<Database>,
}

/// Mirrors `SecondaryIndex` enough for the UI's sidebar — just name,
/// column, uniqueness flag. The full index data stays on the backend.
#[derive(Serialize)]
struct ColumnInfo {
    name: String,
    datatype: String,
    is_pk: bool,
    is_unique: bool,
    not_null: bool,
}

#[derive(Serialize)]
struct TableInfo {
    name: String,
    columns: Vec<ColumnInfo>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CommandResult {
    /// SELECT-style command: column headers plus rows of stringly-typed
    /// values (the frontend handles its own rendering; shipping the
    /// display-string form keeps the wire format simple).
    Rows {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    /// Write-style command or meta output — just a status string.
    Status { message: String },
}

fn engine_err<E: std::fmt::Display>(e: E) -> String {
    format!("{e}")
}

#[tauri::command]
fn open_database(path: String, state: State<'_, AppState>) -> Result<TableInfo, String> {
    let p = PathBuf::from(&path);
    let db_name = p
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("db")
        .to_string();
    let loaded = if p.exists() {
        sqlrite::open_database(&p, db_name).map_err(engine_err)?
    } else {
        let mut fresh = Database::new(db_name);
        fresh.source_path = Some(p.clone());
        sqlrite::save_database(&mut fresh, &p).map_err(engine_err)?;
        fresh
    };
    let mut locked = state.db.lock().map_err(engine_err)?;
    *locked = loaded;
    Ok(snapshot_schema(&locked, &path))
}

/// Persists the current in-memory database to `path` and adopts that
/// file as the new backing store — so subsequent writes auto-save there.
/// Use case: the user started in transient in-memory mode, built up
/// some schema, and now wants to keep it.
///
/// Differs from `open_database`, which wouldn't carry the in-memory
/// tables forward: that one reloads from disk, overwriting whatever
/// was live.
///
/// Differs from the REPL's `.save FILE` too: `.save` writes to a
/// destination without switching the active source. For the desktop
/// app, "Save As…" conventionally means "switch to this file", so
/// we do the switch.
#[tauri::command]
fn save_database_as(path: String, state: State<'_, AppState>) -> Result<TableInfo, String> {
    let p = PathBuf::from(&path);
    let db_name = p
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("db")
        .to_string();
    let mut locked = state.db.lock().map_err(engine_err)?;

    // 1. Flush the in-memory state to the new path. save_database writes
    //    a full file and drops the pager it used; we reopen below to
    //    attach a fresh pager that's now tied to this path.
    sqlrite::save_database(&mut *locked, &p).map_err(engine_err)?;

    // 2. Reopen: populates source_path + pager so every subsequent
    //    committing statement auto-saves to this file.
    let adopted = sqlrite::open_database(&p, db_name).map_err(engine_err)?;
    *locked = adopted;

    Ok(snapshot_schema(&locked, &path))
}

#[tauri::command]
fn list_tables(state: State<'_, AppState>) -> Result<Vec<TableInfo>, String> {
    let locked = state.db.lock().map_err(engine_err)?;
    let mut names: Vec<&String> = locked.tables.keys().collect();
    names.sort();
    Ok(names.into_iter().map(|n| table_info(&locked, n)).collect())
}

#[tauri::command]
fn table_rows(
    name: String,
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<CommandResult, String> {
    let locked = state.db.lock().map_err(engine_err)?;
    let table = locked.get_table(name.clone()).map_err(engine_err)?;
    let columns = table.column_names();
    let rowids = {
        let mut rs = table.rowids();
        if let Some(n) = limit {
            rs.truncate(n);
        }
        rs
    };
    let rows: Vec<Vec<String>> = rowids
        .into_iter()
        .map(|rowid| {
            columns
                .iter()
                .map(|col| match table.get_value(col, rowid) {
                    Some(v) => display_value(&v),
                    None => String::new(),
                })
                .collect()
        })
        .collect();
    Ok(CommandResult::Rows { columns, rows })
}

#[tauri::command]
fn execute_sql(sql: String, state: State<'_, AppState>) -> Result<CommandResult, String> {
    let mut locked = state.db.lock().map_err(engine_err)?;
    let message = process_command(&sql, &mut locked).map_err(|e: SQLRiteError| engine_err(e))?;

    // If this was a SELECT, the engine already `print!`ed the rendered
    // table to stdout before returning. For the desktop app we want the
    // data shipped to the frontend, so we detect SELECT and re-run it
    // against the in-memory `Table` to build structured rows.
    // Cheap heuristic: the status string starts with "SELECT Statement".
    if message.starts_with("SELECT Statement") {
        if let Some(payload) = extract_last_select(&sql, &locked) {
            return Ok(payload);
        }
    }
    Ok(CommandResult::Status { message })
}

/// Re-runs the SELECT against the in-memory state to produce structured
/// rows for the UI. This intentionally duplicates the executor's scan
/// logic because the executor only returns a status message today;
/// Phase 5's cursor refactor will let us skip this step.
fn extract_last_select(sql: &str, db: &Database) -> Option<CommandResult> {
    use sqlrite::sqlparser::ast::{Expr, SelectItem, SetExpr, Statement, TableFactor};
    use sqlrite::sqlparser::dialect::SQLiteDialect;
    use sqlrite::sqlparser::parser::Parser;

    let dialect = SQLiteDialect {};
    let ast = Parser::parse_sql(&dialect, sql).ok()?;
    let stmt = ast.into_iter().next()?;
    let Statement::Query(query) = stmt else {
        return None;
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    let from = select.from.first()?;
    let table_name = match &from.relation {
        TableFactor::Table { name, .. } => name.to_string(),
        _ => return None,
    };
    let table = db.get_table(table_name).ok()?;
    let columns: Vec<String> = if select.projection.len() == 1
        && matches!(select.projection[0], SelectItem::Wildcard(_))
    {
        table.column_names()
    } else {
        select
            .projection
            .iter()
            .filter_map(|item| match item {
                SelectItem::UnnamedExpr(Expr::Identifier(i)) => Some(i.value.clone()),
                _ => None,
            })
            .collect()
    };
    let rows: Vec<Vec<String>> = table
        .rowids()
        .into_iter()
        .map(|rowid| {
            columns
                .iter()
                .map(|c| match table.get_value(c, rowid) {
                    Some(v) => display_value(&v),
                    None => String::new(),
                })
                .collect()
        })
        .collect();
    Some(CommandResult::Rows { columns, rows })
}

fn display_value(v: &Value) -> String {
    match v {
        Value::Integer(n) => n.to_string(),
        Value::Text(s) => s.clone(),
        Value::Real(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "NULL".to_string(),
    }
}

fn table_info(db: &Database, name: &str) -> TableInfo {
    let table = db
        .get_table(name.to_string())
        .expect("caller checked via db.tables.keys()");
    TableInfo {
        name: table.tb_name.clone(),
        columns: table
            .columns
            .iter()
            .map(|c| ColumnInfo {
                name: c.column_name.clone(),
                datatype: format!("{}", c.datatype),
                is_pk: c.is_pk,
                is_unique: c.is_unique,
                not_null: c.not_null,
            })
            .collect(),
    }
}

fn snapshot_schema(db: &Database, path: &str) -> TableInfo {
    // The "opened" result hands back a pseudo-TableInfo carrying the
    // file path in the name field — the frontend uses it mostly as a
    // success signal and to label the sidebar.
    TableInfo {
        name: path.to_string(),
        columns: db
            .tables
            .keys()
            .map(|n| ColumnInfo {
                name: n.clone(),
                datatype: "table".into(),
                is_pk: false,
                is_unique: false,
                not_null: false,
            })
            .collect(),
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            app.manage(AppState {
                db: Mutex::new(Database::new("scratch".into())),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            open_database,
            save_database_as,
            list_tables,
            table_rows,
            execute_sql
        ])
        .run(tauri::generate_context!())
        .expect("error while running sqlrite-desktop");
}
