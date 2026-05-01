# The desktop app

A Tauri 2.0 shell around the engine. Cross-platform (macOS / Linux / Windows) via the system webview, Svelte 5 for the UI, the engine imported as a regular Rust library.

Lives under [`desktop/`](../desktop/).

![SQLRite Desktop](<../images/SQLRite - Desktop.png> "The SQLRite desktop app")

*Screenshot: the default three-pane layout — sidebar with tables and schema on the left, query editor with line numbers up top, result grid below. Header carries New… / Open… / Save As… and shows the active database path.*

## Installing a prebuilt binary

Since Phase 6e, every release pushes installers to a per-version GitHub Release. Grab the right artifact for your platform from the [latest desktop release](https://github.com/joaoh82/rust_sqlite/releases/latest):

| Platform | Artifact | Notes |
|---|---|---|
| Linux x86_64 | `SQLRite_<ver>_amd64.AppImage` | `chmod +x` then run directly; bundles its own libs |
| Linux x86_64 | `SQLRite_<ver>_amd64.deb` | Debian / Ubuntu — `sudo dpkg -i` |
| Linux x86_64 | `SQLRite-<ver>-1.x86_64.rpm` | Fedora / RHEL / openSUSE — `sudo rpm -i` |
| macOS aarch64 | `SQLRite_<ver>_aarch64.dmg` | Apple Silicon only (M1/M2/M3/M4) |
| macOS aarch64 | `SQLRite_aarch64.app.tar.gz` | Raw `.app` bundle — for users who don't want the dmg |
| Windows x86_64 | `SQLRite_<ver>_x64_en-US.msi` | Windows Installer package |
| Windows x86_64 | `SQLRite_<ver>_x64-setup.exe` | NSIS installer, smaller footprint |

Intel Macs and Linux aarch64 are [tracked as follow-ups](roadmap.md#phase-6e-desktop-publish) — for now those platforms build from source.

### Unsigned-installer warnings

Installers ship **unsigned** until Phase 6.1 wires up Apple Developer ID + Windows code-signing certs. Expect one of these on first launch:

#### macOS — "SQLRite is damaged and can't be opened"

Or the gentler "unidentified developer" dialog on older macOS versions. One-time fix:

```bash
xattr -cr /Applications/SQLRite.app
```

Then double-click normally. Point `xattr` at wherever the `.app` lives if you haven't moved it to `/Applications` yet.

**Why "damaged" instead of "unidentified developer"?** Apple Silicon *requires* every Mach-O binary to carry a signature — even an ad-hoc one (`codesign --sign -`), which is what Tauri applies by default. When you download the dmg, your browser attaches `com.apple.quarantine` as an extended attribute. On recent macOS, the combination of (quarantined) + (ad-hoc signed) trips a stricter Gatekeeper code path that reports "damaged" instead of the milder unsigned-app flow. Stripping the quarantine attribute with `xattr -cr` takes Gatekeeper out of the loop. The app isn't actually corrupt — the error message just isn't honest about what it's complaining about.

Once Phase 6.1 lands notarization, this bypass won't be needed; macOS will trust the binary on first launch.

#### Windows — SmartScreen "Windows protected your PC"

Click **More info** → **Run anyway**. SmartScreen remembers your choice, so subsequent launches are clean.

#### Linux — AppImage won't run

```bash
chmod +x SQLRite_<ver>_amd64.AppImage
./SQLRite_<ver>_amd64.AppImage
```

`.deb` and `.rpm` packages don't have this step — they install through the system package manager and get the executable bit from the package metadata.

## Running from source

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

For a one-off release build that produces installers for your local platform:

```bash
npm run tauri build
```

[`tauri.conf.json`](../desktop/src-tauri/tauri.conf.json) ships with `bundle.active: true` since Phase 6e, so this builds a full set of platform-native installers (`.dmg` on macOS, `.AppImage` / `.deb` / `.rpm` on Linux, `.msi` / `.exe` on Windows) under `desktop/src-tauri/target/release/bundle/`.

Icons are pre-generated and committed at `desktop/src-tauri/icons/`. If you swap the source `icon.png`, re-run `npx tauri icon src-tauri/icons/icon.png` from `desktop/` and commit the regenerated `.icns` / `.ico` / size-specific PNGs. CI doesn't re-run this — the commit is the source of truth.

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
| `ask_sql(question)` | **Phase 7g.3** — Natural-language → SQL via the configured LLM (Anthropic by default). Reads `AskConfig::from_env()` and calls `sqlrite::ask::ask_with_database` against the locked engine `Database`. Schema introspection + LLM HTTP call run in the Tauri Rust backend so **the API key never crosses into the webview** — same security story as Q9's WASM JS-callback shape, applied here as a natural side effect of how Tauri's command bridge works. Returns `{ sql, explanation }`; the frontend pastes `sql` into the editor for the user to review + run. Requires `SQLRITE_LLM_API_KEY` in the process environment (Tauri inherits the parent shell's env). |

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
  - **Ask…** *(Phase 7g.3)* — opens a slide-in composer panel above the editor. Type a natural-language question, hit Cmd/Ctrl+Enter (or click "Generate SQL"), and the [`ask_sql`](#commands) Tauri command returns generated SQL + a one-sentence rationale. The SQL replaces the editor textarea; the rationale shows in the panel. **Generated SQL is not auto-executed** — the user reviews and clicks Run themselves. An empty SQL response (model declined the question against this schema) surfaces the model's explanation in the same slot. Esc closes the composer. Requires `SQLRITE_LLM_API_KEY` set in the environment Tauri was launched from; absence surfaces a clean "missing API key" error in the existing error slot.

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
