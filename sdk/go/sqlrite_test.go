// End-to-end tests for the SQLRite `database/sql` driver.
//
// Prerequisite: `cargo build --release -p sqlrite-ffi` at the repo
// root so `libsqlrite_c` is available for cgo to link against. The
// tests then run with the standard `go test ./...`.
//
// These walk the full `database/sql` → driver → cgo → Rust → SQLRite
// pipeline, so a passing suite is strong evidence the driver is
// usable from real Go code.

package sqlrite_test

import (
	"database/sql"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	sqlrite "github.com/joaoh82/rust_sqlite/sdk/go"
)

// openMem returns a fresh in-memory DB via the driver.
func openMem(t *testing.T) *sql.DB {
	t.Helper()
	db, err := sql.Open(sqlrite.DriverName, ":memory:")
	if err != nil {
		t.Fatalf("sql.Open: %v", err)
	}
	t.Cleanup(func() { db.Close() })
	return db
}

// openFile returns a fresh file-backed DB in a temp directory that
// gets cleaned up at test end.
func openFile(t *testing.T) (*sql.DB, string) {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "test.sqlrite")
	db, err := sql.Open(sqlrite.DriverName, path)
	if err != nil {
		t.Fatalf("sql.Open: %v", err)
	}
	t.Cleanup(func() { db.Close() })
	return db, path
}

// ---------------------------------------------------------------------------
// Basic CRUD

func TestInMemoryRoundTrip(t *testing.T) {
	db := openMem(t)
	mustExec(t, db, "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")
	mustExec(t, db, "INSERT INTO users (name, age) VALUES ('alice', 30)")
	mustExec(t, db, "INSERT INTO users (name, age) VALUES ('bob', 25)")

	rows, err := db.Query("SELECT id, name, age FROM users")
	if err != nil {
		t.Fatalf("Query: %v", err)
	}
	defer rows.Close()

	type user struct {
		id   int64
		name string
		age  int64
	}
	var out []user
	for rows.Next() {
		var u user
		if err := rows.Scan(&u.id, &u.name, &u.age); err != nil {
			t.Fatalf("Scan: %v", err)
		}
		out = append(out, u)
	}
	if err := rows.Err(); err != nil {
		t.Fatalf("rows.Err: %v", err)
	}
	if len(out) != 2 {
		t.Fatalf("want 2 rows, got %d", len(out))
	}
	if out[0].name != "alice" || out[0].age != 30 {
		t.Errorf("row[0]: %+v", out[0])
	}
	if out[1].name != "bob" || out[1].age != 25 {
		t.Errorf("row[1]: %+v", out[1])
	}
}

func TestQueryRowScansSingleRow(t *testing.T) {
	db := openMem(t)
	mustExec(t, db, "CREATE TABLE t (x INTEGER PRIMARY KEY)")
	mustExec(t, db, "INSERT INTO t (x) VALUES (42)")

	var x int64
	if err := db.QueryRow("SELECT x FROM t").Scan(&x); err != nil {
		t.Fatalf("QueryRow.Scan: %v", err)
	}
	if x != 42 {
		t.Errorf("want 42, got %d", x)
	}
}

func TestColumnsReportProjectionOrder(t *testing.T) {
	db := openMem(t)
	mustExec(t, db, "CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT, c TEXT)")
	mustExec(t, db, "INSERT INTO t (a, b, c) VALUES (1, 'x', 'y')")

	rows, err := db.Query("SELECT a, b, c FROM t")
	if err != nil {
		t.Fatalf("Query: %v", err)
	}
	defer rows.Close()
	cols, err := rows.Columns()
	if err != nil {
		t.Fatalf("Columns: %v", err)
	}
	want := []string{"a", "b", "c"}
	if len(cols) != 3 || cols[0] != want[0] || cols[1] != want[1] || cols[2] != want[2] {
		t.Errorf("want %v, got %v", want, cols)
	}
}

// ---------------------------------------------------------------------------
// Transactions

