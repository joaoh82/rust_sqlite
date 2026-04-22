<script lang="ts">
  import { onMount, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";

  type ColumnInfo = {
    name: string;
    datatype: string;
    is_pk: boolean;
    is_unique: boolean;
    not_null: boolean;
  };
  type TableInfo = { name: string; columns: ColumnInfo[] };
  type CommandResult =
    | { kind: "rows"; columns: string[]; rows: string[][] }
    | { kind: "status"; message: string };

  // UI state.
  let dbPath = $state<string | null>(null);
  let tables = $state<TableInfo[]>([]);
  let selected = $state<TableInfo | null>(null);
  // A comment-only default so hitting Run right after launch doesn't error.
  // Users can replace with real SQL; Cmd/Ctrl+Enter triggers Run.
  let sql = $state<string>(
    "-- Type a SQL statement and press Cmd/Ctrl+Enter to run.\n" +
      "-- Example:\n" +
      "--   CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);\n" +
      "--   SELECT * FROM users;\n"
  );
  let output = $state<CommandResult | null>(null);
  let errorMessage = $state<string | null>(null);
  let running = $state<boolean>(false);

  // Editor refs and derived line numbers for the gutter. We derive a
  // dense `[1, 2, …, n]` array so Svelte's {#each} iterates every slot
  // — a sparse `Array(n)` would skip indices.
  let textareaRef = $state<HTMLTextAreaElement | null>(null);
  let gutterRef = $state<HTMLDivElement | null>(null);
  let lineNumbers = $derived(
    Array.from({ length: sql.split("\n").length }, (_, i) => i + 1)
  );

  async function refreshTables() {
    try {
      tables = await invoke<TableInfo[]>("list_tables");
    } catch (err) {
      errorMessage = String(err);
    }
  }

  /**
   * Opens an existing `.sqlrite` file chosen via the system file picker.
   * Creating a new file is a separate entry point — `onNewClick`, using
   * the save dialog — because the default platform "Open" dialog either
   * refuses to return a nonexistent path or silently creates an empty
   * file the engine would reject.
   */
  async function onOpenClick() {
    errorMessage = null;
    try {
      const picked = await openFileDialog({
        multiple: false,
        directory: false,
        filters: [
          { name: "SQLRite database", extensions: ["sqlrite"] },
          { name: "All files", extensions: ["*"] },
        ],
      });
      if (!picked || typeof picked !== "string") return;
      await loadDatabase(picked);
    } catch (err) {
      errorMessage = String(err);
    }
  }

  /**
   * Creates a fresh `.sqlrite` file via the system save dialog and
   * opens it. The backend's `open_database` already creates-if-missing,
   * so we just hand it the path the user typed.
   */
  async function onNewClick() {
    errorMessage = null;
    try {
      const picked = await saveFileDialog({
        defaultPath: "untitled.sqlrite",
        filters: [
          { name: "SQLRite database", extensions: ["sqlrite"] },
          { name: "All files", extensions: ["*"] },
        ],
      });
      if (!picked || typeof picked !== "string") return;
      await loadDatabase(picked);
    } catch (err) {
      errorMessage = String(err);
    }
  }

  /** Shared success path for both Open and New. */
  async function loadDatabase(path: string) {
    await invoke<TableInfo>("open_database", { path });
    dbPath = path;
    await refreshTables();
    selected = tables[0] ?? null;
    if (selected) {
      await onSelectTable(selected);
    } else {
      output = {
        kind: "status",
        message: `Opened ${path}. ${tables.length} table${tables.length === 1 ? "" : "s"}.`,
      };
    }
  }

  async function onSelectTable(t: TableInfo) {
    selected = t;
    running = true;
    errorMessage = null;
    try {
      output = await invoke<CommandResult>("table_rows", {
        name: t.name,
        limit: 500,
      });
    } catch (err) {
      errorMessage = String(err);
    } finally {
      running = false;
    }
  }

  async function onRunSql() {
    running = true;
    errorMessage = null;
    try {
      output = await invoke<CommandResult>("execute_sql", { sql });
      // Any write statement may have mutated the schema; refresh sidebar.
      await refreshTables();
    } catch (err) {
      errorMessage = String(err);
    } finally {
      running = false;
    }
  }

  /**
   * Toggles SQL line comments (`-- `) on the line(s) covered by the
   * current selection. If every non-blank line in the range is already
   * commented, the toggle removes the prefix; otherwise it adds one.
   * Empty lines are left alone. Matches the VS Code / Sublime /
   * IntelliJ convention.
   */
  async function toggleComment() {
    const ta = textareaRef;
    if (!ta) return;
    const value = ta.value;
    const selStart = ta.selectionStart;
    const selEnd = ta.selectionEnd;

    // Expand the selection outward to whole lines.
    const lineStart = value.lastIndexOf("\n", selStart - 1) + 1;
    let lineEnd = value.indexOf("\n", selEnd);
    if (lineEnd === -1) lineEnd = value.length;

    const block = value.slice(lineStart, lineEnd);
    const lines = block.split("\n");

    // A line "counts" for the toggle decision only if it has non-whitespace.
    const meaningful = lines.filter((l) => l.trim().length > 0);
    const allCommented =
      meaningful.length > 0 &&
      meaningful.every((l) => l.trimStart().startsWith("--"));

    const toggled = lines.map((line) => {
      if (line.trim().length === 0) return line;
      if (allCommented) {
        // Remove the first "-- " or "--" after any leading whitespace.
        return line.replace(/^(\s*)-- ?/, "$1");
      }
      return "-- " + line;
    });

    const newBlock = toggled.join("\n");
    const newValue = value.slice(0, lineStart) + newBlock + value.slice(lineEnd);

    // Update the bound state; the textarea re-renders.
    sql = newValue;

    // Restore a sensible selection after Svelte flushes. We re-select the
    // edited block so the user can hit Cmd/Ctrl+/ again to untoggle.
    await tick();
    if (textareaRef) {
      textareaRef.focus();
      textareaRef.selectionStart = lineStart;
      textareaRef.selectionEnd = lineStart + newBlock.length;
    }
  }

  /** Keeps the gutter scrolled in sync with the textarea. */
  function onEditorScroll() {
    if (textareaRef && gutterRef) {
      gutterRef.scrollTop = textareaRef.scrollTop;
    }
  }

  function onKey(e: KeyboardEvent) {
    // Cmd/Ctrl+Enter runs the query.
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      onRunSql();
      return;
    }
    // Cmd/Ctrl+/ toggles SQL line comment on the current line or selection.
    if ((e.metaKey || e.ctrlKey) && e.key === "/") {
      e.preventDefault();
      toggleComment();
      return;
    }
  }

  onMount(() => {
    refreshTables();
  });
