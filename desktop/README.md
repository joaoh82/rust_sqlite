# SQLRite Desktop

A Tauri 2 + Svelte 5 SQL playground that embeds the SQLRite engine
directly (no FFI hop). Open or create a `.sqlrite` file, browse tables,
run SQL, and — with an Anthropic API key configured — turn
natural-language questions into SQL via the **Ask…** button.

## Run (dev)

```sh
cd desktop
npm install
npm run tauri dev
```

The app starts in-memory ("scratch") — use **New…** / **Open…** to back
it with a file.

## Configuring `ask`

The **Ask…** button calls the LLM server-side (the Rust backend), so the
API key never crosses into the webview. There are two ways to provide it:

1. **⚙ Settings dialog (recommended).** Click the gear icon in the
   header and paste an Anthropic key. It's persisted to
   `$APP_DATA/com.sqlrite.desktop/settings.json`:

   ```
   ~/Library/Application Support/com.sqlrite.desktop/   (macOS)
   └── settings.json   # { "anthropic_api_key": "sk-ant-…", "model": "…", "max_tokens": … }
   ```

   This works regardless of how the app is launched — including from
   Finder / the Dock, which do **not** inherit a shell's environment.

2. **Env var fallback.** If no key is saved in Settings, the backend
   falls back to `SQLRITE_LLM_API_KEY` from the environment that launched
   the app. Handy for `npm run tauri dev` from a terminal, but it won't
   reach the app if you launch it from Finder.

`get_ask_settings` returns `has_api_key: bool` (never the key value), so
the UI can show which source is active — "key configured in settings",
"picked up from SQLRITE_LLM_API_KEY env var", or "no key configured".
With no key from either source, **Ask…** returns a clear
"no API key configured" error pointing back at the ⚙ gear icon.

### Security note

The settings file is **plain JSON on disk** — better UX than an env-var
gate and good enough for a local-first example app, but less secure than
the OS keychain. A production desktop app shipping the same pattern
should reach for `tauri-plugin-keyring` / `keyring-rs` instead.