func TestTransactionCommitPersistsRows(t *testing.T) {
	db := openMem(t)
	mustExec(t, db, "CREATE TABLE t (x INTEGER PRIMARY KEY, note TEXT)")

	tx, err := db.Begin()
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}
	if _, err := tx.Exec("INSERT INTO t (note) VALUES ('a')"); err != nil {
		t.Fatalf("tx.Exec: %v", err)
	}
	if _, err := tx.Exec("INSERT INTO t (note) VALUES ('b')"); err != nil {
		t.Fatalf("tx.Exec: %v", err)
	}
	if err := tx.Commit(); err != nil {
		t.Fatalf("Commit: %v", err)
	}

	notes := collectStrings(t, db, "SELECT note FROM t")
	if len(notes) != 2 || notes[0] != "a" || notes[1] != "b" {
		t.Errorf("want [a b], got %v", notes)
	}
}

func TestTransactionRollbackRestoresState(t *testing.T) {
	db := openMem(t)
	mustExec(t, db, "CREATE TABLE t (id INTEGER PRIMARY KEY, note TEXT)")
	mustExec(t, db, "INSERT INTO t (note) VALUES ('persistent')")

	tx, err := db.Begin()
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}
	if _, err := tx.Exec("INSERT INTO t (note) VALUES ('doomed')"); err != nil {
		t.Fatalf("tx.Exec: %v", err)
	}
	if err := tx.Rollback(); err != nil {
		t.Fatalf("Rollback: %v", err)
	}

	notes := collectStrings(t, db, "SELECT note FROM t")
	if len(notes) != 1 || notes[0] != "persistent" {
		t.Errorf("want [persistent], got %v", notes)
	}
}

// ---------------------------------------------------------------------------
// File-backed + read-only

func TestFileBackedPersistsAcrossConnections(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "test.sqlrite")

	{
		db, err := sql.Open(sqlrite.DriverName, path)
		if err != nil {
			t.Fatalf("Open: %v", err)
		}
		mustExec(t, db, "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT)")
		mustExec(t, db, "INSERT INTO items (label) VALUES ('a')")
		mustExec(t, db, "INSERT INTO items (label) VALUES ('b')")
		db.Close()
	}

	// Confirm the file is actually there.
	if _, err := os.Stat(path); err != nil {
		t.Fatalf("db file missing: %v", err)
	}

	db2, err := sql.Open(sqlrite.DriverName, path)
	if err != nil {
		t.Fatalf("Open #2: %v", err)
	}
	defer db2.Close()
	labels := collectStrings(t, db2, "SELECT label FROM items")
	if len(labels) != 2 {
		t.Fatalf("want 2 rows, got %d: %v", len(labels), labels)
	}
}

func TestOpenReadOnlyRejectsWrites(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "test.sqlrite")

	// Seed a file.
	{
		db, err := sql.Open(sqlrite.DriverName, path)
		if err != nil {
			t.Fatalf("seed Open: %v", err)
		}
		mustExec(t, db, "CREATE TABLE t (id INTEGER PRIMARY KEY, note TEXT)")
		mustExec(t, db, "INSERT INTO t (note) VALUES ('hello')")
		db.Close()
	}

	ro := sqlrite.OpenReadOnly(path)
	defer ro.Close()

	// Reads work.
	notes := collectStrings(t, ro, "SELECT note FROM t")
	if len(notes) != 1 || notes[0] != "hello" {
		t.Errorf("want [hello], got %v", notes)
	}

	// Writes don't.
	_, err := ro.Exec("INSERT INTO t (note) VALUES ('doomed')")
	if err == nil {
		t.Fatal("expected write to fail on read-only db")
	}
	if !strings.Contains(err.Error(), "read-only") {
		t.Errorf("error should mention read-only, got: %v", err)
	}
}

// ---------------------------------------------------------------------------
// Error paths

func TestBadSQLBubblesUpAsError(t *testing.T) {
	db := openMem(t)
	_, err := db.Exec("THIS IS NOT SQL")
	if err == nil {
		t.Fatal("expected an error on garbage SQL")
	}
}

// ---------------------------------------------------------------------------
// Phase 11.7 — BEGIN CONCURRENT / Busy sentinel errors

