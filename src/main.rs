//! REPL binary — the `sqlrite` CLI.
//!
//! Thin wrapper around the [`sqlrite`] library: rustyline for input, the
//! engine for parsing + execution + persistence.

mod meta_command;
mod repl;

use std::path::PathBuf;

use meta_command::handle_meta_command;
use repl::{CommandType, REPLHelper, ReplState, get_command_type, get_config};
use sqlrite::Connection;

use rustyline::Editor;
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;

use clap::{Arg, ArgAction, Command, crate_authors, crate_name, crate_version};

const ABOUT_SHORT: &str = "A small SQLite-like embedded database and REPL, written in Rust.";

const ABOUT_LONG: &str = "\
A small SQLite-like embedded database and REPL, written in Rust.

Passing a FILE argument is equivalent to launching the REPL and then running
`.open FILE` — existing files are loaded, missing files are created fresh.
Without a FILE the REPL starts in transient in-memory mode.

Add --readonly to open the FILE with a shared OS-level lock. Multiple
read-only REPLs on the same file coexist; any write attempt returns a
'database is opened read-only' error. Useful for poking at a DB while
another process holds the writer lock.

Once in the REPL, meta commands start with a dot:

  .help                  Show the meta-command list
  .open <FILENAME>       Open (or create) a .sqlrite database file
  .save <FILENAME>       Write the current DB to FILENAME (rarely needed —
                         once .open is in play, every write auto-saves)
  .tables                List tables in the current database
  .spawn                 Mint a sibling connection sharing this database
  .use <NAME>            Switch the active handle (A, B, ...) — see .conns
  .conns                 List every handle, marking the active one
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
        .arg(
            Arg::new("readonly")
                .long("readonly")
                .short('r')
                .help("Open the file with a shared lock (read-only). Coexists with other readers; any write returns an error. Requires FILE.")
                .action(ArgAction::SetTrue),
        )
        .arg_required_else_help(false)
        .get_matches();

    let initial_db_path = matches.get_one::<PathBuf>("FILE").cloned();
    let read_only = matches.get_flag("readonly");

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
    if read_only && initial_db_path.is_none() {
        eprintln!("--readonly requires a FILE argument");
        std::process::exit(1);
    }

    let (initial_conn, opened_path): (Connection, Option<&std::path::PathBuf>) =
        match &initial_db_path {
            Some(path) => match open_or_create(path, read_only) {
                Ok(conn) => (conn, Some(path)),
                Err(err) => {
                    eprintln!("Could not open '{}': {err}", path.display());
                    eprintln!("Falling back to a transient in-memory database.");
                    (
                        Connection::open_in_memory().expect("in-memory open never fails"),
                        None,
                    )
                }
            },
            None => (
                Connection::open_in_memory().expect("in-memory open never fails"),
                None,
            ),
        };
    let mut state = ReplState::new(initial_conn);

    // Friendly intro message for the user
    let connection_line = match opened_path {
        Some(path) if read_only => format!("Opened '{}' (read-only).", path.display()),
        Some(path) => format!("Opened '{}' — auto-save enabled.", path.display()),
        None => "Connected to a transient in-memory database.\nUse '.open FILENAME' to reopen on a persistent database.".to_string(),
    };
    println!(
        "{} - {}\nEnter .exit to quit.\nEnter .help for usage hints.\n{}",
        crate_name!(),
        crate_version!(),
        connection_line,
    );

    loop {
        // Prompt shows the active handle so multi-handle demos
        // (`.spawn` / `.use`) make it obvious which connection is
        // about to execute the next statement.
        let prompt = format!("sqlrite[{}]> ", state.active_name());
        repl.helper_mut().expect("No helper found").colored_prompt =
            format!("\x1b[1;32m{prompt}\x1b[0m");
        // Source for ANSI Color information: http://www.perpetualpc.net/6429_colors.html#color_list
        // http://bixense.com/clicolors/

        let readline = repl.readline(&prompt);
        match readline {
            Ok(command) => {
                let _ = repl.add_history_entry(command.as_str());
                // Parsing user's input and returning and enum of repl::CommandType
                match get_command_type(command.trim()) {
                    CommandType::SQLCommand(_cmd) => {
                        // Route through `Connection::execute_with_render`
                        // so `BEGIN CONCURRENT` / `COMMIT` / `ROLLBACK`
                        // hit the per-connection MVCC state, and reads
                        // inside an open concurrent transaction see the
                        // BEGIN-time snapshot. SELECTs come back with
                        // the pre-rendered prettytable; we print that
                        // first so the user sees the rows above the
                        // confirmation. Prior to the engine-stdout-
                        // pollution cleanup the engine printed the table
                        // itself, which corrupted any non-REPL stdout
                        // channel — the REPL owns the printing now.
                        match state.active_conn_mut().execute_with_render(&command) {
                            Ok(output) => {
                                if let Some(rendered) = output.rendered.as_deref() {
                                    print!("{rendered}");
                                }
                                println!("{}", output.status);
                            }
                            Err(err) => eprintln!("An error occured: {err}"),
                        }
                    }
                    CommandType::MetaCommand(cmd) => {
                        // handle_meta_command parses and executes the MetaCommand
                        // and returns a Result<String, SQLRiteError>
                        match handle_meta_command(cmd, &mut repl, &mut state) {
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
/// materialize an empty DB on disk if missing. Returns a fresh
/// `Connection` whose backing `Arc<Mutex<Database>>` carries the
/// long-lived pager so subsequent writes auto-save.
///
/// When `read_only` is set we take a shared advisory lock and never
/// materialize a missing file — read-only mode must fail cleanly if
/// the target doesn't exist rather than silently creating one.
fn open_or_create(path: &std::path::Path, read_only: bool) -> sqlrite::Result<Connection> {
    if read_only {
        if !path.exists() {
            return Err(sqlrite::SQLRiteError::General(format!(
                "read-only open requested but '{}' does not exist",
                path.display()
            )));
        }
        Connection::open_read_only(path)
    } else {
        // `Connection::open` already does the create-on-missing
        // dance (materialize a fresh DB + save_database) and
        // returns the attached pager either way.
        Connection::open(path)
    }
}
