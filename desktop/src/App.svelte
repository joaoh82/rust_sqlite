<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { open as openFileDialog } from "@tauri-apps/plugin-dialog";

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
  let sql = $state<string>("SELECT * FROM sqlrite_master;");
  let output = $state<CommandResult | null>(null);
  let errorMessage = $state<string | null>(null);
  let running = $state<boolean>(false);

  async function refreshTables() {
    try {
      tables = await invoke<TableInfo[]>("list_tables");
    } catch (err) {
      errorMessage = String(err);
    }
  }

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
      await invoke<TableInfo>("open_database", { path: picked });
      dbPath = picked;
      await refreshTables();
      selected = tables[0] ?? null;
      output = {
        kind: "status",
        message: `Opened ${picked}. ${tables.length} table${tables.length === 1 ? "" : "s"}.`,
      };
    } catch (err) {
      errorMessage = String(err);
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

  function onKey(e: KeyboardEvent) {
    // Cmd/Ctrl+Enter runs the query.
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      onRunSql();
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
      <button onclick={onOpenClick}>Open…</button>
    </div>
  </header>

  <div class="layout">
    <aside class="sidebar">
      <h3>Tables</h3>
      {#if tables.length === 0}
        <p class="muted">No tables yet.</p>
      {:else}
        <ul>
          {#each tables as t (t.name)}
            <li
              class:selected={selected?.name === t.name}
              onclick={() => onSelectTable(t)}
              onkeydown={(e) => e.key === "Enter" && onSelectTable(t)}
              role="button"
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
        <textarea
          bind:value={sql}
          onkeydown={onKey}
          spellcheck="false"
          placeholder="SELECT * FROM …;"
        ></textarea>
        <div class="editor-toolbar">
          <button onclick={onRunSql} disabled={running}>
            {running ? "Running…" : "Run (⌘↵)"}
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
