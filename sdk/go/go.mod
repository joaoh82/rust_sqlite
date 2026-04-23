// Go module for the SQLRite `database/sql` driver.
//
// Import path is `github.com/joaoh82/rust_sqlite/sdk/go` — the
// repo-relative layout lets Phase 6e publish via `sdk/go/v*.*.*`
// git tags (Go modules resolve straight from git, no central
// registry to push to).
//
// Go 1.21 is the floor because we rely on stdlib `database/sql/driver`
// iterator shapes settled in that release.
module github.com/joaoh82/rust_sqlite/sdk/go

go 1.21
