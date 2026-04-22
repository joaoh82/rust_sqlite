# Smoke test walkthrough

A step-by-step sanity check to run after any non-trivial change. Covers the REPL binary, the persistence round-trip, the indexes / optimizer paths, and the Tauri desktop app. Roughly 10 minutes end-to-end.

**Keep this doc in sync with the engine.** When a feature ships that changes expected output (a new meta command, a new SQL statement, a file-format bump, etc.), update the relevant section here at the same time.

## Prerequisites

Before starting:

- Clean repo state, build is green:
  ```bash
  cargo build --workspace
  cargo test --workspace
  ```
  Current expected: 123 tests pass across lib + bin + doctests.

- For the desktop section: Node 18+, a functional webview (macOS has it built in; Linux needs `webkit2gtk-4.1`; Windows needs Edge WebView2), and the Tauri prerequisites per [docs/desktop.md](desktop.md).

Work from a shell at the repo root for all REPL steps.

---

## Part 1 — REPL (in-memory)

Launches the REPL without opening a file. Every statement lives in RAM and disappears on `.exit`.

### 1.1 Launch

```bash
cargo run --quiet --bin sqlrite
```

You should see:

```
sqlrite - 0.1.0
Enter .exit to quit.
Enter .help for usage hints.
Connected to a transient in-memory database.
Use '.open FILENAME' to reopen on a persistent database.
sqlrite>
```

### 1.2 Meta commands

```
sqlrite> .help
```

Expect the 5-command list (`.help`, `.open`, `.save`, `.tables`, `.exit`) and a note that `.read` / `.ast` aren't implemented.

### 1.3 Create a table

```sql
CREATE TABLE users (
  id INTEGER PRIMARY KEY,
  email TEXT NOT NULL UNIQUE,
  dept TEXT NOT NULL,
  hired INTEGER
);
```

Expect a schema table printed with four rows (one per column) and `CREATE TABLE Statement executed.`

### 1.4 Insert rows

```sql
INSERT INTO users (email, dept, hired) VALUES ('alice@co', 'eng', 2020);
INSERT INTO users (email, dept, hired) VALUES ('bob@co', 'eng', 2021);
INSERT INTO users (email, dept, hired) VALUES ('carol@co', 'sales', 2019);
INSERT INTO users (email, dept, hired) VALUES ('dan@co', 'sales', 2022);
```

Each `INSERT` prints the full table so far followed by `INSERT Statement executed.`

### 1.5 Query with projection + filter + order

```sql
SELECT email FROM users WHERE dept = 'sales' ORDER BY hired;
```

Expect:

```
+----------+
| email    |
+----------+
| carol@co |
+----------+
| dan@co   |
+----------+
SELECT Statement executed. 2 rows returned.
```

### 1.6 Arithmetic in UPDATE SET

```sql
UPDATE users SET hired = hired + 1 WHERE email = 'bob@co';
```

Expect `UPDATE Statement executed. 1 row updated.` Bob's `hired` is now 2022.

### 1.7 DELETE with range predicate

```sql
DELETE FROM users WHERE hired < 2020;
```

Expect `DELETE Statement executed. 1 row deleted.` Carol goes away (she was the only pre-2020 hire).

### 1.8 Auto-index on UNIQUE — duplicate rejected

```sql
INSERT INTO users (email, dept, hired) VALUES ('alice@co', 'hr', 2024);
```

Expect an error message containing `UNIQUE constraint violated for column 'email'`. The auto-index named `sqlrite_autoindex_users_email` caught it.

### 1.9 CREATE INDEX + WHERE-equality probe

```sql
CREATE INDEX users_dept_idx ON users (dept);
SELECT email FROM users WHERE dept = 'eng';
```

Expect `CREATE INDEX 'users_dept_idx' executed.` followed by the two `eng` rows (alice and bob). This exercises the executor's index-probe fast path.

### 1.10 Error cases don't crash the REPL

These should each print a clean `An error occured: …` message and leave the REPL live:

```sql
INSERT INTO users (email, dept) VALUES ('e', 'x', 999);           -- 3 values for 2 columns
SELECT * FROM nope;                                                -- unknown table
SELECT height FROM users;                                          -- unknown column
CREATE TABLE sqlrite_master (x INTEGER);                           -- reserved name
SELECT * FROM users WHERE hired / 0 > 0;                           -- division by zero
```

