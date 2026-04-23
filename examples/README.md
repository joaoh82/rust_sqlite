# Examples

Working samples for every language that embeds the SQLRite engine.

Phase 5 lands these incrementally — each sub-phase fills in one language. The directory layout is designed so new SDKs slot in cleanly without disturbing the existing ones.

| Language | Status | SDK published | Directory |
|----------|--------|---------------|-----------|
| Rust     | ✅ Phase 5a       | crates.io (Phase 6c) | [`rust/`](rust/)     |
| C (FFI)  | 🚧 Phase 5b       | GitHub Releases (Phase 6d) | _coming soon_ |
| Python   | 🚧 Phase 5c       | PyPI (Phase 6e)      | _coming soon_ |
| Node.js  | 🚧 Phase 5d       | npm (Phase 6e)       | _coming soon_ |
| Go       | 🚧 Phase 5e       | Go modules (Phase 6e)| _coming soon_ |
| WASM     | 🚧 Phase 5g       | npm as `sqlrite-wasm` (Phase 6e) | _coming soon_ |

See [docs/roadmap.md](../docs/roadmap.md) for what each sub-phase delivers.

## Running the Rust quickstart

```bash
cargo run --example quickstart
```

Walks through opening an in-memory `Connection`, creating a table, inserting rows, preparing a SELECT, iterating typed `Row` values, and running a `BEGIN` / `ROLLBACK` block. About 50 lines with comments — read [`rust/quickstart.rs`](rust/quickstart.rs) first.

## Design notes

- **One shape across languages.** Every SDK exposes `Connection`, `prepare`, `execute`, and typed `Row` access. The language-specific file in each subdir shows the same CRUD + transaction walkthrough, so users picking up a new binding recognize the surface immediately.
- **No build step required for end users.** Phase 6 ships prebuilt wheels (Python), `.node` binaries (Node.js), `libsqlrite.{so,dylib,dll}` (for Go / C), and `sqlrite-wasm` (browser) — no "install Rust first" tax.
- **Examples track the engine.** Each sub-phase's commit lands the example alongside the binding itself, so the sample always works against the current library shape.
