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
