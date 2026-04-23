//! C FFI shim around the SQLRite engine (Phase 5b).
//!
//! This crate turns the Rust-native `sqlrite::Connection` / `Statement`
//! / `Rows` API into a C-callable shared library. Every non-Rust SDK
//! (Python via PyO3, Node.js via napi-rs, Go via cgo, plus raw C) binds
//! against the same ABI surface defined here.
//!
//! ## Design
//!
//! - **Opaque pointers.** `SqlriteConnection` and `SqlriteStatement`
//!   are opaque to the caller; only the library constructs and
//!   destroys them. Callers must pair every `*_open` / `*_prepare`
//!   with the matching `*_close` / `*_finalize` or leak memory.
//! - **C-style error codes.** Every mutating call returns an
//!   [`SqlriteStatus`] int. On nonzero, the caller can fetch a
//!   descriptive message via [`sqlrite_last_error`]. The error string
//!   is thread-local so multi-threaded callers don't race.
//! - **Split execute / query.** SQLite's one-size-fits-all
//!   `sqlite3_prepare` + `sqlite3_step` collapses statement-that-
//!   returns-rows and statement-that-doesn't into one type. We split
//!   them: [`sqlrite_execute`] is fire-and-forget for DDL/DML/
//!   transactions; [`sqlrite_query`] returns a statement handle that
//!   yields rows via [`sqlrite_step`]. Cleaner to bind against.
//! - **Strings.** C strings in (inputs to [`sqlrite_open`],
//!   [`sqlrite_execute`], etc.) are NUL-terminated UTF-8 borrows
//!   owned by the caller. C strings out (from [`sqlrite_column_text`],
//!   [`sqlrite_column_name`], [`sqlrite_last_error`]) are heap-
//!   allocated by this library and must be freed with
//!   [`sqlrite_free_string`] — *except* `sqlrite_last_error` whose
//!   return value is owned by the thread-local and stays valid until
//!   the next error on the same thread.
//!
//! ## Memory rules at a glance
//!
//! | API                       | Ownership of return                          |
//! |---------------------------|----------------------------------------------|
//! | `sqlrite_open`            | caller frees via `sqlrite_close`             |
//! | `sqlrite_query`           | caller frees via `sqlrite_finalize`          |
//! | `sqlrite_column_text`     | caller frees via `sqlrite_free_string`       |
//! | `sqlrite_column_name`     | caller frees via `sqlrite_free_string`       |
//! | `sqlrite_last_error`      | library-owned thread-local, do *not* free   |
//!
//! ## Thread safety
//!
//! A `SqlriteConnection` is `!Sync` — don't share a single connection
//! across threads without external synchronization. The last-error
//! slot is thread-local, so multi-threaded callers can each inspect
//! their own error independently.

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_double, c_int};
use std::path::Path;
use std::ptr;

use sqlrite::{Connection, Rows, Value};

// ---------------------------------------------------------------------------
// Status codes

/// Return value of every mutating FFI function. `Ok` is zero; every
/// error is a distinct positive integer so bindings can switch on it.
/// The full error message (with path / SQL / nested causes) is
/// available via [`sqlrite_last_error`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlriteStatus {
    Ok = 0,
    /// Generic error — check `sqlrite_last_error` for details.
    Error = 1,
    /// A required pointer argument was null, or an input string was
    /// invalid UTF-8 / not NUL-terminated.
    InvalidArgument = 2,
    /// A SELECT query returned no more rows (returned from `step`).
    Done = 101,
    /// A SELECT query produced a row (returned from `step`).
    Row = 102,
}

// ---------------------------------------------------------------------------
// Thread-local last-error

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(msg: impl Into<String>) {
    let s = msg.into();
    // Strip NULs just in case — a C string can't contain interior NULs.
    let cleaned = s.replace('\0', "\\0");
    let cstr = CString::new(cleaned).unwrap_or_else(|_| CString::new("error").unwrap());
    LAST_ERROR.with(|slot| *slot.borrow_mut() = Some(cstr));
}

fn clear_last_error() {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = None);
}

/// Returns the last error message raised on the current thread, or
/// `NULL` if the most recent call succeeded. The returned pointer is
/// owned by the library (thread-local storage) and stays valid until
/// the next FFI call on the same thread — do *not* pass it to
/// [`sqlrite_free_string`].
///
/// # Safety
///
/// The caller must not mutate or free the returned pointer.
#[unsafe(no_mangle)]
pub extern "C" fn sqlrite_last_error() -> *const c_char {
    LAST_ERROR.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|cstr| cstr.as_ptr())
            .unwrap_or(ptr::null())
    })
}