### 1.11 Exit

```
sqlrite> .exit
```

Terminal returns to the shell prompt. Data is gone — it was never on disk.

---

## Part 2 — REPL (persistent, multi-session)

Round-trips data through a `.sqlrite` file across three REPL invocations.

### 2.1 Choose a path

```bash
DB=/tmp/smoke.sqlrite
rm -f "$DB"
```

### 2.2 Session 1 — create + populate

```bash
cargo run --quiet --bin sqlrite
```

```
sqlrite> .open /tmp/smoke.sqlrite
```

Expect `Opened '/tmp/smoke.sqlrite' (new database). Auto-save enabled.`

```sql
CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL, priority INTEGER);
INSERT INTO notes (body, priority) VALUES ('review PR', 1);
INSERT INTO notes (body, priority) VALUES ('write tests', 2);
INSERT INTO notes (body, priority) VALUES ('ship feature', 3);
```

```
sqlrite> .tables
```

Expect `notes` (one line). **Do not type `.save`.** The auto-save ran after each INSERT.

```
sqlrite> .exit
```

Confirm the file was written:

```bash
ls -la "$DB"
```

Expect a file `12288` or `16384` bytes (3 × 4096 or 4 × 4096 — depends on index content).

### 2.3 Session 2 — reopen + mutate

```bash
cargo run --quiet --bin sqlrite
```

```
sqlrite> .open /tmp/smoke.sqlrite
```

Expect `Opened '/tmp/smoke.sqlrite' (1 table loaded). Auto-save enabled.`

```sql
SELECT * FROM notes ORDER BY priority;
UPDATE notes SET priority = priority + 10 WHERE id = 1;
```

```
sqlrite> .exit
```

### 2.4 Session 3 — verify the update persisted

```bash
cargo run --quiet --bin sqlrite
```

```
sqlrite> .open /tmp/smoke.sqlrite
SELECT * FROM notes ORDER BY id;
```

Expect row `id=1` to have `priority=11` (not 1). Auto-save carried the UPDATE through without a manual `.save`.

```
sqlrite> .exit
```

Cleanup:

```bash
rm -f "$DB"
```

### 2.5 Format-guard sanity

A file that isn't a SQLRite database should be rejected cleanly.

```bash
echo "not a database" > /tmp/bad.sqlrite
cargo run --quiet --bin sqlrite
```

```
sqlrite> .open /tmp/bad.sqlrite
```

Expect `An error occured: General error: not a SQLRite database (bad magic bytes)`. REPL stays live.

```
sqlrite> .exit
```

```bash
rm -f /tmp/bad.sqlrite
```

---

## Part 3 — Desktop app

### 3.1 Install frontend deps (first run only)

```bash
cd desktop
npm install
```

Expect ~300 packages. No warnings worth worrying about.

### 3.2 Launch

```bash
npm run tauri dev
```

First launch compiles the Tauri backend (a few hundred crates; takes a minute or two on a cold cache) and starts Vite. A native window appears titled "SQLRite" with a dark UI.

### 3.3 Initial state

- Header shows `◆ SQLRite — in-memory (no file)` on the left, `Open…` button on the right.
- Sidebar: `TABLES` heading followed by "No tables yet."
- Main area: textarea with `SELECT * FROM sqlrite_master;` pre-filled, `Run (⌘↵)` button below.

### 3.4 Open a database

Click **Open…**. In the file dialog, either:

- Navigate to a `.sqlrite` file you created in Part 2, or
- Type a new filename (e.g. `desktop-smoke.sqlrite`) and confirm — the engine creates it.

After the dialog closes:

- Header shows the selected path.
- Sidebar lists any existing tables. For a fresh file, "No tables yet."
- The status line below the query editor shows `Opened /path/to/file.sqlrite. N tables.`

### 3.5 Create a table via the query editor

Replace the textarea contents with:

```sql
CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, age INTEGER);
```

Press **⌘ + Enter** (macOS) or **Ctrl + Enter** (Linux / Windows), or click **Run**.

Expect:

- Status: `CREATE TABLE Statement executed.`
- Sidebar refreshes with `users` row (1 col count shown badge-style).

### 3.6 Insert rows

```sql
INSERT INTO users (name, age) VALUES ('alice', 30);
```

