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

Also verify the help text is detailed:

```bash
cargo run --quiet --bin sqlrite -- --help
```

Should print the project description, the meta-command table, and a summary of supported SQL — not just `-h` / `-V` flags.

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

### 2.5 CLI file argument (shortcut)

The positional `FILE` argument is equivalent to `.open FILE` after launch:

```bash
DB=/tmp/smoke-cli.sqlrite && rm -f "$DB"
cargo run --quiet --bin sqlrite -- "$DB"
```

The banner should say `Opened '/tmp/smoke-cli.sqlrite' — auto-save enabled.` (the file was just created), and the REPL prompt appears.

```sql
CREATE TABLE a (id INTEGER PRIMARY KEY, s TEXT);
INSERT INTO a (s) VALUES ('hi');
```

```
sqlrite> .exit
```

Relaunch with the same argument — data should still be there:

```bash
cargo run --quiet --bin sqlrite -- "$DB"
```

```sql
SELECT * FROM a;
```

Expect 1 row. `.exit` and `rm -f "$DB"`.

### 2.6 Concurrent-open sanity (Phase 4a)

Two REPLs against the same file should no longer silently race. Start the first session and leave it at the prompt (do NOT `.exit`):

```bash
DB=/tmp/smoke-lock.sqlrite && rm -f "$DB"
cargo run --quiet --bin sqlrite -- "$DB"
```

In a second terminal while the first is still open:

```bash
cargo run --quiet --bin sqlrite -- "$DB"
```

The second one should fail during startup with something like:

```
Could not open '/tmp/smoke-lock.sqlrite': General error: database '...' is in use (another process has it open; readers and writers are exclusive) (...)
Falling back to a transient in-memory database.
```

… and then drop into a transient in-memory REPL (prompts "Connected to a transient in-memory database."). The first terminal is unaffected.

#### 2.6a Multi-reader coexistence (Phase 4e)

With the DB open in the first terminal (read-write), open a third and fourth terminal both running:

```bash
cargo run --quiet --bin sqlrite -- --readonly "$DB"
```

Both will fail until you `.exit` the read-write session (a writer excludes readers, POSIX flock). After the writer closes, both `--readonly` sessions should open simultaneously and can `SELECT * FROM notes;` concurrently. Any `INSERT` / `UPDATE` / `DELETE` attempt in either read-only REPL returns:

```
An error occured: General error: cannot commit: database is opened read-only
```

`.exit` both terminals and `rm -f "$DB"`.

### 2.6b Transactions (Phase 4f)

With a fresh DB, verify `BEGIN` / `COMMIT` / `ROLLBACK` behave as expected:

```bash
TXN="/tmp/smoke-txn.sqlrite"
rm -f "$TXN" "$TXN-wal"
cargo run --quiet --bin sqlrite -- "$TXN"
```

```sql
CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT);
INSERT INTO items (name) VALUES ('alpha');
BEGIN;
INSERT INTO items (name) VALUES ('beta');
SELECT * FROM items;           -- 2 rows, including 'beta'
ROLLBACK;
SELECT * FROM items;           -- back to 1 row, 'beta' is gone
BEGIN;
INSERT INTO items (name) VALUES ('gamma');
COMMIT;
SELECT * FROM items;           -- 2 rows: alpha + gamma
.exit
```

Reopen and verify `alpha` and `gamma` both survived:

```bash
cargo run --quiet --bin sqlrite -- "$TXN"
```

```sql
SELECT * FROM items;           -- alpha + gamma
BEGIN;
BEGIN;                          -- should error: "transaction is already open"
ROLLBACK;                       -- clears the outer BEGIN
COMMIT;                         -- should error: "no transaction is open"
.exit
```

```bash
rm -f "$TXN" "$TXN-wal"
```

### 2.7 Format-guard sanity

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

- Header shows `◆ SQLRite — in-memory (no file)` on the left, `New…` / `Open…` / `Save As…` buttons on the right.
- Sidebar: `TABLES` heading followed by "No tables yet."
- Main area: textarea pre-filled with a comment-only placeholder (nothing that would error on Run), `Run (⌘↵)` button below.

### 3.4a Create a new database

Click **New…**. The native save dialog appears — type `desktop-smoke.sqlrite` (or any new name) and confirm. The engine creates the file on disk immediately.

After the dialog closes:

- Header shows the chosen path.
- Sidebar: "No tables yet." (fresh database).
- Status line below the editor shows `Opened /path/to/file.sqlrite. 0 tables.`

### 3.4b Open an existing database

Click **Open…**. Pick a `.sqlrite` file that already exists (e.g. the one created in Part 2). After the dialog closes:

