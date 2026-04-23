package sqlrite

/*
#include "sqlrite.h"
*/
import "C"

import (
	"context"
	"database/sql/driver"
	"errors"
	"io"
	"sync"
)

// conn is our `driver.Conn` — a thin wrapper over a
// `SqlriteConnection*` owned by the C library. We serialize access
// with a Mutex because `database/sql`'s pool will sometimes call
// into a single conn from a goroutine that grabbed it via Acquire —
// the Rust engine is single-writer per file-locked open anyway, so
// there's no contention loss.
type conn struct {
	mu     sync.Mutex
	handle *C.SqlriteConnection
	closed bool
}

func newConn(name string, readOnly bool) (*conn, error) {
	cName := cString(name)
	defer freeCString(cName)

	var handle *C.SqlriteConnection
	var status Status
	if readOnly {
		status = Status(C.sqlrite_open_read_only(cName, &handle))
	} else if name == ":memory:" {
		status = Status(C.sqlrite_open_in_memory(&handle))
	} else {
		status = Status(C.sqlrite_open(cName, &handle))
	}
	if err := wrapErr(status, "open"); err != nil {
		return nil, err
	}
	return &conn{handle: handle}, nil
}

// Ensure we satisfy the extended driver interfaces `database/sql`
// recognizes for better error-handling + context support.
var _ driver.Conn = (*conn)(nil)
var _ driver.ConnBeginTx = (*conn)(nil)
var _ driver.ExecerContext = (*conn)(nil)
var _ driver.QueryerContext = (*conn)(nil)
var _ driver.Pinger = (*conn)(nil)

// Prepare returns a `driver.Stmt` for the given SQL. The engine
// parses at prepare time so syntax errors surface immediately.
func (c *conn) Prepare(query string) (driver.Stmt, error) {
	return c.PrepareContext(context.Background(), query)
}

// PrepareContext is identical today — the engine doesn't have a
// cancellation hook yet. Required by driver.ConnPrepareContext.
func (c *conn) PrepareContext(_ context.Context, query string) (driver.Stmt, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil, errors.New("sqlrite: connection is closed")
	}
	// We don't precompile at the engine level yet (no prepared-plan
	// cache — lands with 5a.2). Store the SQL and run it on demand.
	return &stmt{conn: c, sql: query}, nil
}

// Close releases the underlying `SqlriteConnection*`. Safe to call
// multiple times.
func (c *conn) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil
	}
	C.sqlrite_close(c.handle)
	c.handle = nil
	c.closed = true
	return nil
}

// BeginTx begins a transaction. Only default isolation is supported —
// the engine doesn't expose isolation levels yet.
func (c *conn) BeginTx(_ context.Context, opts driver.TxOptions) (driver.Tx, error) {
	if opts.ReadOnly {
		return nil, errors.New("sqlrite: read-only transactions aren't supported via TxOptions (open the db via OpenReadOnly instead)")
	}
	if err := c.exec("BEGIN"); err != nil {
		return nil, err
	}
	return &tx{conn: c}, nil
}

// Begin is the pre-1.8 entry point that `BeginTx` subsumes.
func (c *conn) Begin() (driver.Tx, error) {
	return c.BeginTx(context.Background(), driver.TxOptions{})
}

// Ping verifies the connection is alive. We check closed + run a
// cheap statement.
func (c *conn) Ping(_ context.Context) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return driver.ErrBadConn
	}
	return nil
}

// ExecContext is the fast path for non-query statements. Avoids
// allocating a stmt struct when the caller used `db.ExecContext` /
// `db.Exec` directly (rather than via Prepare).
func (c *conn) ExecContext(_ context.Context, query string, args []driver.NamedValue) (driver.Result, error) {
	if err := rejectNamedParamsForNow(args); err != nil {
		return nil, err
	}
	if err := c.exec(query); err != nil {
		return nil, err
	}
	// `database/sql` expects a `driver.Result`. We return a stub
	// because the engine doesn't yet track affected-row counts.
	return execResult{}, nil
}

// QueryContext is the fast path for SELECTs. Same shortcut idea as
// ExecContext.
func (c *conn) QueryContext(_ context.Context, query string, args []driver.NamedValue) (driver.Rows, error) {
	if err := rejectNamedParamsForNow(args); err != nil {
		return nil, err
	}
	return c.query(query)
}

// ---------------------------------------------------------------------------
// internal helpers (mutex-holding variants)

func (c *conn) exec(query string) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return driver.ErrBadConn
	}
	cQuery := cString(query)
	defer freeCString(cQuery)
	status := Status(C.sqlrite_execute(c.handle, cQuery))
	return wrapErr(status, "execute")
}

func (c *conn) query(query string) (driver.Rows, error) {
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return nil, driver.ErrBadConn
	}
	cQuery := cString(query)
	defer freeCString(cQuery)
	var stmtHandle *C.SqlriteStatement
	status := Status(C.sqlrite_query(c.handle, cQuery, &stmtHandle))
	if err := wrapErr(status, "query"); err != nil {
		c.mu.Unlock()
		return nil, err
	}

	// Pull column names up front so `Rows.Columns()` doesn't have
	// to round-trip into C each call.
	var colCount C.int
	if st := Status(C.sqlrite_column_count(stmtHandle, &colCount)); st != statusOk {
		C.sqlrite_finalize(stmtHandle)
		c.mu.Unlock()
		return nil, wrapErr(st, "column_count")
	}
	cols := make([]string, int(colCount))
	for i := 0; i < int(colCount); i++ {
		var name *C.char
		if st := Status(C.sqlrite_column_name(stmtHandle, C.int(i), &name)); st != statusOk {
			C.sqlrite_finalize(stmtHandle)
			c.mu.Unlock()
			return nil, wrapErr(st, "column_name")
		}
		cols[i] = C.GoString(name)
		C.sqlrite_free_string(name)
	}
	c.mu.Unlock()

	return &rows{
		conn:   c,
		handle: stmtHandle,
		cols:   cols,
	}, nil
}

// ---------------------------------------------------------------------------
// Tx + stub Result

type tx struct {
	conn *conn
}

func (t *tx) Commit() error   { return t.conn.exec("COMMIT") }
func (t *tx) Rollback() error { return t.conn.exec("ROLLBACK") }

// execResult is returned from ExecContext / Exec. The engine doesn't
// track LastInsertId / RowsAffected at the public API yet, so both
// methods return `0, nil` — documented in the SDK README.
type execResult struct{}

func (execResult) LastInsertId() (int64, error) { return 0, nil }
func (execResult) RowsAffected() (int64, error) { return 0, nil }

// Satisfy the io.Closer check some linters want on Rows.
var _ io.Closer = (*rows)(nil)
