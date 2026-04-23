package sqlrite

/*
#include "sqlrite.h"
*/
import "C"

import (
	"database/sql/driver"
	"fmt"
	"io"
)

// rows implements driver.Rows. Holds the C-side `SqlriteStatement*`
// handle and advances it once per `Next(dest)` call. The column
// count + names are captured at construction time so `Columns()`
// doesn't have to cross the FFI boundary on every call.
type rows struct {
	conn   *conn
	handle *C.SqlriteStatement
	cols   []string
	closed bool
}

var _ driver.Rows = (*rows)(nil)

// Columns returns the cached projection-order column names.
func (r *rows) Columns() []string {
	// database/sql guarantees this is never called concurrently with
	// Next on the same Rows — we can return the slice directly.
	return r.cols
}

// Close releases the underlying statement handle. Safe to call
// multiple times.
func (r *rows) Close() error {
	if r.closed {
		return nil
	}
	r.closed = true
	if r.handle != nil {
		C.sqlrite_finalize(r.handle)
		r.handle = nil
	}
	return nil
}

// Next advances to the next row and scans columns into dest.
// Returns io.EOF when the result set is exhausted — per the
// `database/sql/driver` contract.
func (r *rows) Next(dest []driver.Value) error {
	if r.closed || r.handle == nil {
		return io.EOF
	}
	r.conn.mu.Lock()
	defer r.conn.mu.Unlock()

	status := Status(C.sqlrite_step(r.handle))
	switch status {
	case statusDone:
		return io.EOF
	case statusRow:
		// fall through
	default:
		return wrapErr(status, "step")
	}

	// Populate each dest[i] by sniffing IS_NULL and then picking
	// the right typed accessor. Type information isn't exposed by
	// the FFI yet, so we prefer int64 → double → text fallbacks:
	// try each accessor in turn and use the first that succeeds
	// without an error.
	for i := 0; i < len(dest); i++ {
		if i >= len(r.cols) {
			// More `dest` slots than columns. database/sql won't
			// normally do this, but be defensive.
			dest[i] = nil
			continue
		}
		v, err := readColumn(r.handle, C.int(i))
		if err != nil {
			return err
		}
		dest[i] = v
	}
	return nil
}

// readColumn reads column `idx` of the current row as a Go value.
// Strategy: check IS_NULL first; then try int64 → double → text in
// that order. (Bool doesn't round-trip through our Value enum — the
// engine stores Bool as Value::Bool but we don't have a dedicated
// FFI accessor; `sqlrite_column_int64` falls back on the display
// form for non-ints, same as sqlite3.)
func readColumn(handle *C.SqlriteStatement, idx C.int) (driver.Value, error) {
	var isNull C.int
	if st := Status(C.sqlrite_column_is_null(handle, idx, &isNull)); st != statusOk {
		return nil, wrapErr(st, "is_null")
	}
	if isNull != 0 {
		return nil, nil
	}

	// Try int64 first. The engine errors with "cannot convert" for
	// non-int types, which we use as a type sniff.
	var i int64
	if st := Status(C.sqlrite_column_int64(handle, idx, (*C.int64_t)(&i))); st == statusOk {
		return i, nil
	}

	// Try double next. Engine coerces Integer → Double for us
	// already; we land here on Real / other numeric-y types.
	var d C.double
	if st := Status(C.sqlrite_column_double(handle, idx, &d)); st == statusOk {
		return float64(d), nil
	}

	// Fall back to text. `sqlrite_column_text` is deliberately
	// lenient — it renders Int/Real/Bool via their Display if the
	// column isn't Text. So this is the catch-all.
	var cstr *C.char
	if st := Status(C.sqlrite_column_text(handle, idx, &cstr)); st == statusOk {
		defer C.sqlrite_free_string(cstr)
		return C.GoString(cstr), nil
	}

	return nil, fmt.Errorf("sqlrite: failed to read column %d", int(idx))
}
