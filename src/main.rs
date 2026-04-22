//! REPL binary — the `sqlrite` CLI.
//!
//! Thin wrapper around the [`sqlrite`] library: rustyline for input, the
//! engine for parsing + execution + persistence.

mod meta_command;
mod repl;

use std::path::PathBuf;

use meta_command::handle_meta_command;
use repl::{CommandType, REPLHelper, get_command_type, get_config};
use sqlrite::{Database, process_command};

use rustyline::Editor;
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;

use clap::{Arg, Command, crate_authors, crate_name, crate_version};

const ABOUT_SHORT: &str = "A small SQLite-like embedded database and REPL, written in Rust.";

const ABOUT_LONG: &str = "\
A small SQLite-like embedded database and REPL, written in Rust.

Passing a FILE argument is equivalent to launching the REPL and then running
`.open FILE` — existing files are loaded, missing files are created fresh.
Without a FILE the REPL starts in transient in-memory mode.

Once in the REPL, meta commands start with a dot:

  .help                  Show the meta-command list
  .open <FILENAME>       Open (or create) a .sqlrite database file
  .save <FILENAME>       Write the current DB to FILENAME (rarely needed —
                         once .open is in play, every write auto-saves)
  .tables                List tables in the current database
  .exit                  Quit

Supported SQL: CREATE TABLE / CREATE [UNIQUE] INDEX / INSERT / SELECT /
UPDATE / DELETE with WHERE, ORDER BY, LIMIT, arithmetic (+ - * / %),
logical (AND / OR / NOT), string concat (||), and NULL-aware comparisons.
Index-probe fast path activates for `WHERE col = literal` on indexed
columns.

For the full reference see docs/usage.md; for end-to-end testing walk
through docs/smoke-test.md.";

fn main() -> rustyline::Result<()> {
    env_logger::init();

    let matches = Command::new(crate_name!())
        .version(crate_version!())
        .author(crate_authors!())
        .about(ABOUT_SHORT)
        .long_about(ABOUT_LONG)
        .arg(
            Arg::new("FILE")
                .help("Path to a .sqlrite database file. If it exists, it's opened; if not, it's created.")
                .value_parser(clap::value_parser!(PathBuf))
                .index(1),
        )
        .arg_required_else_help(false)
        .get_matches();

    let initial_db_path = matches.get_one::<PathBuf>("FILE").cloned();

    // Starting Rustyline with a default configuration
    let config = get_config();

    // Getting a new Rustyline Helper
    let helper = REPLHelper::default();

    // Initiatlizing Rustyline Editor with set config and setting helper
    let mut repl: Editor<REPLHelper, DefaultHistory> = Editor::with_config(config)?;
    repl.set_helper(Some(helper));

    // This method loads history file into memory
    // If it doesn't exist, creates one
    // TODO: Check history file size and if too big, clean it.
    if repl.load_history("history").is_err() {
        println!("No previous history.");
    }

    // Either open/create the requested file, or drop into a transient
    // in-memory database (the legacy default when no FILE is given).
    // We track whether the open succeeded so the banner doesn't claim
    // "Opened …" when we actually fell back to in-memory after a lock
    // contention or other failure.
    let (mut db, opened_path): (Database, Option<&std::path::PathBuf>) = match &initial_db_path {
        Some(path) => match open_or_create(path) {
            Ok(db) => (db, Some(path)),
            Err(err) => {
                eprintln!("Could not open '{}': {err}", path.display());
                eprintln!("Falling back to a transient in-memory database.");
                (Database::new("tempdb".to_string()), None)
            }
        },
        None => (Database::new("tempdb".to_string()), None),
    };

    // Friendly intro message for the user
    let connection_line = match opened_path {
        Some(path) => format!("Opened '{}' — auto-save enabled.", path.display()),
        None => "Connected to a transient in-memory database.\nUse '.open FILENAME' to reopen on a persistent database.".to_string(),
    };
    println!(
        "{} - {}\n{}{}{}",
        crate_name!(),
        crate_version!(),
        "Enter .exit to quit.\n",
        "Enter .help for usage hints.\n",
        connection_line,
    );

    let prompt = "sqlrite> ";

    loop {
        repl.helper_mut().expect("No helper found").colored_prompt =
            format!("\x1b[1;32m{prompt}\x1b[0m");
        // Source for ANSI Color information: http://www.perpetualpc.net/6429_colors.html#color_list
        // http://bixense.com/clicolors/

        let readline = repl.readline(prompt);
        match readline {
            Ok(command) => {
                let _ = repl.add_history_entry(command.as_str());
                // Parsing user's input and returning and enum of repl::CommandType
                match get_command_type(command.trim()) {
                    CommandType::SQLCommand(_cmd) => {
                        // process_command takes care of tokenizing, parsing and executing
                        // the SQL Statement and returning a Result<String, SQLRiteError>
                        match process_command(&command, &mut db) {
                            Ok(response) => println!("{response}"),
                            Err(err) => eprintln!("An error occured: {err}"),
                        }
                    }
                    CommandType::MetaCommand(cmd) => {
                        // handle_meta_command parses and executes the MetaCommand
                        // and returns a Result<String, SQLRiteError>
                        match handle_meta_command(cmd, &mut repl, &mut db) {
                            Ok(response) => println!("{response}"),
                            Err(err) => eprintln!("An error occured: {err}"),
                        }
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                break;
            }
            Err(ReadlineError::Eof) => {
                break;
            }
            Err(err) => {
                eprintln!("An error occured: {err:?}");
                break;
            }
        }
    }
    repl.append_history("history").unwrap();

    Ok(())
}

/// Equivalent to typing `.open FILE` at the REPL: load if present,
/// materialize an empty DB on disk if missing. Attaches the long-lived
/// Pager either way so subsequent writes auto-save.
fn open_or_create(path: &std::path::Path) -> sqlrite::Result<Database> {
    let db_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("db")
        .to_string();
    if path.exists() {
        sqlrite::open_database(path, db_name)
    } else {
        let mut fresh = Database::new(db_name);
        fresh.source_path = Some(path.to_path_buf());
        sqlrite::save_database(&mut fresh, path)?;
        Ok(fresh)
    }
}