Run. Then:

```sql
INSERT INTO users (name, age) VALUES ('bob', 25);
INSERT INTO users (name, age) VALUES ('carol', 40);
```

(Run each individually — the UI runs one statement at a time.)

### 3.7 Click a table in the sidebar

Click `users`. Below the table list, its schema appears (three rows: id/name/age with PK / UQ / NN flags). The main area's result grid populates with all three rows.

### 3.8 Run a SELECT in the editor

```sql
SELECT name, age FROM users WHERE age > 25;
```

Run. Expect a two-column result grid with two rows (alice and carol). `2 rows` shown above the grid.

### 3.9 Run a CREATE INDEX

```sql
CREATE INDEX users_age_idx ON users (age);
```

Expect `CREATE INDEX 'users_age_idx' executed.` Sidebar doesn't expose indexes yet (known gap; see [docs/desktop.md §What's not here yet](desktop.md#whats-not-here-yet)), but the index is in memory and persisted to disk.

### 3.10 Trigger an error

```sql
INSERT INTO users (name, age) VALUES ('alice', 99);
```

Expect a red `Error: …UNIQUE constraint violated…` message above the result grid. The app stays responsive.

### 3.11 Close + relaunch

Close the window. Run `npm run tauri dev` again. Click **Open…** and pick the same file. The sidebar should show `users` and selecting it should reveal all three rows (including any UPDATEs from this session).

### 3.12 Stop the dev server

In the terminal running `tauri dev`, press Ctrl+C.

---

## Regression checklist

When you want a fast before/after comparison for a change, run this condensed checklist instead of the full walkthrough:

- [ ] `cargo build --workspace` → clean, zero warnings
- [ ] `cargo test --workspace` → 123 tests pass (123 was the count as of Phase 2.5; update when new tests land)
- [ ] REPL launches, `.help` shows 5 commands
- [ ] CREATE TABLE + INSERT + SELECT `*` work in memory
- [ ] `SELECT ... WHERE col = literal` on a UNIQUE column returns the right row (index probe path)
- [ ] UPDATE with arithmetic (`SET x = x + 1`) works
- [ ] Duplicate INSERT on a UNIQUE column errors cleanly
- [ ] `.open <new file>` → INSERT → `.exit` → `.open <same file>` → rows still there
- [ ] Bad-magic file is rejected with a clear error
- [ ] `cargo check -p sqlrite-desktop` compiles the Tauri crate
- [ ] `cd desktop && npm run tauri dev` opens a window, Open… → file picker works
- [ ] In the desktop app: CREATE TABLE via the editor updates the sidebar
- [ ] In the desktop app: SELECT runs and populates the result grid

Mark the ones you haven't covered for the current change; revisit if any fail.

---

## When something fails

**REPL exits unexpectedly.** Rare now — Phase 1 removed the panicky insert paths. If it still happens, the stack trace on stderr (with `RUST_BACKTRACE=1 cargo run …`) will point at the offender.

**`.open` fails with `not a SQLRite database (bad magic bytes)`** on a file you just wrote. Likely cause: the file was written by an older format version (pre-Phase-3e). Delete and recreate.

**`.open` fails with `unsupported SQLRite format version N`**. The current code expects format version `3` (Phase 3e). Older / newer files produce this error. If you hit it on a file from *this* build, the format constant and the file's bytes have desynced — rerun `cargo build` and `.open` again.

**`cargo run --bin sqlrite` fails to find the binary.** Since Phase 2.5.1 the binary name is `sqlrite`, not `SQLRite`. Passing `--bin sqlrite` is only necessary in the workspace context; `cargo run` alone also defaults to the REPL.

**Tauri window opens blank.** On Linux: install `webkit2gtk-4.1`. On Windows: install Edge WebView2. On macOS: this shouldn't happen — if it does, check that the dev server is running on port 1420 (visible in `npm run tauri dev` output).

**Tauri error "failed to open icon"**. The placeholder at `desktop/src-tauri/icons/icon.png` is missing. Regenerate by running the Python script captured in commit `741effb` (the one that created the desktop scaffold).

**`npm install` hangs.** Network issue — retry. The Tauri plugins aren't in every mirror; if you're behind a proxy that blocks them, switch to `npmrc` with a public registry.
