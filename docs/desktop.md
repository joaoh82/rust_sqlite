# The desktop app

A Tauri 2.0 shell around the engine. Cross-platform (macOS / Linux / Windows) via the system webview, Svelte 5 for the UI, the engine imported as a regular Rust library.

Lives under [`desktop/`](../desktop/).

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

- **Header**: product name, current DB path, "Open…" button.
- **Sidebar**: alphabetical list of user tables. Clicking one selects it and fetches up to 500 rows via `table_rows`. Below the list, the selected table's column list with flags (PK / UQ / NN).
- **Main area**: textarea query editor (plain contenteditable would be nicer but needs a proper editor library later) + result grid. `Cmd/Ctrl + Enter` runs the query.

### Styling

[`src/app.css`](../desktop/src/app.css) is a hand-written dark theme with CSS variables for colors. Monospace for data cells, sans-serif for chrome. Sticky result-grid header.

### State management

No external store — just `$state` runes inside the component. For a single-window app with this little state, it's simpler than bringing in Zustand / Pinia / etc.

## What's not here yet

- **Tabs / multiple queries** — one editor, one result pane
- **Query history** — per-session only, dropped when the app exits
- **Schema editing from the UI** — you can paste `CREATE TABLE` into the editor, but there's no form-based flow
- **Indexes surfaced in the sidebar** — indexes exist on disk via `sqlrite_master`, they just aren't shown yet
- **Dark/light toggle** — dark only for now
- **App icon** — a placeholder PNG; replace before bundling for distribution
- **Error recovery after panic** — the Tauri app is a thin shell, so if the engine panics the whole window dies. The engine isn't supposed to panic on user input (Phase 1 made that a requirement), but a panic in the Tauri layer itself would take the app down.

Most of the above are straightforward frontend additions. The bigger shift is the cursor API — see [Roadmap](roadmap.md) Phase 5.

## Troubleshooting

- **`npm run tauri dev` hangs at "Compiling tauri…"**: first build is slow (several hundred deps). Subsequent builds are incremental.
- **Blank window on Linux**: usually a missing WebKit2GTK. See the Tauri prerequisites guide.
- **"failed to open icon" build error**: means `desktop/src-tauri/icons/icon.png` is missing or unreadable. The repo ships a placeholder; regenerate with the Python snippet in the commit for `741effb` if needed.
- **`@tauri-apps/...` imports resolve in VS Code but fail at runtime**: re-run `npm install` in `desktop/` to pull the runtime side of the plugins, not just the type definitions.
