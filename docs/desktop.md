# The desktop app

A Tauri 2.0 shell around the engine. Cross-platform (macOS / Linux / Windows) via the system webview, Svelte 5 for the UI, the engine imported as a regular Rust library.

Lives under [`desktop/`](../desktop/).

![SQLRite Desktop](<../images/SQLRite - Desktop.png> "The SQLRite desktop app")

*Screenshot: the default three-pane layout — sidebar with tables and schema on the left, query editor with line numbers up top, result grid below. Header carries New… / Open… / Save As… and shows the active database path.*

## Running it

Two prerequisites beyond the engine's toolchain:

- **Node.js** 18+ with `npm`
- **Tauri 2's platform deps** — on macOS this is just Xcode Command Line Tools. See the [official Tauri prerequisites guide](https://v2.tauri.app/start/prerequisites/) for Linux (WebKit + build tools) and Windows (Edge WebView2 + build tools).

First-time setup:

```bash
cd desktop
npm install     # pull Svelte, Vite, Tauri CLI, the dialog plugin
npm run tauri dev
```

That builds the Rust side (incremental after the first time), boots the Vite dev server on port 1420, and opens a native window pointing at it. Hot reload works for the Svelte side; Rust changes trigger a rebuild.

For a one-off release build:

```bash
npm run tauri build
```

Bundling is disabled in [`tauri.conf.json`](../desktop/src-tauri/tauri.conf.json) by default — flip `bundle.active` to `true` and plug in real app icons when you want to produce an installer.

## Architecture

```
                    ┌───────────────────────────────┐
                    │  Svelte 5 UI (desktop/src/)   │
                    │   App.svelte — sidebar,       │
                    │   query editor, result grid   │
                    └──────────────┬────────────────┘
                                   │  JSON over IPC
                                   │  (Tauri invoke)
                    ┌──────────────▼────────────────┐
                    │ Tauri 2 commands              │
                    │ desktop/src-tauri/src/main.rs │
                    │  - open_database(path)        │
                    │  - list_tables()              │
                    │  - table_rows(name, limit)    │
                    │  - execute_sql(sql)           │
                    └──────────────┬────────────────┘
                                   │  direct function calls
                                   │
                    ┌──────────────▼────────────────┐
                    │  sqlrite (engine crate)       │
                    │  src/lib.rs                   │
                    │   Database, process_command,  │
                    │   open_database, save_database│
                    └───────────────────────────────┘
```

### State

`AppState` holds one `Mutex<Database>`. Every Tauri command locks it, runs against the engine, drops the lock, returns a serializable result.

The `Mutex` isn't a concurrency optimization — it's a correctness requirement. Tauri's `State<T>` demands `Send + Sync`, and the engine uses interior mutability (`Arc<Mutex<HashMap<String, Row>>>`) internally. Only one command at a time touches the database.

### Commands

| Command | What it does |
|---|---|
| `open_database(path)` | Load an existing `.sqlrite` file or create a fresh one at `path`. Replaces the in-memory state entirely. |
| `save_database_as(path)` | Write the current in-memory state to `path` and **adopt** that path as the new auto-save target. Different from the REPL's `.save FILE`, which writes without adopting. |
| `list_tables()` | Sidebar data — one entry per user table with column metadata. |
| `table_rows(name, limit)` | Seed the result grid with up to `limit` rows from `name`. |
| `execute_sql(sql)` | Run one SQL statement through `process_command`. Returns structured rows for SELECTs, a status string for everything else. |

### Command / response types

Every command returns something serde-serializable. The reusable shape for statement output is a tagged enum:

```rust
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CommandResult {
    Rows { columns: Vec<String>, rows: Vec<Vec<String>> },
    Status { message: String },
}
```

SELECT statements produce `Rows`; everything else produces `Status`. The Svelte side pattern-matches on `kind`.

### The SELECT re-run hack

The engine's `process_command` does its SELECT display by calling `print!(...)` on stdout before returning a status message (the REPL expects that layout). For the desktop app we want structured rows, so `execute_sql` detects SELECTs, re-parses the SQL via `sqlrite::sqlparser`, re-walks the table, and ships (columns, rows) back through the IPC bridge.

This is explicitly a stopgap. Phase 5's Cursor refactor will have the executor return structured results directly, and this extraction path disappears.

## UI

### Layout

One Svelte component — [`App.svelte`](../desktop/src/App.svelte) — renders three panes:

