// Go module for the SQLRite edge/IoT event collector example (SQLR-43).
//
// This is an end-to-end example app under the SQLR-38 umbrella, not a
// per-SDK quick-start tour (that's `examples/go/`). It embeds the
// SQLRite engine through the Go `database/sql` driver in `sdk/go`,
// which calls into the `libsqlrite_c` cdylib shipped by `sqlrite-ffi`
// over cgo.
//
// While developing from a repo clone we point at the sibling `sdk/go`
// module via a `replace` directive — the same shape `examples/go`
// uses. A standalone consumer would instead `require` a tagged
// `sdk/go/v*` release and point cgo at a prebuilt `libsqlrite_c`
// tarball (see this example's README + `sdk/go/README.md`).
//
// Pinned engine version: sqlrite-engine 0.10.2 (the workspace head at
// the time this example was written). cgo links whatever `libsqlrite_c`
// you built from this checkout — `cargo build --release -p sqlrite-ffi`.
//
// Go 1.21 is the floor, matching `sdk/go`.
module github.com/joaoh82/rust_sqlite/examples/go-collector

go 1.21

require github.com/joaoh82/rust_sqlite/sdk/go v0.0.0

replace github.com/joaoh82/rust_sqlite/sdk/go => ../../sdk/go