func TestBusySentinelsAreDistinctErrors(t *testing.T) {
	if sqlrite.ErrBusy == nil {
		t.Fatal("sqlrite.ErrBusy is nil")
	}
	if sqlrite.ErrBusySnapshot == nil {
		t.Fatal("sqlrite.ErrBusySnapshot is nil")
	}
	// Sanity: the two sentinels are independent values.
	if errors.Is(sqlrite.ErrBusy, sqlrite.ErrBusySnapshot) {
		t.Error("sqlrite.ErrBusy must not match sqlrite.ErrBusySnapshot via errors.Is")
	}
	if errors.Is(sqlrite.ErrBusySnapshot, sqlrite.ErrBusy) {
		t.Error("sqlrite.ErrBusySnapshot must not match sqlrite.ErrBusy via errors.Is")
	}
}

func TestIsRetryableCoversBothSentinels(t *testing.T) {
	if !sqlrite.IsRetryable(sqlrite.ErrBusy) {
		t.Error("sqlrite.IsRetryable(sqlrite.ErrBusy) should be true")
	}
	if !sqlrite.IsRetryable(sqlrite.ErrBusySnapshot) {
		t.Error("sqlrite.IsRetryable(sqlrite.ErrBusySnapshot) should be true")
	}
	if sqlrite.IsRetryable(errors.New("not a busy error")) {
		t.Error("sqlrite.IsRetryable on a generic error should be false")
	}
	if sqlrite.IsRetryable(nil) {
		t.Error("sqlrite.IsRetryable(nil) should be false")
	}
	// Wrapped errors flow through errors.Is — retry loops use
	// `fmt.Errorf("... %w", sqlrite.ErrBusy)` shape, so we verify the
	// helper recognises wrapped variants too.
	wrapped := fmt.Errorf("commit failed: %w", sqlrite.ErrBusy)
	if !sqlrite.IsRetryable(wrapped) {
		t.Error("sqlrite.IsRetryable should unwrap %w to find sqlrite.ErrBusy")
	}
}

func TestJournalModeMvccReachesGoDriver(t *testing.T) {
	// Sanity that `PRAGMA journal_mode = mvcc` reaches the engine
	// through cgo. BEGIN CONCURRENT itself isn't usefully
	// exercisable through `database/sql` today (the driver
	// doesn't expose sibling Connection handles per the Phase
	// 11.1 multi-connection contract), but PRAGMA accepts and the
	// `BEGIN CONCURRENT` gate flips, which proves the cgo
	// plumbing is right.
	//
	// Note: PRAGMA renders a single-row result in the engine's
	// `CommandOutput.rendered`, but the Go driver routes non-SELECT
	// statements through `sqlrite_execute` (no rows), so we don't
	// try to read the value back through `db.Query`.
	db := openMem(t)
	mustExec(t, db, "PRAGMA journal_mode = mvcc")
	// BEGIN CONCURRENT only succeeds once journal_mode is mvcc;
	// the gate proves the toggle landed.
	mustExec(t, db, "CREATE TABLE t (id INTEGER PRIMARY KEY)")
	mustExec(t, db, "BEGIN CONCURRENT")
	mustExec(t, db, "ROLLBACK")
	// Unknown values still error cleanly (regression guard for
	// the PRAGMA dispatcher).
	if _, err := db.Exec("PRAGMA journal_mode = nonsense"); err == nil {
		t.Fatal("expected unknown journal_mode to error")
	}
}

// ---------------------------------------------------------------------------
// Phase 11.11c — cross-pool sibling sharing via the path registry