- **Header**: product name, current DB path, **New…** / **Open…** / **Save As…** buttons.
  - **New…** uses the system save dialog to let you type a fresh filename; the backend's `open_database` creates-if-missing and attaches the long-lived pager.
  - **Open…** uses the open dialog for existing files only; refuses paths that don't exist.
  - **Save As…** writes the current in-memory or file-backed database to a chosen path and **adopts it** as the new auto-save target. Primary use case: user launched the app in transient in-memory mode, built up some schema, and now wants to keep it. Also works as "save a snapshot to a different file" while already file-backed — but note the adoption, so after Save As… every subsequent write lands in the newly-chosen file, not the original.
- **Sidebar**: alphabetical list of user tables. Clicking one selects it and fetches up to 500 rows via `table_rows`. Below the list, the selected table's column list with flags (PK / UQ / NN).
- **Main area**: query editor (textarea with a line-number gutter) + result grid.
  - **Line numbers**: rendered in a gutter on the left of the textarea; derived from the text content (`sql.split("\n").length`) and kept scroll-synced with the textarea via an `onscroll` handler. Font size and line height are locked between the gutter and the textarea so every line number aligns with its row.
  - **Cmd/Ctrl + Enter** — run the query. If the textarea has a non-empty selection, only the selected substring runs; otherwise the full editor contents run. Matches DataGrip / DBeaver / pgAdmin behavior. The Run button label flips to **Run selection** and the shortcut hint shows "selection only" when a selection is active so the state is visible without clicking.
  - **Cmd/Ctrl + /** — toggle SQL line comment (`-- `) on the current line or on every line of the selection. Matches VS Code / Sublime / IntelliJ convention.

The textarea starts with a short comment-only placeholder so clicking Run before typing any SQL doesn't error.

### Styling

[`src/app.css`](../desktop/src/app.css) is a hand-written dark theme with CSS variables for colors. Monospace for data cells, sans-serif for chrome. Sticky result-grid header.

### State management

No external store — just `$state` runes inside the component. For a single-window app with this little state, it's simpler than bringing in Zustand / Pinia / etc.

## What's not here yet

- **Tabs / multiple queries** — one editor, one result pane
- **Query history** — per-session only, dropped when the app exits
- **Schema editing from the UI** — you can paste `CREATE TABLE` into the editor, but there's no form-based flow
- **Indexes surfaced in the sidebar** — indexes exist on disk via `sqlrite_master`, they just aren't shown yet
- **`sqlrite_master` exposed for read-only inspection** — the engine hides it from `db.tables`; selecting from it through the query editor errors with `Table 'sqlrite_master' not found`. A `.schema` meta-command (also missing from the REPL) would be the less-dangerous path to surface it
- **Dark/light toggle** — dark only for now
- **App icon** — a placeholder PNG; replace before bundling for distribution
- **Error recovery after panic** — the Tauri app is a thin shell, so if the engine panics the whole window dies. The engine isn't supposed to panic on user input (Phase 1 made that a requirement), but a panic in the Tauri layer itself would take the app down.

Most of the above are straightforward frontend additions. The bigger shift is the cursor API — see [Roadmap](roadmap.md) Phase 5.

## Multi-process behavior

Phase 4a introduced OS-level advisory locks on open databases; Phase 4e graduated them to shared-vs-exclusive modes. Consequences worth knowing:

- The desktop app opens files **read-write** (exclusive lock). If a REPL or another desktop instance already has the file open in the same mode, the second open fails with `database '...' is in use (another process has it open; readers and writers are exclusive)`. The failure is clean — no data loss, just a surfaced error.
- A REPL launched with `--readonly` takes a **shared lock** on its file and can coexist with other read-only openers, but a writer (the desktop app, or a read-write REPL) still excludes them. POSIX flock semantics are "multiple readers OR one writer", not both simultaneously.
- The desktop app holds its lock for the entire time a file is open; it releases on New…, Open… to a different path, or Save As… (which switches the active file).

## Troubleshooting

- **`npm run tauri dev` hangs at "Compiling tauri…"**: first build is slow (several hundred deps). Subsequent builds are incremental.
- **Blank window on Linux**: usually a missing WebKit2GTK. See the Tauri prerequisites guide.
- **"failed to open icon" build error**: means `desktop/src-tauri/icons/icon.png` is missing or unreadable. The repo ships a placeholder; regenerate with the Python snippet in the commit for `741effb` if needed.
- **`@tauri-apps/...` imports resolve in VS Code but fail at runtime**: re-run `npm install` in `desktop/` to pull the runtime side of the plugins, not just the type definitions.
