//! SQLRite Journal — a markdown daily-notes desktop app showcasing
//! the engine's `Connection` API, Phase 8 BM25 full-text search, and
//! (when the `ask` feature is on) natural-language `Connection::ask`.
//!
//! Architecture in one paragraph: the Rust side owns an
//! `Arc<Mutex<Connection>>` in [`tauri::State`]. Every `#[tauri::command]`
//! takes the mutex, runs SQL via the public `Connection` /
//! `Statement` API, serialises the result, and returns. The Svelte
//! frontend never touches the DB directly — there's no fs or shell
//! capability in `capabilities/default.json`, so the IPC boundary is
//! the only path into the engine.
//!
//! **Why a single mutex, not BEGIN CONCURRENT.** Concurrent writes
//! (SQLR-22 / Phase 10/11) shipped; the engine can run multiple
//! `BEGIN CONCURRENT` transactions in parallel. A single-user
//! journaling desktop is the wrong shape for that — there's exactly
//! one writer (the user). Serialising every command through one
//! `Connection` mutex means commands compose without retry loops,
//! and "user mashes save twice while another command is running"
//! becomes "the second command waits ~ms for the first". Simple,
//! correct, no torn writes.

// Prevent a second console window on Windows in release mode.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod journal;
mod settings;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use sqlrite::Connection;
use tauri::{Manager, State};

use journal::JournalDb;
use settings::{AskSettings, AskSettingsDto, AskSettingsUpdate};

/// Holds the single active database + the path to the on-disk
/// `settings.json`. The settings path is immutable for the app's
/// lifetime, so it lives outside any mutex; reads/writes go through
/// `AskSettings::load` / `AskSettings::save` which touch disk directly.
struct AppState {
    db: Arc<Mutex<JournalDb>>,
    settings_path: PathBuf,
}

#[derive(Serialize)]
struct OpenedDb {
    path: String,
    entry_count: i64,
}

/// Opens an existing `.sqlrite` journal file (creating + migrating one
/// if absent) and swaps it in as the active database for the app.
#[tauri::command]
fn open_database(path: String, state: State<'_, AppState>) -> Result<OpenedDb, String> {
    let p = PathBuf::from(&path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create_dir_all: {e}"))?;
    }
    let conn = Connection::open(&p).map_err(|e| format!("open: {e}"))?;
    let mut db = JournalDb::with_connection(conn).map_err(stringify_err)?;
    let entry_count = db.count_entries().map_err(stringify_err)?;
    let mut locked = state
        .db
        .lock()
        .map_err(|e| format!("mutex poisoned: {e}"))?;
    *locked = db;
    Ok(OpenedDb {
        path: p.display().to_string(),
        entry_count,
    })
}

#[tauri::command]
fn current_db_path(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let locked = state
        .db
        .lock()
        .map_err(|e| format!("mutex poisoned: {e}"))?;
    Ok(locked.path().map(|p| p.display().to_string()))
}