// TestTwoSqlOpenOnSameFileShareState verifies that two independent
// `*sql.DB` instances pointing at the same path share the same
// backing engine state — a row written through db1 is immediately
// visible through db2, WITHOUT closing db1 first. Pre-11.11c the
// second `sql.Open` for an already-open file would deadlock on
// `flock(LOCK_EX)`.
func TestTwoSqlOpenOnSameFileShareState(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "shared.sqlrite")

	db1, err := sql.Open(sqlrite.DriverName, path)
	if err != nil {
		t.Fatalf("sql.Open db1: %v", err)
	}
	defer db1.Close()
	// Force db1 to actually acquire a connection so the registry
	// entry is live before db2 opens.
	mustExec(t, db1, "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT)")
	mustExec(t, db1, "INSERT INTO items (label) VALUES ('via-db1')")

	db2, err := sql.Open(sqlrite.DriverName, path)
	if err != nil {
		t.Fatalf("sql.Open db2: %v", err)
	}
	defer db2.Close()

	// db2 should see the row db1 wrote — they share `Arc<Mutex<Database>>`.
	labels := collectStrings(t, db2, "SELECT label FROM items")
	if len(labels) != 1 || labels[0] != "via-db1" {
		t.Fatalf("db2 sees %v, want [via-db1]", labels)
	}

	// And a write via db2 surfaces through db1 — bidirectional.
	mustExec(t, db2, "INSERT INTO items (label) VALUES ('via-db2')")
	labels1 := collectStrings(t, db1, "SELECT label FROM items ORDER BY id")
	if len(labels1) != 2 || labels1[0] != "via-db1" || labels1[1] != "via-db2" {
		t.Errorf("db1 sees %v, want [via-db1, via-db2]", labels1)
	}
}

// TestBeginConcurrentAcrossSqlOpenInstances exercises the headline
// 11.11c use case: two `*sql.DB` instances over the same path each
// hold their own `BEGIN CONCURRENT`, the first commit wins, the
// second hits `ErrBusy` and a retry succeeds.
//
// Without the path registry this test would deadlock at db2's
// open (flock conflict) before any tx machinery ran. With it,
// both pools mint sibling handles off a shared primary, so each
// pool's pinned `*sql.Conn` carries its own per-connection
// `ConcurrentTx` slot.
func TestBeginConcurrentAcrossSqlOpenInstances(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "concurrent.sqlrite")

	db1, err := sql.Open(sqlrite.DriverName, path)
	if err != nil {
		t.Fatalf("sql.Open db1: %v", err)
	}
	defer db1.Close()
	mustExec(t, db1, "PRAGMA journal_mode = mvcc")
	mustExec(t, db1, "CREATE TABLE counters (id INTEGER PRIMARY KEY, n INTEGER NOT NULL)")
	mustExec(t, db1, "INSERT INTO counters (id, n) VALUES (1, 0)")

	db2, err := sql.Open(sqlrite.DriverName, path)
	if err != nil {
		t.Fatalf("sql.Open db2: %v", err)
	}
	defer db2.Close()

	// Pin one driver-level conn out of each pool so the BEGIN /
	// COMMIT sequence stays on the same underlying handle for the
	// whole transaction. `database/sql` would otherwise be free to
	// round-robin successive Exec calls across pool slots.
	ctx := t.Context()
	connA, err := db1.Conn(ctx)
	if err != nil {
		t.Fatalf("db1.Conn: %v", err)
	}
	defer connA.Close()
	connB, err := db2.Conn(ctx)
	if err != nil {
		t.Fatalf("db2.Conn: %v", err)
	}
	defer connB.Close()

	mustExecConn := func(c *sql.Conn, q string) {
		t.Helper()
		if _, err := c.ExecContext(ctx, q); err != nil {
			t.Fatalf("%s: %v", q, err)
		}
	}

	// Interleave BEGINs so connA.begin_ts < connB.begin_ts and
	// both see the same pre-update value.
	mustExecConn(connA, "BEGIN CONCURRENT")
	mustExecConn(connB, "BEGIN CONCURRENT")
	mustExecConn(connA, "UPDATE counters SET n = n + 1 WHERE id = 1")
	mustExecConn(connB, "UPDATE counters SET n = n + 100 WHERE id = 1")

	// connA commits first → succeeds (no version newer than A.begin_ts yet).
	mustExecConn(connA, "COMMIT")
	// connB's commit now collides with connA's commit. Expect ErrBusy.
	if _, err := connB.ExecContext(ctx, "COMMIT"); err == nil {
		t.Fatal("connB COMMIT should have hit a write-write conflict, got nil")
	} else if !errors.Is(err, sqlrite.ErrBusy) {
		t.Fatalf("connB COMMIT: want ErrBusy, got %v", err)
	}

	// Retry on connB picks up connA's committed value and lands.
	mustExecConn(connB, "BEGIN CONCURRENT")
	mustExecConn(connB, "UPDATE counters SET n = n + 100 WHERE id = 1")
	mustExecConn(connB, "COMMIT")

	// Final value should be 0 + 1 (from A) + 100 (from B's retry).
	rows, err := db1.QueryContext(ctx, "SELECT n FROM counters WHERE id = 1")
	if err != nil {
		t.Fatalf("final SELECT: %v", err)
	}
	defer rows.Close()
	if !rows.Next() {
		t.Fatal("expected one row")
	}
	var n int
	if err := rows.Scan(&n); err != nil {
		t.Fatalf("scan: %v", err)
	}
	if n != 101 {
		t.Errorf("final counter = %d, want 101", n)
	}
}

