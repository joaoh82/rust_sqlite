//! `sqlrite-mcp` — Model Context Protocol server adapter for SQLRite.
//!
//! Spawned by an MCP client (Claude Code / Cursor / `mcp-inspector` /
//! anything that speaks the MCP stdio transport) as a subprocess. The
//! client speaks JSON-RPC 2.0 in line-delimited JSON on our stdin; we
//! reply on stdout. Stderr is reserved for diagnostics — anything that
//! prints there is invisible to the protocol but visible in the
//! client's "MCP server log" pane.
//!
//! See `docs/mcp.md` for the user-facing reference and the wiring
//! examples for each MCP-aware client. See `src/protocol.rs` and
//! `src/transport.rs` for the protocol mechanics.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use sqlrite::Connection;

mod error;
mod protocol;
mod stdio_redirect;
mod tools;
mod transport;

/// SQLRite as an MCP server. One server process = one open database.
///
/// Examples:
///
///     # Open an existing on-disk DB read-write (the default):
///     sqlrite-mcp ./mydb.sqlrite
///
///     # Read-only — `execute` tool is omitted from tools/list and
///     # rejected if the client tries to call it anyway:
///     sqlrite-mcp ./mydb.sqlrite --read-only
///
///     # Ephemeral in-memory DB — useful for one-off LLM scratchpads:
///     sqlrite-mcp --in-memory
///
/// You can also set `SQLRITE_MCP_DATABASE` instead of passing the
/// path on the command line. Some MCP client config files (Claude
/// Code's `claude.json`, Cursor's MCP UI, etc.) prefer a fixed
/// `command` with no arguments, in which case the env var is the
/// cleanest way to pin the database.
#[derive(Parser, Debug)]
#[command(
    name = "sqlrite-mcp",
    version,
    about = "MCP server for SQLRite — exposes a database to LLM agents over stdio.",
    long_about = None,
)]
struct Cli {
    /// Path to the SQLRite database file. Mutually exclusive with `--in-memory`.
    /// Falls back to `$SQLRITE_MCP_DATABASE` if neither is given.
    database: Option<PathBuf>,

    /// Open the database read-only. Acquires a shared lock; multiple
    /// `sqlrite-mcp --read-only` processes can sit on the same DB
    /// concurrently. The `execute` tool is hidden from `tools/list`
    /// and rejected with a tool-error if a client calls it anyway.
    #[arg(long)]
    read_only: bool,

    /// Run against a fresh in-memory database. State lives only for
    /// the lifetime of this process. Mutually exclusive with the
    /// `database` positional.
    #[arg(long)]
    in_memory: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Reserve stdout for the MCP protocol BEFORE anything else can
    // write to it. The engine's `process_command` prints REPL-style
    // tables on CREATE / INSERT / SELECT — those writes would
    // otherwise corrupt the JSON-RPC channel. After this call,
    // anything that writes to fd 1 from anywhere in the process ends
    // up on stderr (visible in the MCP client's server-log pane);
    // JSON-RPC responses go through the returned `File` handle.
    // See `src/stdio_redirect.rs` for the full reasoning.
    let real_stdout = match stdio_redirect::redirect_stdout_to_stderr() {
        Ok(f) => f,
        Err(err) => {
            eprintln!("error: failed to set up stdio for MCP protocol: {err}");
            return ExitCode::from(1);
        }
    };

    // Resolve the DB path: CLI arg → SQLRITE_MCP_DATABASE → in-memory
    // (only if --in-memory is set, otherwise we error out with a
    // clear message rather than silently picking a default).
    let db_path = cli
        .database
        .clone()
        .or_else(|| std::env::var_os("SQLRITE_MCP_DATABASE").map(PathBuf::from));

    if cli.in_memory && db_path.is_some() {
        eprintln!("error: --in-memory and a database path are mutually exclusive.");
        return ExitCode::from(2);
    }

    let conn_result = if cli.in_memory {
        // Phase 5a: Connection::open_in_memory() — shipped, returns a
        // fresh Database with no on-disk backing. Read-only flag is
        // ignored for in-memory; there's nothing to lock anyway.
        Connection::open_in_memory()
    } else {
        let Some(path) = db_path else {
            eprintln!(
                "error: no database specified. Pass a path, set \
                 SQLRITE_MCP_DATABASE, or use --in-memory.\n\
                 \n\
                 Try: sqlrite-mcp --help"
            );
            return ExitCode::from(2);
        };
        if cli.read_only {
            Connection::open_read_only(&path)
        } else {
            Connection::open(&path)
        }
    };

    let conn = match conn_result {
        Ok(c) => c,
        Err(err) => {
            eprintln!("error: failed to open database: {err}");
            return ExitCode::from(1);
        }
    };

    // Hand off to the transport runner. Returns once the client closes
    // stdin (clean shutdown) or we hit an unrecoverable I/O error.
    // We pass `real_stdout` (the saved-off original fd 1) rather than
    // `io::stdout()` so MCP responses bypass any stray engine writes.
    let stdin = std::io::stdin();
    match transport::run(stdin.lock(), real_stdout, conn, cli.read_only) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: transport loop terminated: {err}");
            ExitCode::from(1)
        }
    }
}