#[tauri::command]
fn list_entries(
    tag: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<journal::EntrySummary>, String> {
    let mut locked = state
        .db
        .lock()
        .map_err(|e| format!("mutex poisoned: {e}"))?;
    locked.list_entries(tag.as_deref()).map_err(stringify_err)
}

#[tauri::command]
fn get_entry(id: i64, state: State<'_, AppState>) -> Result<journal::Entry, String> {
    let mut locked = state
        .db
        .lock()
        .map_err(|e| format!("mutex poisoned: {e}"))?;
    locked.get_entry(id).map_err(stringify_err)
}

#[tauri::command]
fn create_entry(
    date: String,
    title: String,
    content: String,
    tags: Vec<String>,
    state: State<'_, AppState>,
) -> Result<i64, String> {
    let mut locked = state
        .db
        .lock()
        .map_err(|e| format!("mutex poisoned: {e}"))?;
    locked
        .create_entry(&date, &title, &content, &tags)
        .map_err(stringify_err)
}

#[tauri::command]
fn update_entry(
    id: i64,
    date: String,
    title: String,
    content: String,
    tags: Vec<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut locked = state
        .db
        .lock()
        .map_err(|e| format!("mutex poisoned: {e}"))?;
    locked
        .update_entry(id, &date, &title, &content, &tags)
        .map_err(stringify_err)
}

#[tauri::command]
fn delete_entry(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    let mut locked = state
        .db
        .lock()
        .map_err(|e| format!("mutex poisoned: {e}"))?;
    locked.delete_entry(id).map_err(stringify_err)
}

#[tauri::command]
fn list_tags(state: State<'_, AppState>) -> Result<Vec<journal::TagSummary>, String> {
    let mut locked = state
        .db
        .lock()
        .map_err(|e| format!("mutex poisoned: {e}"))?;
    locked.list_tags().map_err(stringify_err)
}

#[tauri::command]
fn search_entries(
    query: String,
    state: State<'_, AppState>,
) -> Result<Vec<journal::SearchHit>, String> {
    let mut locked = state
        .db
        .lock()
        .map_err(|e| format!("mutex poisoned: {e}"))?;
    locked.search(&query).map_err(stringify_err)
}

#[tauri::command]
fn stats(state: State<'_, AppState>) -> Result<journal::Stats, String> {
    let mut locked = state
        .db
        .lock()
        .map_err(|e| format!("mutex poisoned: {e}"))?;
    locked.stats().map_err(stringify_err)
}

/// Natural-language → SQL → result rows.
///
/// Flow: lock the connection (schema-dump-consistent with what the
/// user would see), call `Connection::ask`, validate the returned SQL
/// is read-only (SELECT / WITH only), execute it, and pack `{ sql,
/// explanation, columns, rows }` for the UI.
///
/// **API key.** Read from the parent process's environment
/// (`SQLRITE_LLM_API_KEY`). It never crosses into the webview — the
/// Rust backend is the only thing that ever sees it.
#[cfg(feature = "ask")]
#[tauri::command]
fn ask_journal(question: String, state: State<'_, AppState>) -> Result<journal::AskResult, String> {
    let saved = AskSettings::load(&state.settings_path);
    let cfg = settings::build_ask_config(&saved)?;
    let mut locked = state
        .db
        .lock()
        .map_err(|e| format!("mutex poisoned: {e}"))?;
    locked.ask(&question, &cfg).map_err(stringify_err)
}

#[tauri::command]
fn get_ask_settings(state: State<'_, AppState>) -> Result<AskSettingsDto, String> {
    Ok(AskSettings::load(&state.settings_path).to_dto())
}

#[tauri::command]
fn update_ask_settings(
    update: AskSettingsUpdate,
    state: State<'_, AppState>,
) -> Result<AskSettingsDto, String> {
    let mut current = AskSettings::load(&state.settings_path);
    current.apply_update(update);
    current
        .save(&state.settings_path)
        .map_err(|e| format!("save settings: {e}"))?;
    Ok(current.to_dto())
}

#[tauri::command]
fn export_db(dest: String, state: State<'_, AppState>) -> Result<(), String> {
    let locked = state
        .db
        .lock()
        .map_err(|e| format!("mutex poisoned: {e}"))?;
    locked
        .export_db(&PathBuf::from(dest))
        .map_err(stringify_err)
}

#[tauri::command]
fn export_markdown(
    dir: String,
    state: State<'_, AppState>,
) -> Result<journal::ExportSummary, String> {
    let mut locked = state
        .db
        .lock()
        .map_err(|e| format!("mutex poisoned: {e}"))?;
    locked
        .export_markdown(&PathBuf::from(dir))
        .map_err(stringify_err)
}

fn stringify_err(e: journal::JournalError) -> String {
    e.to_string()
}

fn main() {
    // Cargo's feature-cfg on a `tauri::generate_handler![…]` literal
    // requires two near-identical builders, otherwise the macro
    // expansion captures whichever branch the cfg evaluates first.
    // Splitting at the .invoke_handler call is the cleanest shape.
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // On startup we open (or create) the default journal file
            // under the OS's app-data directory. The user can later swap
            // it via the Open… dialog. Failing this is non-fatal in
            // theory — but we treat it as setup-fatal: there's no
            // sensible "no DB" UI state for a journaling app to start
            // in. The frontend can still call open_database to switch
            // files later.
            let app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("app_data_dir: {e}"))?;
            std::fs::create_dir_all(&app_data_dir)?;
            let default_path = app_data_dir.join("journal.sqlrite");
            let conn = Connection::open(&default_path)?;
            let db = JournalDb::with_connection(conn)?;
            let settings_path = settings::settings_path(&app_data_dir);
            app.manage(AppState {
                db: Arc::new(Mutex::new(db)),
                settings_path,
            });
            Ok(())
        });

    #[cfg(feature = "ask")]
    let builder = builder.invoke_handler(tauri::generate_handler![
        open_database,
        current_db_path,
        list_entries,
        get_entry,
        create_entry,
        update_entry,
        delete_entry,
        list_tags,
        search_entries,
        stats,
        ask_journal,
        get_ask_settings,
        update_ask_settings,
        export_db,
        export_markdown,
    ]);
    #[cfg(not(feature = "ask"))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        open_database,
        current_db_path,
        list_entries,
        get_entry,
        create_entry,
        update_entry,
        delete_entry,
        list_tags,
        search_entries,
        stats,
        get_ask_settings,
        update_ask_settings,
        export_db,
        export_markdown,
    ]);

    builder
        .run(tauri::generate_context!())
        .expect("error while running sqlrite-journal");
}