// ---------------------------------------------------------------------------
// Opaque handle types
//
// `#[repr(C)]` on a unit struct gives cbindgen a typedef to reference;
// callers only see the pointer, never the contents.

/// Opaque handle to a SQLRite database connection.
#[repr(C)]
pub struct SqlriteConnection {
    _private: [u8; 0],
}

/// Opaque handle to a running SELECT query. Yields rows via
/// [`sqlrite_step`] until `SqlriteStatus::Done`.
#[repr(C)]
pub struct SqlriteStatement {
    _private: [u8; 0],
}

// Internal wrapper types — never exposed directly; the `SqlriteXxx`
// opaque struct pointers above are pointers to these under the hood.
struct ConnHandle {
    conn: Connection,
}

struct StmtHandle {
    rows: Rows,
    /// The row most recently produced by `step`. Column accessors
    /// read from this — `None` before the first `step` call and
    /// after the iterator has been exhausted.
    current: Option<sqlrite::OwnedRow>,
}

// ---------------------------------------------------------------------------
// Helpers

/// Turns a `Result<_, E>` into a status code + last-error side effect.
/// Callers capture the `Ok` value via an out-parameter before calling this.
fn status_of<T, E: std::fmt::Display>(r: Result<T, E>) -> (SqlriteStatus, Option<T>) {
    match r {
        Ok(v) => {
            clear_last_error();
            (SqlriteStatus::Ok, Some(v))
        }
        Err(e) => {
            set_last_error(e.to_string());
            (SqlriteStatus::Error, None)
        }
    }
}

/// Borrows a C string into &str or sets the last-error and returns None.
unsafe fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        set_last_error("null pointer passed where a string was required");
        return None;
    }
    // Safety precondition: caller guarantees `ptr` points at a valid
    // NUL-terminated UTF-8 sequence for its lifetime. Documented on
    // every `*const c_char` input in the generated header.
    let cstr = unsafe { CStr::from_ptr(ptr) };
    match cstr.to_str() {
        Ok(s) => Some(s),
        Err(_) => {
            set_last_error("input string was not valid UTF-8");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Connection lifecycle

/// Opens (or creates) a database file for read-write access.
///
/// On success, `*out` is set to a non-null handle and the return value
/// is `SqlriteStatus::Ok`. On failure, `*out` is left null and the
/// return value is an error status; call [`sqlrite_last_error`] for
/// the message.
///
/// # Safety
///
/// `path` must be a valid NUL-terminated UTF-8 string. `out` must be a
/// valid writable pointer. The caller owns the returned connection
/// and must free it with [`sqlrite_close`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_open(
    path: *const c_char,
    out: *mut *mut SqlriteConnection,
) -> SqlriteStatus {
    if out.is_null() {
        set_last_error("output pointer is null");
        return SqlriteStatus::InvalidArgument;
    }
    let Some(path_str) = (unsafe { cstr_to_str(path) }) else {
        return SqlriteStatus::InvalidArgument;
    };

    let (status, conn) = status_of(Connection::open(Path::new(path_str)));
    if let Some(conn) = conn {
        let boxed = Box::new(ConnHandle { conn });
        unsafe { *out = Box::into_raw(boxed) as *mut SqlriteConnection };
    } else {
        unsafe { *out = ptr::null_mut() };
    }
    status
}

/// Opens an existing database file for read-only access — takes a
/// shared advisory lock so multiple read-only openers coexist.
///
/// # Safety
///
/// Same as [`sqlrite_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_open_read_only(
    path: *const c_char,
    out: *mut *mut SqlriteConnection,
) -> SqlriteStatus {
    if out.is_null() {
        set_last_error("output pointer is null");
        return SqlriteStatus::InvalidArgument;
    }
    let Some(path_str) = (unsafe { cstr_to_str(path) }) else {
        return SqlriteStatus::InvalidArgument;
    };

    let (status, conn) = status_of(Connection::open_read_only(Path::new(path_str)));
    if let Some(conn) = conn {
        let boxed = Box::new(ConnHandle { conn });
        unsafe { *out = Box::into_raw(boxed) as *mut SqlriteConnection };
    } else {
        unsafe { *out = ptr::null_mut() };
    }
    status
}

