# Getting started

## Prerequisites

- **Rust** — stable, edition 2024 (rustc 1.85 or newer). The [`rust-toolchain.toml`](../rust-toolchain.toml) at the repo root pins a stable channel with `rustfmt` and `clippy` components. Installing Rust via [`rustup`](https://www.rust-lang.org/en-US/install.html) is the simplest path.
- **A Unix-ish shell** — the REPL itself works on macOS, Linux, and Windows, but the build instructions below use `bash`/`zsh` idioms.

No external database dependency is required (SQLRite is self-contained; the older README mention of needing SQLite3 is obsolete).

## Building

```bash
git clone https://github.com/joaoh82/rust_sqlite.git
cd rust_sqlite
cargo build
```

First build takes ~20 s as dependencies compile. Incremental builds are near-instant.

## Running the tests

```bash
cargo test
```

All tests are unit tests embedded in their respective modules. The suite is the primary documentation for how each subsystem is supposed to behave — browsing `mod tests { ... }` at the bottom of a file is a fast way to understand that file.

## Launching the REPL

```bash
cargo run
```

You'll land in an in-memory REPL:

```
SQLRite - 0.1.0
Enter .exit to quit.
Enter .help for usage hints.
Connected to a transient in-memory database.
Use '.open FILENAME' to reopen on a persistent database.
sqlrite>
```

### First session

Try this sequence:

```sql
sqlrite> CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER);
sqlrite> INSERT INTO users (name, age) VALUES ('alice', 30);
sqlrite> INSERT INTO users (name, age) VALUES ('bob', 25);
sqlrite> SELECT name FROM users WHERE age > 25 ORDER BY name;
sqlrite> .exit
```

Every statement that mutates state is auto-saved — *if* the REPL is attached to a file. Without `.open`, everything lives in memory and disappears on `.exit`.

### First persistent session

```sql
sqlrite> .open demo.sqlrite
sqlrite> CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);
sqlrite> INSERT INTO notes (body) VALUES ('hello, disk');
sqlrite> .exit
```

Re-launch the REPL and the data is still there:

```bash
$ cargo run
sqlrite> .open demo.sqlrite
Opened 'demo.sqlrite' (1 table loaded). Auto-save enabled.
sqlrite> SELECT * FROM notes;
+----+-------------+
| id | body        |
+----+-------------+
| 1  | hello, disk |
+----+-------------+
```

See [Usage](usage.md) for the full list of supported SQL and meta-commands.

## Repository layout

```
rust_sqlite/
├── Cargo.toml              Workspace + engine crate
├── rust-toolchain.toml     Pinned stable Rust + rustfmt + clippy
├── README.md               Project overview (what/why/how)
├── docs/                   Developer guide (you are here)
├── src/
│   ├── lib.rs              Library root — the public engine API
│   ├── main.rs             Binary entry point, REPL loop
│   ├── error.rs            SQLRiteError enum
│   ├── repl/               rustyline integration, input validation
│   ├── meta_command/       Parsing + execution of .exit, .open, .save, .tables
│   └── sql/
│       ├── mod.rs          Top-level process_command dispatcher
│       ├── parser/         sqlparser → internal SelectQuery / CreateQuery / InsertQuery
│       ├── executor.rs     SELECT / UPDATE / DELETE / CREATE INDEX execution
│       ├── db/             In-memory data model (Database, Table, Column, Row,
│       │                   SecondaryIndex)
│       └── pager/          On-disk paged file format + Pager cache
├── desktop/                Tauri 2.0 desktop app (see docs/desktop.md)
│   ├── src/                Svelte 5 UI
│   └── src-tauri/          Tauri backend; pulls in the engine by path
└── samples/                Example SQL and reference ASTs
```

## Running the desktop app

A Tauri 2.0 desktop GUI lives under [`desktop/`](../desktop/). It needs Node.js in addition to the Rust toolchain:

```bash
cd desktop
npm install
npm run tauri dev
```

See [docs/desktop.md](desktop.md) for architecture + platform prerequisites.

## Next reading

- [Architecture](architecture.md) for the bird's-eye view of those modules
- [SQL engine](sql-engine.md) to understand how a user query flows through the codebase
- [Storage model](storage-model.md) to understand how rows are laid out in memory
- [Pager](pager.md) to understand how the DB file is written to disk
- [Desktop app](desktop.md) for the Tauri shell's architecture and troubleshooting
