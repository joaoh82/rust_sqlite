// Minimal walkthrough of the SQLRite Go driver.
//
// Prerequisite: build libsqlrite_c once from the repo root:
//
//	cargo build --release -p sqlrite-ffi
//
// Then from inside examples/go/:
//
//	go run hello.go
//
// Shape mirrors the standard library's `database/sql` API — if you've
// used any Go SQL driver (pq, mysql, sqlite3), you already know how
// to drive this.
package main

import (
	"database/sql"
	"fmt"
	"log"

	// Side-effect import registers "sqlrite" as a sql.Open driver name.
	_ "github.com/joaoh82/rust_sqlite/sdk/go"
)

func main() {
	// Pass `:memory:` for a transient in-memory DB (matching SQLite
	// convention); pass a file path like "foo.sqlrite" for a file-
	// backed DB that auto-saves on every write.
	db, err := sql.Open("sqlrite", ":memory:")
	if err != nil {
		log.Fatalf("open: %v", err)
	}
	defer db.Close()

	mustExec(db, "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")
	mustExec(db, "INSERT INTO users (name, age) VALUES ('alice', 30)")
	mustExec(db, "INSERT INTO users (name, age) VALUES ('bob', 25)")
	mustExec(db, "INSERT INTO users (name, age) VALUES ('carol', 40)")

	// database/sql's Query + Scan — typed into Go primitives.
	fmt.Println("All users:")
	rows, err := db.Query("SELECT id, name, age FROM users")
	if err != nil {
		log.Fatalf("query: %v", err)
	}
	for rows.Next() {
		var id, age int64
		var name string
		if err := rows.Scan(&id, &name, &age); err != nil {
			log.Fatalf("scan: %v", err)
		}
		fmt.Printf("  %d: %s (%d)\n", id, name, age)
	}
	rows.Close()

	// Transactions via database/sql: Begin, issue statements, commit
	// or rollback. The engine treats this the same as `BEGIN` /
	// `COMMIT` / `ROLLBACK` statements via Exec — auto-save is
	// suppressed until the transaction commits, and Rollback
	// restores the pre-BEGIN snapshot.
	fmt.Println()
	tx, err := db.Begin()
	if err != nil {
		log.Fatalf("begin: %v", err)
	}
	mustTxExec(tx, "INSERT INTO users (name, age) VALUES ('phantom', 99)")
	fmt.Printf("Mid-transaction row count: %d\n", countRows(tx, "SELECT id FROM users"))

	if err := tx.Rollback(); err != nil {
		log.Fatalf("rollback: %v", err)
	}
	fmt.Printf("Post-rollback row count:   %d\n", countRows(db, "SELECT id FROM users"))
}

// Small helpers — `database/sql`'s idiomatic error-handling gets
// noisy in example code, so we factor out two tiny wrappers.

type queryer interface {
	Query(query string, args ...any) (*sql.Rows, error)
}

func mustExec(db *sql.DB, q string) {
	if _, err := db.Exec(q); err != nil {
		log.Fatalf("exec %q: %v", q, err)
	}
}

func mustTxExec(tx *sql.Tx, q string) {
	if _, err := tx.Exec(q); err != nil {
		log.Fatalf("tx.Exec %q: %v", q, err)
	}
}

func countRows(q queryer, sqlText string) int {
	rows, err := q.Query(sqlText)
	if err != nil {
		log.Fatalf("query: %v", err)
	}
	defer rows.Close()
	n := 0
	for rows.Next() {
		n++
	}
	return n
}