- Header shows the path.
- Sidebar lists the tables; if any exist, the first is auto-selected and its rows appear in the result grid.
- Status line shows `Opened /path/to/file.sqlrite. N tables.` (or the first table's rows if auto-selected).

If you try Open… on a file that doesn't exist, the dialog refuses to return; to create a fresh database, use New… instead.

### 3.4c Save As… (save an in-memory DB to a file)

Start a fresh session (no file opened) and create some schema directly via the editor:

```sql
CREATE TABLE scratch (id INTEGER PRIMARY KEY, note TEXT);
INSERT INTO scratch (note) VALUES ('in-memory row');
```

The sidebar should show `scratch`. Header still says "in-memory (no file)".

Now click **Save As…**. The system save dialog appears — type `scratch.sqlrite` (or any name) and confirm. After the dialog closes:

- Status line shows `Saved as /path/to/scratch.sqlrite. 1 table. Auto-save enabled.`
- Header updates to show the new file path.
- Clicking the `scratch` table in the sidebar still shows the row.

Close the app and relaunch. Open the file you just saved — the row should still be there.

If you started with a file-backed DB (`New…` or `Open…` earlier) and hit Save As…, the new path becomes the active one — subsequent writes go to the new file, not the original. The original stays on disk as a snapshot of whatever was there when you hit Save As….

### 3.4d Editor gutter + comment toggle

- **Line numbers**: the query textarea has a gutter on the left numbering each line. As you type multi-line SQL, the numbers update live. If the content exceeds the visible height and the textarea scrolls, the gutter scrolls in lockstep (no misalignment).
- **Comment toggle**: place the cursor on any line and press **⌘ + /** (macOS) or **Ctrl + /** (Linux / Windows). The line gets a `-- ` prefix if it wasn't commented, or has it removed if it was. Select multiple lines and the toggle acts on all of them; a mix of commented and uncommented lines is treated as "not all commented" and adds `-- ` uniformly (matching VS Code / Sublime behavior).

The editor toolbar shows the shortcuts (`Run: ⌘↵ · Comment: ⌘/`) as a reminder.

### 3.4e Run-selected-only

Prep: put at least two statements in the editor, for example

```sql
CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT);
INSERT INTO t (s) VALUES ('a');
INSERT INTO t (s) VALUES ('b');
SELECT * FROM t;
```

With no selection, clicking Run (or ⌘↵) runs *the whole editor*. Since the engine only accepts one statement per call, you'll see an error like `Expected a single query statement, but there are 4`. This is the intended behavior — it nudges users toward the selection-based flow.

Now select just the last line (`SELECT * FROM t;`) — or double-click a line, or drag across it. Two things happen:

- The Run button label flips to **Run selection**.
- The shortcut hint appends "· selection only".

Click Run (or ⌘↵). Only the SELECT executes. The result grid populates with whatever state `t` is in. Select a different statement, run it — it executes in isolation too.

Selecting just a few characters works too; sqlparser doesn't care about leading/trailing whitespace, but it does need a complete statement. `SELECT *` without `FROM` would error.

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
- [ ] `cargo run -- --help` prints the full description + meta-command table + SQL surface (not just `-h` / `-V`)
- [ ] `cargo run -- somefile.sqlrite` on a non-existent path creates the file and enters the REPL with auto-save on
- [ ] REPL launches, `.help` shows 5 commands
- [ ] `.tables` in a populated DB prints one name per line
- [ ] CREATE TABLE + INSERT + SELECT `*` work in memory
- [ ] `SELECT ... WHERE col = literal` on a UNIQUE column returns the right row (index probe path)
- [ ] UPDATE with arithmetic (`SET x = x + 1`) works
- [ ] Duplicate INSERT on a UNIQUE column errors cleanly
- [ ] `.open <new file>` → INSERT → `.exit` → `.open <same file>` → rows still there
- [ ] Bad-magic file is rejected with a clear error
- [ ] Opening the same file from two read-write REPLs simultaneously rejects the second with an "in use" / "readers and writers are exclusive" message and falls back to in-memory
- [ ] Two `--readonly` REPLs on the same file open simultaneously and both `SELECT` works; any `INSERT` in a `--readonly` REPL fails with "cannot commit: database is opened read-only"
- [ ] `BEGIN; INSERT …; ROLLBACK;` leaves the table unchanged; `BEGIN; INSERT …; COMMIT;` persists across `.exit`/reopen
- [ ] `BEGIN; BEGIN;` errors on the second (nested) BEGIN; orphan `COMMIT` / `ROLLBACK` error cleanly
- [ ] `cargo check -p sqlrite-desktop` compiles the Tauri crate
- [ ] `cd desktop && npm run tauri dev` opens a window
- [ ] In the desktop app: **New…** button opens a save dialog; picking a fresh filename creates the file and shows "0 tables"
- [ ] In the desktop app: **Open…** button opens a file picker for existing `.sqlrite` files
- [ ] In the desktop app: **Save As…** persists an in-memory DB to a new file and flips the header to that path
- [ ] In the desktop app: pressing Run on the default placeholder textarea doesn't error (it's comment-only)
- [ ] In the desktop app: the editor gutter shows one line number per row of the query and stays aligned while scrolling
- [ ] In the desktop app: **⌘/** (or Ctrl+/) on a line toggles its `-- ` comment; on a multi-line selection it toggles all of them
- [ ] In the desktop app: selecting one of several statements and hitting Run executes only the selection; the Run button label flips to **Run selection**
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
