# Examples

Working samples for every language that embeds the SQLRite engine.

Phase 5 lands these incrementally — each sub-phase fills in one language. The directory layout is designed so new SDKs slot in cleanly without disturbing the existing ones.

| Language | Status | SDK published | Directory |
|----------|--------|---------------|-----------|
| Rust     | ✅ Phase 5a       | crates.io as `sqlrite-engine` (Phase 6d) | [`rust/`](rust/)     |
| C (FFI)  | ✅ Phase 5b       | GitHub Releases (Phase 6d) | [`c/`](c/)           |
| Python   | ✅ Phase 5c       | PyPI as `sqlrite` (Phase 6f) | [`python/`](python/) |
| Node.js  | ✅ Phase 5d       | npm as `@joaoh82/sqlrite` (Phase 6g) | [`nodejs/`](nodejs/) |
| Go       | ✅ Phase 5e       | Go modules (Phase 6i) | [`go/`](go/)         |
| WASM     | ✅ Phase 5g       | npm as `@joaoh82/sqlrite-wasm` (Phase 6h) | [`wasm/`](wasm/)     |
| MCP      | ✅ Phase 7h       | crates.io as `sqlrite-mcp` + per-platform binary tarballs | *(no `examples/` subdir — see [`docs/mcp.md`](../docs/mcp.md) for the full quickstart with Claude Code / Cursor / `mcp-inspector` wiring snippets)* |

See [docs/roadmap.md](../docs/roadmap.md) for what each sub-phase delivers.

## Running the Rust quickstart

```bash
cargo run --example quickstart
```

Walks through opening an in-memory `Connection`, creating a table, inserting rows, preparing a SELECT, iterating typed `Row` values, and running a `BEGIN` / `ROLLBACK` block. About 50 lines with comments — read [`rust/quickstart.rs`](rust/quickstart.rs) first.

## Running the hybrid-retrieval example (Phase 8d)

```bash
cargo run --example hybrid-retrieval
```

Combines BM25 lexical scoring (Phase 8b) with vector cosine distance (Phase 7d) in a single `ORDER BY`, showing where each ranking shape wins on the same corpus. Pre-baked vectors keep the example self-contained — no embedding-model dependency. Read [`hybrid-retrieval/README.md`](hybrid-retrieval/README.md) for the narrative.

## Running the C sample

```bash
cd examples/c && make run
```

Builds the Rust cdylib (`libsqlrite_c.{so,dylib,dll}`) and compiles [`c/hello.c`](c/hello.c) against its generated header. The binary embeds an rpath pointing at the cargo target dir so `./hello` runs without any `LD_LIBRARY_PATH` / `DYLD_*` dance. Covers open → execute → query → step → column accessors + an explicit transaction block.

See the top of [`c/hello.c`](c/hello.c) for the ownership rules that apply to every non-Rust binding (opaque handles, `sqlrite_free_string` for text columns, thread-local `sqlrite_last_error`).

## Running the Python sample

```bash
# One-time: install maturin and build the wheel into your Python env.
pip install maturin
cd sdk/python && maturin develop

# Then from the repo root:
python examples/python/hello.py
```

Mirrors the Rust quickstart shape via the DB-API: `sqlrite.connect(":memory:")` → `cursor.execute` → iterate tuples, plus a BEGIN/ROLLBACK block. See [`python/hello.py`](python/hello.py) and [`sdk/python/README.md`](../sdk/python/README.md) for the full API tour.

## Running the Node.js sample

```bash
# One-time: build the .node binding.
cd sdk/nodejs
npm install
npm run build

# Then from the repo root:
node examples/nodejs/hello.mjs
```

Mirrors the `better-sqlite3` shape: `new Database(":memory:")` → `db.prepare(sql).all()` returning row objects, plus a BEGIN/ROLLBACK block with the `inTransaction` getter. See [`nodejs/hello.mjs`](nodejs/hello.mjs) and [`sdk/nodejs/README.md`](../sdk/nodejs/README.md) for the full API tour.

## Running the Go sample

```bash
# One-time: build the C shared library (the Go driver is cgo-linked).
cargo build --release -p sqlrite-ffi

# Then:
cd examples/go
go run hello.go
```

Uses the standard library's `database/sql` API — `sql.Open("sqlrite", ":memory:")` → `db.Query` + `rows.Scan(&id, &name)` into typed Go vars, plus a `db.Begin() / tx.Rollback()` block. See [`go/hello.go`](go/hello.go) and [`sdk/go/README.md`](../sdk/go/README.md) for the full API tour.

## Running the WASM demo

```bash
cd examples/wasm
make          # builds sdk/wasm via wasm-pack, then starts a local server
```

Opens `http://localhost:8080` with a tiny SQL console embedded in a single HTML page — `CREATE TABLE` / `INSERT` / `SELECT` all run client-side via the compiled WebAssembly module. No server backend.

The demo pre-fills the textarea with a `users` table + three inserts + a `SELECT ... ORDER BY age DESC`. Hit **Run** and you'll see the JSON result appear in the output pane. **Reset DB** drops the handle and spins up a fresh in-memory DB.

See [`wasm/index.html`](wasm/index.html) and [`sdk/wasm/README.md`](../sdk/wasm/README.md) for the full API tour and build options (`--target web` / `bundler` / `nodejs`).

## Design notes

- **One shape across languages.** Every SDK exposes `Connection`, `prepare`, `execute`, and typed `Row` access. The language-specific file in each subdir shows the same CRUD + transaction walkthrough, so users picking up a new binding recognize the surface immediately.
- **No build step required for end users.** Phase 6 ships prebuilt wheels (Python), `.node` binaries (Node.js), `libsqlrite.{so,dylib,dll}` (for Go / C), and `@joaoh82/sqlrite-wasm` (browser) — no "install Rust first" tax.
- **Examples track the engine.** Each sub-phase's commit lands the example alongside the binding itself, so the sample always works against the current library shape.