</script>

<main>
  <header>
    <div class="brand">
      <span class="logo">◆</span>
      <span class="title">SQLRite</span>
      {#if dbPath}
        <span class="path">— {dbPath}</span>
      {:else}
        <span class="path">— in-memory (no file)</span>
      {/if}
    </div>
    <div class="actions">
      <button onclick={onNewClick}>New…</button>
      <button onclick={onOpenClick}>Open…</button>
    </div>
  </header>

  <div class="layout">
    <aside class="sidebar">
      <h3>Tables</h3>
      {#if tables.length === 0}
        <p class="muted">No tables yet.</p>
      {:else}
        <ul role="listbox" aria-label="Tables">
          {#each tables as t (t.name)}
            <li
              class:selected={selected?.name === t.name}
              onclick={() => onSelectTable(t)}
              onkeydown={(e) => e.key === "Enter" && onSelectTable(t)}
              role="option"
              aria-selected={selected?.name === t.name}
              tabindex="0"
            >
              <span class="table-name">{t.name}</span>
              <span class="col-count">{t.columns.length} col{t.columns.length === 1 ? "" : "s"}</span>
            </li>
          {/each}
        </ul>
      {/if}
      {#if selected}
        <div class="schema">
          <h4>Schema: {selected.name}</h4>
          <ul class="cols">
            {#each selected.columns as c (c.name)}
              <li>
                <span class="col-name">{c.name}</span>
                <span class="col-type">{c.datatype}</span>
                <span class="col-flags">
                  {#if c.is_pk}PK {/if}
                  {#if c.is_unique && !c.is_pk}UQ {/if}
                  {#if c.not_null && !c.is_pk}NN{/if}
                </span>
              </li>
            {/each}
          </ul>
        </div>
      {/if}
    </aside>

    <section class="main">
      <div class="editor">
        <div class="editor-surface">
          <div class="gutter" bind:this={gutterRef} aria-hidden="true">
            {#each lineNumbers as n (n)}
              <div class="line-num">{n}</div>
            {/each}
          </div>
          <textarea
            bind:this={textareaRef}
            bind:value={sql}
            onkeydown={onKey}
            onscroll={onEditorScroll}
            spellcheck="false"
            placeholder="SELECT * FROM …;"
          ></textarea>
        </div>
        <div class="editor-toolbar">
          <span class="shortcut-hint">Run: ⌘↵ · Comment: ⌘/</span>
          <button onclick={onRunSql} disabled={running}>
            {running ? "Running…" : "Run"}
          </button>
        </div>
      </div>

      <div class="output">
        {#if errorMessage}
          <pre class="error">Error: {errorMessage}</pre>
        {/if}
        {#if output}
          {#if output.kind === "rows"}
            <div class="result-meta">{output.rows.length} row{output.rows.length === 1 ? "" : "s"}</div>
            <div class="table-wrap">
              <table class="result">
                <thead>
                  <tr>
                    {#each output.columns as c (c)}
                      <th>{c}</th>
                    {/each}
                  </tr>
                </thead>
                <tbody>
                  {#each output.rows as row, i (i)}
                    <tr>
                      {#each row as cell, j (j)}
                        <td>{cell}</td>
                      {/each}
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          {:else}
            <pre class="status">{output.message}</pre>
          {/if}
        {/if}
      </div>
    </section>
  </div>
</main>
