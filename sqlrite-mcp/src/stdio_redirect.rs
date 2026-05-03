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
//! ## Engine-side fix + this defense-in-depth backstop
//!
//! As of the engine-stdout-pollution cleanup, the SQLRite engine's
//! `process_command` no longer writes to stdout. The historical REPL-
//! convenience prints (CREATE TABLE schema dump, INSERT row dump,
//! SELECT result table) all moved out: the rendered SELECT table now
//! comes back inside [`sqlrite::CommandOutput::rendered`] for the REPL
//! to print itself, and the CREATE / INSERT prints were dropped
//! entirely (the latter was a spammy bug-feature anyway). So in normal
//! operation this redirect catches nothing.
//!
//! We keep the redirect anyway as **defense in depth.** Three concrete
//! futures it protects against: (a) a future engine PR accidentally
//! reintroducing a `println!` that the test suite doesn't catch
//! (engine tests don't assert on stdout); (b) a transitive dep adding
//! a startup banner; (c) a debug print left in mid-development. Any
//! one of those would silently break the MCP server without this
//! safety net. Cost: ~140 LOC + a libc dep that's already a
//! transitive of clap.
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
//! `libc::dup` / `libc::dup2` exist on both Unix and Windows (Windows
//! targets resolve them to `_dup` / `_dup2` in the MSVCRT). The
//! divergence is in turning the resulting C runtime fd into a Rust
//! `File`:
//!
//! - **Unix**: `std::os::fd::FromRawFd::from_raw_fd(fd) -> File`
//!   directly. The fd IS the kernel handle.
//! - **Windows**: C runtime fds aren't kernel handles. We need
//!   `_get_osfhandle(fd) -> HANDLE`, then
//!   `std::os::windows::io::FromRawHandle::from_raw_handle(handle) -> File`.
//!
//! Two `#[cfg]` arms below.

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
#[cfg(unix)]
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
        // Best-effort: close the saved fd before returning.
        unsafe { libc::close(saved_stdout_fd) };
        return Err(err);
    }

    // Flush Rust's cached `Stdout` buffer in case anything raced in
    // before we got here. (In practice nothing does — `main` is the
    // first user code to run — but flushing keeps the contract tight.)
    let _ = io::Write::flush(&mut io::stdout());

    // Wrap the saved fd in a `File`. The `File` owns the fd and will
    // close it on drop.
    let real_stdout = unsafe { File::from_raw_fd(saved_stdout_fd) };
    Ok(real_stdout)
}

#[cfg(windows)]
pub fn redirect_stdout_to_stderr() -> io::Result<File> {
    use std::os::raw::c_int;
    use std::os::windows::io::{FromRawHandle, RawHandle};

    // Bind directly to MSVCRT instead of relying on whatever surface
    // `libc` happens to expose on Windows targets. `_dup`/`_dup2`/
    // `_close` operate on C runtime fds (small ints, same as Unix);
    // `_get_osfhandle` converts a C runtime fd into a Win32 HANDLE
    // (a pointer-sized integer) so we can wrap it in a Rust `File`.
    //
    // The underscore-prefixed names are the canonical exports —
    // they've been stable in MSVCRT / UCRT for decades and aren't
    // going anywhere. Return type for `_get_osfhandle` is `intptr_t`,
    // which we model as `isize` (pointer-width signed integer matches
    // on both x86 and x64 Windows targets).
    unsafe extern "C" {
        fn _dup(fd: c_int) -> c_int;
        fn _dup2(src: c_int, dst: c_int) -> c_int;
        fn _close(fd: c_int) -> c_int;
        fn _get_osfhandle(fd: c_int) -> isize;
    }

    // Save fd 1, then redirect fd 1 → fd 2.
    let saved_stdout_fd = unsafe { _dup(1) };
    if saved_stdout_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { _dup2(2, 1) } < 0 {
        let err = io::Error::last_os_error();
        unsafe { _close(saved_stdout_fd) };
        return Err(err);
    }

    let _ = io::Write::flush(&mut io::stdout());

    // Convert C runtime fd → Win32 HANDLE. Returns -1 on error
    // (INVALID_HANDLE_VALUE).
    let handle = unsafe { _get_osfhandle(saved_stdout_fd) };
    if handle == -1 {
        unsafe { _close(saved_stdout_fd) };
        return Err(io::Error::last_os_error());
    }

    // Wrap the HANDLE in a `File`. NOTE: the underlying C runtime fd
    // (`saved_stdout_fd`) is intentionally leaked here — `File` only
    // closes the HANDLE on drop, and closing both would double-free
    // the kernel handle. The leaked fd is harmless: it lives for the
    // process lifetime, and there's exactly one of them per server run.
    let real_stdout = unsafe { File::from_raw_handle(handle as RawHandle) };
    Ok(real_stdout)
}
