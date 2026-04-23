// Package sqlrite is a `database/sql`-compatible driver for the
// SQLRite embedded database engine.
//
// Usage:
//
//	import (
//	    "database/sql"
//	    _ "github.com/joaoh82/rust_sqlite/sdk/go"
//	)
//
//	db, err := sql.Open("sqlrite", "foo.sqlrite")
//	// or: sql.Open("sqlrite", ":memory:")
//
// All standard `database/sql` operations work — Exec, Query, QueryRow,
// transactions via Begin/Commit/Rollback on *sql.Tx. Rows are scanned
// into Go types via `rows.Scan(&id, &name, ...)`.
//
// # How it's wired
//
// This package is a thin cgo shim over the C FFI crate (sqlrite-ffi)
// at ../../sqlrite-ffi. At build time cgo compiles sqlrite.h (the
// cbindgen-generated header) and links the `libsqlrite_c` dynamic
// library. Phase 6e will ship prebuilt binaries alongside the Go
// module for every supported platform; for now, developers building
// from a repo clone need `cargo build --release -p sqlrite-ffi`
// first so the shared library exists.
//
// # Parameter binding
//
// Like the other SDKs, parameter binding isn't yet in the engine
// (deferred to Phase 5a.2). The driver accepts the `database/sql`
// signature for forward compat, but any non-empty args slice returns
// a clear error. Inline values into the SQL for the moment.
package sqlrite

/*
// Point cgo at the FFI crate's header + the cargo target dir. Paths
// are relative to this Go file (${SRCDIR}). Developers checking out
// the repo need to `cargo build --release -p sqlrite-ffi` once
// before `go test`; Phase 6e packages prebuilt libraries into the
// published Go module so end users don't need the Rust toolchain.
#cgo CFLAGS: -I${SRCDIR}/../../sqlrite-ffi/include
#cgo LDFLAGS: -L${SRCDIR}/../../target/release -lsqlrite_c

// Embed an rpath pointing at the cargo target dir so `go test` /
// `go run` find libsqlrite_c without any DYLD_LIBRARY_PATH dance.
// Production builds will replace this rpath with a location that
// matches where the library ships (e.g. /usr/local/lib).
#cgo linux LDFLAGS: -Wl,-rpath=${SRCDIR}/../../target/release
#cgo darwin LDFLAGS: -Wl,-rpath,${SRCDIR}/../../target/release

#include <stdlib.h>
#include "sqlrite.h"
*/
import "C"

import (
	"context"
	"database/sql"
	"database/sql/driver"
	"errors"
	"fmt"
	"unsafe"
)

// ---------------------------------------------------------------------------
// Driver registration

// DriverName is the name callers pass to `sql.Open`.
const DriverName = "sqlrite"

func init() {
	sql.Register(DriverName, &sqlriteDriver{})
}

type sqlriteDriver struct{}

// Open implements `driver.Driver`. `name` is the database path (or
// `":memory:"` for a transient in-memory DB, matching SQLite).
func (d *sqlriteDriver) Open(name string) (driver.Conn, error) {
	return newConn(name, false)
}

// OpenReadOnly is a package-level escape hatch — users who want a
// read-only handle can call this directly instead of going through
// `sql.Open`. The `database/sql` API doesn't carry a read-only flag
// through Open, so we offer this as a side door: internally it
// builds a `driver.Connector` that opens each new conn in read-
// only mode, then hands the resulting `*sql.DB` back to the caller.
func OpenReadOnly(name string) *sql.DB {
	return sql.OpenDB(&roConnector{name: name})
}

type roConnector struct{ name string }

// Connect matches driver.Connector. `context.Context` is accepted
// for the signature but unused — the engine has no cancellation
// hook yet.
func (c *roConnector) Connect(_ context.Context) (driver.Conn, error) {
	return newConn(c.name, true)
}
func (c *roConnector) Driver() driver.Driver { return &sqlriteDriver{} }

// ---------------------------------------------------------------------------
// Helpers

// lastError pulls the thread-local last-error string from the FFI.
// Returns an empty string if no error is pending.
func lastError() string {
	p := C.sqlrite_last_error()
	if p == nil {
		return ""
	}
	return C.GoString(p)
}

// Status is a Go-side alias for the C `SqlriteStatus` enum. cgo
// exposes the enum as `uint32` by default (rather than a named type),
// so we work in uint32 internally and compare against the exported
// constants below. The values match the C header byte-for-byte.
type Status uint32

const (
	statusOk              Status = 0
	statusError           Status = 1
	statusInvalidArgument Status = 2
	statusDone            Status = 101
	statusRow             Status = 102
)

// wrapErr returns a Go error when the status code is nonzero. Use
// after any `sqlrite_*` call that can fail.
func wrapErr(status Status, op string) error {
	if status == statusOk {
		return nil
	}
	msg := lastError()
	if msg == "" {
		msg = fmt.Sprintf("SQLRite status %d", uint32(status))
	}
	return fmt.Errorf("sqlrite: %s: %s", op, msg)
}

// cString converts a Go string into a C-allocated NUL-terminated
// copy the caller must `C.free`.
func cString(s string) *C.char { return C.CString(s) }

// isSelect is a coarse heuristic: trim leading whitespace/comments
// and check if the statement starts with `select`. Used to pick
// between `sqlrite_execute` (no rows) and `sqlrite_query` (rows).
// The engine also reports the statement type via its parser, but
// exposing that through the C FFI would add another round-trip per
// call — not worth it for this level of granularity.
func isSelect(sql string) bool {
	for i := 0; i < len(sql); i++ {
		c := sql[i]
		if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
			continue
		}
		// Strip `--` line comments. Block comments (`/* */`) aren't
		// worth the complexity here — users almost never put those
		// before SELECT in practice.
		if c == '-' && i+1 < len(sql) && sql[i+1] == '-' {
			for i < len(sql) && sql[i] != '\n' {
				i++
			}
			continue
		}
		remaining := sql[i:]
		if len(remaining) < 6 {
			return false
		}
		head := remaining[:6]
		return eqFold(head, "select")
	}
	return false
}

// eqFold is an ASCII-only case-insensitive compare — avoids the
// strings.ToLower allocation for this hot path.
func eqFold(a, b string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := 0; i < len(a); i++ {
		ca, cb := a[i], b[i]
		if ca >= 'A' && ca <= 'Z' {
			ca += 'a' - 'A'
		}
		if cb >= 'A' && cb <= 'Z' {
			cb += 'a' - 'A'
		}
		if ca != cb {
			return false
		}
	}
	return true
}

// rejectParamsForNow is the uniform "we don't do parameter binding
// yet" response. Accepted: nil / empty. Anything else is an error.
func rejectParamsForNow(args []driver.Value) error {
	if len(args) == 0 {
		return nil
	}
	return errors.New(
		"sqlrite: parameter binding is not yet supported — inline values into the SQL " +
			"(a future Phase 5a.2 release will add real binding)",
	)
}

func rejectNamedParamsForNow(args []driver.NamedValue) error {
	if len(args) == 0 {
		return nil
	}
	return errors.New(
		"sqlrite: parameter binding is not yet supported — inline values into the SQL " +
			"(a future Phase 5a.2 release will add real binding)",
	)
}

// freeCString is a typed alias so call sites read cleanly.
func freeCString(p *C.char) { C.free(unsafe.Pointer(p)) }