/// Opens a transient in-memory database — no file, no locks. Useful
/// for tests and short-lived in-process DBs.
///
/// # Safety
///
/// `out` must be a valid writable pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_open_in_memory(out: *mut *mut SqlriteConnection) -> SqlriteStatus {
    if out.is_null() {
        set_last_error("output pointer is null");
        return SqlriteStatus::InvalidArgument;
    }
    let (status, conn) = status_of(Connection::open_in_memory());
    if let Some(conn) = conn {
        let boxed = Box::new(ConnHandle { conn });
        unsafe { *out = Box::into_raw(boxed) as *mut SqlriteConnection };
    } else {
        unsafe { *out = ptr::null_mut() };
    }
    status
}

/// Closes a connection and releases its file locks. Safe to call with
/// a null pointer (no-op).
///
/// # Safety
///
/// `conn` must be a pointer returned by one of the `sqlrite_open_*`
/// functions and not yet closed. After this call the pointer is
/// invalid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_close(conn: *mut SqlriteConnection) {
    if conn.is_null() {
        return;
    }
    // Safety: caller-provided pointer obtained from Box::into_raw in
    // one of the open functions.
    unsafe { drop(Box::from_raw(conn as *mut ConnHandle)) };
}

// ---------------------------------------------------------------------------
// Non-query execution — DDL, DML, transactions

/// Parses and executes a single SQL statement that doesn't produce
/// rows (CREATE / INSERT / UPDATE / DELETE / BEGIN / COMMIT /
/// ROLLBACK). Use [`sqlrite_query`] for SELECT.
///
/// # Safety
///
/// `conn` must be a valid open connection handle. `sql` must be a
/// valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_execute(
    conn: *mut SqlriteConnection,
    sql: *const c_char,
) -> SqlriteStatus {
    if conn.is_null() {
        set_last_error("connection handle is null");
        return SqlriteStatus::InvalidArgument;
    }
    let Some(sql_str) = (unsafe { cstr_to_str(sql) }) else {
        return SqlriteStatus::InvalidArgument;
    };
    // Safety: caller guarantees `conn` is a valid handle.
    let handle = unsafe { &mut *(conn as *mut ConnHandle) };
    let (status, _) = status_of(handle.conn.execute(sql_str));
    status
}

// ---------------------------------------------------------------------------
// Query execution — SELECT

/// Parses and runs a SELECT, returning a statement handle whose rows
/// are iterated via [`sqlrite_step`]. Errors if the SQL isn't a
/// SELECT.
///
/// # Safety
///
/// `conn` must be a valid open connection handle. `sql` must be a
/// valid NUL-terminated UTF-8 string. `out` must be a valid writable
/// pointer. On success the caller owns the returned statement and
/// must call [`sqlrite_finalize`] when done.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_query(
    conn: *mut SqlriteConnection,
    sql: *const c_char,
    out: *mut *mut SqlriteStatement,
) -> SqlriteStatus {
    if conn.is_null() || out.is_null() {
        set_last_error("connection or output handle is null");
        return SqlriteStatus::InvalidArgument;
    }
    let Some(sql_str) = (unsafe { cstr_to_str(sql) }) else {
        return SqlriteStatus::InvalidArgument;
    };
    let handle = unsafe { &mut *(conn as *mut ConnHandle) };

    let stmt_result: Result<Rows, _> = (|| {
        let stmt = handle.conn.prepare(sql_str)?;
        stmt.query()
    })();

    let (status, rows) = status_of(stmt_result);
    if let Some(rows) = rows {
        let boxed = Box::new(StmtHandle {
            rows,
            current: None,
        });
        unsafe { *out = Box::into_raw(boxed) as *mut SqlriteStatement };
    } else {
        unsafe { *out = ptr::null_mut() };
    }
    status
}

