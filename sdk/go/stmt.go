package sqlrite

/*
#include "sqlrite.h"
*/
import "C"

import (
	"context"
	"database/sql/driver"
	"errors"
)

// stmt implements driver.Stmt / driver.StmtExecContext /
// driver.StmtQueryContext. The engine doesn't have prepared-plan
// caching yet (Phase 5a.2), so stmt just holds the SQL string and
// re-prepares on each execution.
type stmt struct {
    conn *conn
    sql  string
}

var _ driver.Stmt = (*stmt)(nil)
var _ driver.StmtExecContext = (*stmt)(nil)
var _ driver.StmtQueryContext = (*stmt)(nil)

// Close is a no-op today — we don't hold onto any C-side handle
// until the actual exec/query runs.
func (s *stmt) Close() error {
    s.conn = nil
    return nil
}

// NumInput returns -1 because we don't track bind parameters yet
// (and the engine doesn't support them). Per `database/sql/driver`:
// returning -1 means "any number is OK; don't sanity-check".
func (s *stmt) NumInput() int { return -1 }

// Exec is the legacy pre-1.8 entry point. ExecContext is what
// modern callers hit.
func (s *stmt) Exec(args []driver.Value) (driver.Result, error) {
    if err := rejectParamsForNow(args); err != nil {
        return nil, err
    }
    return s.ExecContext(context.Background(), valuesToNamed(args))
}

// ExecContext runs a non-query statement.
func (s *stmt) ExecContext(_ context.Context, args []driver.NamedValue) (driver.Result, error) {
    if err := rejectNamedParamsForNow(args); err != nil {
        return nil, err
    }
    if s.conn == nil {
        return nil, errors.New("sqlrite: stmt is closed")
    }
    if err := s.conn.exec(s.sql); err != nil {
        return nil, err
    }
    return execResult{}, nil
}

// Query is the legacy pre-1.8 entry point.
func (s *stmt) Query(args []driver.Value) (driver.Rows, error) {
    if err := rejectParamsForNow(args); err != nil {
        return nil, err
    }
    return s.QueryContext(context.Background(), valuesToNamed(args))
}

// valuesToNamed is the equivalent of the stdlib's `database/sql`
// internal helper of the same name — it re-packages positional
// args from the legacy `driver.Stmt.Exec/Query` signature into the
// `driver.NamedValue` form the context-aware methods accept. We
// implement it ourselves because the stdlib doesn't export it.
func valuesToNamed(args []driver.Value) []driver.NamedValue {
    out := make([]driver.NamedValue, len(args))
    for i, v := range args {
        out[i] = driver.NamedValue{Ordinal: i + 1, Value: v}
    }
    return out
}

// QueryContext runs a SELECT.
func (s *stmt) QueryContext(_ context.Context, args []driver.NamedValue) (driver.Rows, error) {
    if err := rejectNamedParamsForNow(args); err != nil {
        return nil, err
    }
    if s.conn == nil {
        return nil, errors.New("sqlrite: stmt is closed")
    }
    if !isSelect(s.sql) {
        // Match database/sql's expectation: Query must run a query
        // that returns rows. If the user prepared a non-query via
        // .Prepare and calls Query on it, surface a clear error.
        return nil, errors.New("sqlrite: Query called on a non-SELECT statement — use Exec")
    }
    return s.conn.query(s.sql)
}
