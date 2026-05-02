//! Stdout-redirect dance for the MCP transport.
//!
//! ## Why this exists
//!
//! The MCP wire format on stdio is JSON-RPC 2.0, one message per line
//! on stdout. ANY other write to stdout corrupts the protocol — a
//! lone `println!` from somewhere in the dep tree, a debug print left
//! in by accident, a banner from a third-party crate. The MCP client
//! sees garbage where it expected JSON and disconnects.
//!
//! The SQLRite engine's REPL-convenience prints inside
//! `process_command` (the `CREATE TABLE` schema dump at sql/mod.rs:150,
//! the `INSERT` row dump at sql/mod.rs:208, the `SELECT` result table
//! at sql/mod.rs:224) are exactly this kind of write. They're great
//! for an interactive REPL; lethal for an MCP server.
//!
//! ## What this module does
//!
//! At process startup, before opening the database:
//!
//! 1. Duplicate fd 1 (stdout) to get a private handle pointing at
//!    the original destination — that's where MCP responses go.
//! 2. `dup2(2, 1)` — overwrite fd 1 with stderr, so any subsequent
//!    `print!` / `println!` from anywhere in the process ends up on
//!    stderr (visible in the MCP client's "server log" pane).
//! 3. Hand back a `File` wrapping the saved-off fd; the transport
//!    runner writes JSON-RPC responses to it.
//!
//! ## Cross-platform notes
//!
//! `libc::dup` / `libc::dup2` work on Unix and Windows alike (the
//! Windows targets in the `libc` crate map them to `_dup` / `_dup2`
//! from the MSVCRT). One implementation, two platforms — no `#[cfg]`
//! arms required.

use std::fs::File;
use std::io;

/// Replace process fd 1 (stdout) with a duplicate of fd 2 (stderr),
/// returning a `File` that still writes to the original stdout. Call
/// this exactly once, very early in `main`, before anything else can
/// emit stdout.
///
/// Returns an error if either `dup` or `dup2` fails — both extremely
/// rare (out of file descriptors, somebody closed fd 1 already, etc.)
/// and worth aborting on.
pub fn redirect_stdout_to_stderr() -> io::Result<File> {
    use std::os::fd::FromRawFd;

    // Save fd 1 (stdout) into a fresh fd. The new fd points at the
    // same underlying open-file-description (the controlling terminal
    // or pipe the parent set up for us).
    let saved_stdout_fd = unsafe { libc::dup(1) };
    if saved_stdout_fd < 0 {
        return Err(io::Error::last_os_error());
    }

    // Replace fd 1 with a duplicate of fd 2. From this point on, any
    // `println!()` (which writes to Rust's `io::stdout()`, which
    // ultimately writes to fd 1) goes to stderr.
    if unsafe { libc::dup2(2, 1) } < 0 {
        let err = io::Error::last_os_error();
        // Best-effort: close the saved fd before returning. If this
        // fails too, we're well past graceful recovery.
        unsafe { libc::close(saved_stdout_fd) };
        return Err(err);
    }

    // Important: drop Rust's cached `Stdout` wrapper if it has buffered
    // anything. We haven't written anything yet (no `println!` runs
    // before `main`), but flushing-then-rebinding keeps the contract
    // tight. The new `File` from the saved fd is what the transport
    // runner will use for all MCP writes.
    let _ = io::Write::flush(&mut io::stdout());

    // Wrap the saved fd in a `File`. `from_raw_fd` is the safe-ish way
    // to take ownership: when the `File` drops, it closes the fd.
    let real_stdout = unsafe { File::from_raw_fd(saved_stdout_fd) };
    Ok(real_stdout)
}