/// Advances the statement to the next row.
///
/// Returns:
/// - `SqlriteStatus::Row` — a row is available; read columns via the
///   `sqlrite_column_*` accessors.
/// - `SqlriteStatus::Done` — the query is exhausted; stop calling step.
/// - any error code — check `sqlrite_last_error`.
///
/// # Safety
///
/// `stmt` must be a valid statement handle returned by
/// [`sqlrite_query`] and not yet finalized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_step(stmt: *mut SqlriteStatement) -> SqlriteStatus {
    if stmt.is_null() {
        set_last_error("statement handle is null");
        return SqlriteStatus::InvalidArgument;
    }
    let handle = unsafe { &mut *(stmt as *mut StmtHandle) };
    match handle.rows.next() {
        Ok(Some(row)) => {
            handle.current = Some(row.to_owned_row());
            clear_last_error();
            SqlriteStatus::Row
        }
        Ok(None) => {
            handle.current = None;
            clear_last_error();
            SqlriteStatus::Done
        }
        Err(e) => {
            set_last_error(e.to_string());
            handle.current = None;
            SqlriteStatus::Error
        }
    }
}

/// Frees a statement handle.
///
/// # Safety
///
/// `stmt` must be a pointer returned by [`sqlrite_query`] and not
/// yet finalized. After this call the pointer is invalid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_finalize(stmt: *mut SqlriteStatement) {
    if stmt.is_null() {
        return;
    }
    unsafe { drop(Box::from_raw(stmt as *mut StmtHandle)) };
}

// ---------------------------------------------------------------------------
// Column accessors
//
// Every accessor reads from the most recent row produced by step(). If
// step() hasn't been called or returned Done, the accessors set an
// error.

/// Number of columns in the current row. Writes the count to `*out`
/// and returns `Ok`, or an error status if no row is active.
///
/// # Safety
///
/// `stmt` must be a valid statement handle. `out` must be a valid
/// writable pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_column_count(
    stmt: *mut SqlriteStatement,
    out: *mut c_int,
) -> SqlriteStatus {
    if stmt.is_null() || out.is_null() {
        set_last_error("statement or output pointer is null");
        return SqlriteStatus::InvalidArgument;
    }
    let handle = unsafe { &*(stmt as *const StmtHandle) };
    unsafe { *out = handle.rows.columns().len() as c_int };
    SqlriteStatus::Ok
}

/// Writes the name of column `idx` into `*out` as a heap-allocated
/// NUL-terminated string. The caller must free it via
/// [`sqlrite_free_string`].
///
/// # Safety
///
/// Same as [`sqlrite_column_count`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_column_name(
    stmt: *mut SqlriteStatement,
    idx: c_int,
    out: *mut *mut c_char,
) -> SqlriteStatus {
    if stmt.is_null() || out.is_null() {
        set_last_error("statement or output pointer is null");
        return SqlriteStatus::InvalidArgument;
    }
    let handle = unsafe { &*(stmt as *const StmtHandle) };
    let columns = handle.rows.columns();
    let Some(name) = columns.get(idx as usize) else {
        set_last_error(format!(
            "column index {idx} out of bounds (statement has {} columns)",
            columns.len()
        ));
        return SqlriteStatus::Error;
    };
    unsafe { *out = alloc_c_string(name) };
    SqlriteStatus::Ok
}

/// Reads column `idx` of the current row as a 64-bit integer into `*out`.
/// Errors if no row is active, the column is NULL, or the column's
/// native type can't be losslessly converted to i64.
///
/// # Safety
///
/// Same as [`sqlrite_column_count`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_column_int64(
    stmt: *mut SqlriteStatement,
    idx: c_int,
    out: *mut i64,
) -> SqlriteStatus {
    with_current_value(
        stmt,
        idx,
        |v, out_void| match v {
            Value::Integer(n) => {
                unsafe { *(out_void as *mut i64) = *n };
                SqlriteStatus::Ok
            }
            Value::Null => {
                set_last_error("column is NULL (use sqlrite_column_is_null to check first)");
                SqlriteStatus::Error
            }
            other => {
                set_last_error(format!("cannot convert {other:?} to int64"));
                SqlriteStatus::Error
            }
        },
        out as *mut c_void,
    )
}

/// Reads column `idx` as a double.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_column_double(
    stmt: *mut SqlriteStatement,
    idx: c_int,
    out: *mut c_double,
) -> SqlriteStatus {
    with_current_value(
        stmt,
        idx,
        |v, out_void| match v {
            Value::Real(f) => {
                unsafe { *(out_void as *mut c_double) = *f };
                SqlriteStatus::Ok
            }
            Value::Integer(n) => {
                unsafe { *(out_void as *mut c_double) = *n as c_double };
                SqlriteStatus::Ok
            }
            Value::Null => {
                set_last_error("column is NULL");
                SqlriteStatus::Error
            }
            other => {
                set_last_error(format!("cannot convert {other:?} to double"));
                SqlriteStatus::Error
            }
        },
        out as *mut c_void,
    )
}

