module sqlrite-example-hello

go 1.21

// While developing in a repo clone, point at the sibling sdk/go
// module via a replace directive. Once Phase 6e tags `sdk/go/v*`
// the standard `require github.com/joaoh82/rust_sqlite/sdk/go v0.x.y`
// line alone will resolve via Go's module proxy.
require github.com/joaoh82/rust_sqlite/sdk/go v0.0.0

replace github.com/joaoh82/rust_sqlite/sdk/go => ../../sdk/go