// TestRegistryRefcountDropsToZeroOnLastClose verifies the
// registry's bookkeeping: after the last sibling closes the
// entry is removed (so the next `sql.Open` for the same path
// pays for a fresh `sqlrite_open` rather than minting a sibling
// off a stale primary).
func TestRegistryRefcountDropsToZeroOnLastClose(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "refcount.sqlrite")

	db1, err := sql.Open(sqlrite.DriverName, path)
	if err != nil {
		t.Fatalf("sql.Open db1: %v", err)
	}
	mustExec(t, db1, "CREATE TABLE t (id INTEGER PRIMARY KEY)")
	db2, err := sql.Open(sqlrite.DriverName, path)
	if err != nil {
		t.Fatalf("sql.Open db2: %v", err)
	}
	// Pin a conn from each pool so refcount > 1.
	c1, err := db1.Conn(t.Context())
	if err != nil {
		t.Fatalf("db1.Conn: %v", err)
	}
	c2, err := db2.Conn(t.Context())
	if err != nil {
		t.Fatalf("db2.Conn: %v", err)
	}

	// Now we have 2 outstanding siblings.
	if c1.Close() != nil {
		t.Fatal("c1.Close")
	}
	if c2.Close() != nil {
		t.Fatal("c2.Close")
	}
	if err := db1.Close(); err != nil {
		t.Fatalf("db1.Close: %v", err)
	}
	if err := db2.Close(); err != nil {
		t.Fatalf("db2.Close: %v", err)
	}

	// After everything closes, a new sql.Open on the same path
	// must succeed (the registry entry has been removed and the
	// flock released). Pre-11.11c this is what the existing
	// `TestFileBackedPersistsAcrossConnections` already
	// verified; here we re-prove it AFTER siblings have been in
	// play.
	db3, err := sql.Open(sqlrite.DriverName, path)
	if err != nil {
		t.Fatalf("post-close re-open: %v", err)
	}
	defer db3.Close()
	mustExec(t, db3, "INSERT INTO t (id) VALUES (1)")
}

func TestNonEmptyParametersRejected(t *testing.T) {
	db := openMem(t)
	mustExec(t, db, "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")

	// Zero args is fine.
	if _, err := db.Exec("INSERT INTO t (name) VALUES ('x')"); err != nil {
		t.Fatalf("zero-arg Exec: %v", err)
	}

	// Non-empty args should return a clear error.
	_, err := db.Exec("INSERT INTO t (name) VALUES (?)", "y")
	if err == nil {
		t.Fatal("expected parameter-binding error")
	}
	if !strings.Contains(err.Error(), "parameter binding") {
		t.Errorf("error should mention parameter binding, got: %v", err)
	}
}

// ---------------------------------------------------------------------------
// Helpers

func mustExec(t *testing.T, db *sql.DB, query string) {
	t.Helper()
	if _, err := db.Exec(query); err != nil {
		t.Fatalf("Exec %q: %v", query, err)
	}
}

func collectStrings(t *testing.T, db *sql.DB, query string) []string {
	t.Helper()
	rows, err := db.Query(query)
	if err != nil {
		t.Fatalf("Query: %v", err)
	}
	defer rows.Close()
	var out []string
	for rows.Next() {
		var s string
		if err := rows.Scan(&s); err != nil {
			t.Fatalf("Scan: %v", err)
		}
		out = append(out, s)
	}
	return out
}