/// Reads column `idx` as a newly-allocated NUL-terminated UTF-8
/// string. The caller must free the result via [`sqlrite_free_string`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_column_text(
    stmt: *mut SqlriteStatement,
    idx: c_int,
    out: *mut *mut c_char,
) -> SqlriteStatus {
    with_current_value(
        stmt,
        idx,
        |v, out_void| match v {
            Value::Text(s) => {
                unsafe { *(out_void as *mut *mut c_char) = alloc_c_string(s) };
                SqlriteStatus::Ok
            }
            Value::Null => {
                set_last_error("column is NULL");
                SqlriteStatus::Error
            }
            // For Int/Real/Bool we coerce to the display form — matches
            // sqlite3_column_text's lenient behavior.
            other => {
                let rendered = other.to_display_string();
                unsafe { *(out_void as *mut *mut c_char) = alloc_c_string(&rendered) };
                SqlriteStatus::Ok
            }
        },
        out as *mut c_void,
    )
}

/// Writes `1` to `*out` if column `idx` is NULL, `0` otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_column_is_null(
    stmt: *mut SqlriteStatement,
    idx: c_int,
    out: *mut c_int,
) -> SqlriteStatus {
    with_current_value(
        stmt,
        idx,
        |v, out_void| {
            unsafe { *(out_void as *mut c_int) = matches!(v, Value::Null) as c_int };
            SqlriteStatus::Ok
        },
        out as *mut c_void,
    )
}

// ---------------------------------------------------------------------------
// Introspection

/// Returns 1 if a `BEGIN … COMMIT/ROLLBACK` block is open on this
/// connection, 0 otherwise. -1 on error (null handle).
///
/// # Safety
///
/// `conn` must be a valid open connection handle (or null, in which
/// case this returns -1).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_in_transaction(conn: *mut SqlriteConnection) -> c_int {
    if conn.is_null() {
        set_last_error("connection handle is null");
        return -1;
    }
    let handle = unsafe { &*(conn as *const ConnHandle) };
    handle.conn.in_transaction() as c_int
}

/// Returns 1 if this connection was opened read-only, 0 otherwise.
/// -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_is_read_only(conn: *mut SqlriteConnection) -> c_int {
    if conn.is_null() {
        set_last_error("connection handle is null");
        return -1;
    }
    let handle = unsafe { &*(conn as *const ConnHandle) };
    handle.conn.is_read_only() as c_int
}

// ---------------------------------------------------------------------------
// Memory freeing

/// Frees a string returned by `sqlrite_column_text` or `sqlrite_column_name`.
/// Safe to call with a null pointer (no-op).
///
/// # Safety
///
/// `ptr` must be a pointer returned by one of those functions and not
/// yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlrite_free_string(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    unsafe { drop(CString::from_raw(ptr)) };
}

// ---------------------------------------------------------------------------
// Internal helpers

use std::ffi::c_void;

/// Runs `f` against the current row's column value, handling the
/// no-row / out-of-bounds / null-pointer paths centrally. `out` is
/// a type-erased pointer; `f` casts it back to the expected type.
fn with_current_value<F>(
    stmt: *mut SqlriteStatement,
    idx: c_int,
    f: F,
    out: *mut c_void,
) -> SqlriteStatus
where
    F: FnOnce(&Value, *mut c_void) -> SqlriteStatus,
{
    if stmt.is_null() || out.is_null() {
        set_last_error("statement or output pointer is null");
        return SqlriteStatus::InvalidArgument;
    }
    let handle = unsafe { &*(stmt as *const StmtHandle) };
    let Some(row) = handle.current.as_ref() else {
        set_last_error(
            "no current row — call sqlrite_step() and check for Row before reading columns",
        );
        return SqlriteStatus::Error;
    };
    let Some(val) = row.values.get(idx as usize) else {
        set_last_error(format!(
            "column index {idx} out of bounds (row has {} columns)",
            row.values.len()
        ));
        return SqlriteStatus::Error;
    };
    clear_last_error();
    f(val, out)
}

/// Heap-allocates a C string from a Rust &str. The caller owns the
/// result and must free it via `sqlrite_free_string`. Falls back to
/// an empty string if the input contains interior NULs (shouldn't
/// happen for UTF-8 text from SQLite-style engines but we defend).
fn alloc_c_string(s: &str) -> *mut c_char {
    CString::new(s.replace('\0', "\\0"))
        .unwrap_or_else(|_| CString::new("").unwrap())
        .into_raw()
}

// ---------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: makes a CString and returns its raw pointer (while keeping
    // the CString alive for the test's scope via the returned holder).
    fn cstr(s: &str) -> (CString, *const c_char) {
        let c = CString::new(s).unwrap();
        let p = c.as_ptr();
        (c, p)
    }

    fn last_err() -> Option<String> {
        unsafe {
            let p = sqlrite_last_error();
            if p.is_null() {
                None
            } else {
                Some(CStr::from_ptr(p).to_string_lossy().into_owned())
            }
        }
    }

    #[test]
    fn open_in_memory_execute_and_close() {
        unsafe {
            let mut conn: *mut SqlriteConnection = ptr::null_mut();
            assert_eq!(sqlrite_open_in_memory(&mut conn), SqlriteStatus::Ok);
            assert!(!conn.is_null());

            let (_c1, p1) = cstr("CREATE TABLE t (id INTEGER PRIMARY KEY, x TEXT);");
            assert_eq!(sqlrite_execute(conn, p1), SqlriteStatus::Ok);

            let (_c2, p2) = cstr("INSERT INTO t (x) VALUES ('alpha');");
            assert_eq!(sqlrite_execute(conn, p2), SqlriteStatus::Ok);

            assert!(last_err().is_none());
            sqlrite_close(conn);
        }
    }

    #[test]
    fn query_step_iterates_rows() {
        unsafe {
            let mut conn: *mut SqlriteConnection = ptr::null_mut();
            sqlrite_open_in_memory(&mut conn);

            let (_c, p) = cstr("CREATE TABLE t (id INTEGER PRIMARY KEY, x TEXT);");
            sqlrite_execute(conn, p);
            let (_c, p) = cstr("INSERT INTO t (x) VALUES ('alpha');");
            sqlrite_execute(conn, p);
            let (_c, p) = cstr("INSERT INTO t (x) VALUES ('beta');");
            sqlrite_execute(conn, p);

            let mut stmt: *mut SqlriteStatement = ptr::null_mut();
            let (_c, p) = cstr("SELECT id, x FROM t;");
            assert_eq!(sqlrite_query(conn, p, &mut stmt), SqlriteStatus::Ok);
            assert!(!stmt.is_null());

            let mut col_count: c_int = 0;
            sqlrite_column_count(stmt, &mut col_count);
            assert_eq!(col_count, 2);

            let mut names: Vec<String> = Vec::new();
            for i in 0..col_count {
                let mut out: *mut c_char = ptr::null_mut();
                sqlrite_column_name(stmt, i, &mut out);
                names.push(CStr::from_ptr(out).to_string_lossy().into_owned());
                sqlrite_free_string(out);
            }
            assert_eq!(names, vec!["id", "x"]);

            let mut collected: Vec<(i64, String)> = Vec::new();
            loop {
                let status = sqlrite_step(stmt);
                if status == SqlriteStatus::Done {
                    break;
                }
                assert_eq!(status, SqlriteStatus::Row);
                let mut id: i64 = 0;
                assert_eq!(sqlrite_column_int64(stmt, 0, &mut id), SqlriteStatus::Ok);
                let mut text: *mut c_char = ptr::null_mut();
                assert_eq!(sqlrite_column_text(stmt, 1, &mut text), SqlriteStatus::Ok);
                let s = CStr::from_ptr(text).to_string_lossy().into_owned();
                sqlrite_free_string(text);
                collected.push((id, s));
            }
            assert_eq!(collected.len(), 2);
            assert_eq!(collected[0].1, "alpha");
            assert_eq!(collected[1].1, "beta");

            sqlrite_finalize(stmt);
            sqlrite_close(conn);
        }
    }

    #[test]
    fn execute_surfaces_error_via_last_error() {
        unsafe {
            let mut conn: *mut SqlriteConnection = ptr::null_mut();
            sqlrite_open_in_memory(&mut conn);

            let (_c, p) = cstr("SELECT from where");
            let status = sqlrite_execute(conn, p);
            assert_eq!(status, SqlriteStatus::Error);
            let err = last_err().expect("expected an error message");
            assert!(!err.is_empty());

            sqlrite_close(conn);
        }
    }

    #[test]
    fn null_pointer_inputs_return_invalid_argument() {
        unsafe {
            let status = sqlrite_open(ptr::null(), ptr::null_mut());
            assert_eq!(status, SqlriteStatus::InvalidArgument);

            let status = sqlrite_execute(ptr::null_mut(), ptr::null());
            assert_eq!(status, SqlriteStatus::InvalidArgument);
        }
    }

    #[test]
    fn in_memory_transactions_work_through_ffi() {
        unsafe {
            let mut conn: *mut SqlriteConnection = ptr::null_mut();
            sqlrite_open_in_memory(&mut conn);

            let (_c, p) = cstr("CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER);");
            sqlrite_execute(conn, p);
            let (_c, p) = cstr("INSERT INTO t (x) VALUES (1);");
            sqlrite_execute(conn, p);

            assert_eq!(sqlrite_in_transaction(conn), 0);
            let (_c, p) = cstr("BEGIN;");
            sqlrite_execute(conn, p);
            assert_eq!(sqlrite_in_transaction(conn), 1);

            let (_c, p) = cstr("INSERT INTO t (x) VALUES (2);");
            sqlrite_execute(conn, p);

            let (_c, p) = cstr("ROLLBACK;");
            sqlrite_execute(conn, p);
            assert_eq!(sqlrite_in_transaction(conn), 0);

            // Should still have just one row after rollback.
            let mut stmt: *mut SqlriteStatement = ptr::null_mut();
            let (_c, p) = cstr("SELECT id FROM t;");
            sqlrite_query(conn, p, &mut stmt);
            let mut row_count = 0;
            while sqlrite_step(stmt) == SqlriteStatus::Row {
                row_count += 1;
            }
            assert_eq!(row_count, 1);
            sqlrite_finalize(stmt);
            sqlrite_close(conn);
        }
    }

    #[test]
    fn is_null_detects_null_columns() {
        unsafe {
            let mut conn: *mut SqlriteConnection = ptr::null_mut();
            sqlrite_open_in_memory(&mut conn);
            let (_c, p) = cstr("CREATE TABLE t (id INTEGER PRIMARY KEY, note TEXT);");
            sqlrite_execute(conn, p);
            let (_c, p) = cstr("INSERT INTO t (id) VALUES (1);");
            sqlrite_execute(conn, p);

            let mut stmt: *mut SqlriteStatement = ptr::null_mut();
            let (_c, p) = cstr("SELECT id, note FROM t;");
            sqlrite_query(conn, p, &mut stmt);
            assert_eq!(sqlrite_step(stmt), SqlriteStatus::Row);

            let mut is_null: c_int = 0;
            sqlrite_column_is_null(stmt, 0, &mut is_null);
            assert_eq!(is_null, 0);
            sqlrite_column_is_null(stmt, 1, &mut is_null);
            assert_eq!(is_null, 1);

            sqlrite_finalize(stmt);
            sqlrite_close(conn);
        }
    }

    #[test]
    fn close_null_is_a_noop() {
        unsafe {
            sqlrite_close(ptr::null_mut());
            sqlrite_finalize(ptr::null_mut());
            sqlrite_free_string(ptr::null_mut());
        }
    }

    #[test]
    fn step_without_query_returns_error() {
        // Column accessors against a handle that hasn't produced a row
        // yet should error, not segfault.
        unsafe {
            let mut conn: *mut SqlriteConnection = ptr::null_mut();
            sqlrite_open_in_memory(&mut conn);
            let (_c, p) = cstr("CREATE TABLE t (x INTEGER PRIMARY KEY);");
            sqlrite_execute(conn, p);

            let mut stmt: *mut SqlriteStatement = ptr::null_mut();
            let (_c, p) = cstr("SELECT x FROM t;");
            sqlrite_query(conn, p, &mut stmt);

            // Table is empty → first step is Done.
            assert_eq!(sqlrite_step(stmt), SqlriteStatus::Done);

            // Column read without a current row errors.
            let mut out: i64 = 0;
            assert_eq!(
                sqlrite_column_int64(stmt, 0, &mut out),
                SqlriteStatus::Error
            );
            assert!(last_err().unwrap().contains("no current row"));

            sqlrite_finalize(stmt);
            sqlrite_close(conn);
        }
    }
}
