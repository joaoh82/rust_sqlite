<script lang="ts">
  import { onMount } from "svelte";
  import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
  import {
    api,
    type Entry,
    type EntrySummary,
    type SearchHit,
    type Stats,
    type TagSummary,
  } from "./lib/api";
  import EntryList from "./lib/EntryList.svelte";
  import EntryEditor from "./lib/EntryEditor.svelte";
  import AskPanel from "./lib/AskPanel.svelte";
  import SettingsPanel from "./lib/SettingsPanel.svelte";

  let dbPath = $state<string | null>(null);
  let entries = $state<EntrySummary[]>([]);
  let tags = $state<TagSummary[]>([]);
  let stats = $state<Stats | null>(null);

  let selectedId = $state<number | null>(null);
  let selectedEntry = $state<Entry | null>(null);
  let isNew = $state(false);

  let searchQuery = $state("");
  let searchHits = $state<SearchHit[]>([]);

  let tagFilter = $state<string | null>(null);

  let showAsk = $state(false);
  let showSettings = $state(false);
  let globalError = $state<string | null>(null);

  onMount(async () => {
    try {
      dbPath = await api.currentDbPath();
      await refreshAll();
    } catch (e) {
      globalError = String(e);
    }
  });

  async function refreshAll() {
    try {
      const [es, ts, st] = await Promise.all([
        api.listEntries(tagFilter),
        api.listTags(),
        api.stats(),
      ]);
      entries = es;
      tags = ts;
      stats = st;
      globalError = null;
    } catch (e) {
      globalError = String(e);
    }
  }

  async function selectEntry(id: number) {
    selectedId = id;
    isNew = false;
    try {
      selectedEntry = await api.getEntry(id);
    } catch (e) {
      globalError = String(e);
    }
  }

  function startNew() {
    selectedId = null;
    selectedEntry = null;
    isNew = true;
  }

  async function onSaved(id: number) {
    isNew = false;
    selectedId = id;
    await refreshAll();
    try {
      selectedEntry = await api.getEntry(id);
    } catch (e) {
      globalError = String(e);
    }
  }

  async function onDeleted() {
    selectedId = null;
    selectedEntry = null;
    isNew = false;
    await refreshAll();
  }

  function closeEditor() {
    selectedId = null;
    selectedEntry = null;
    isNew = false;
  }

  async function pickAndOpen() {
    try {
      const picked = await openFileDialog({
        multiple: false,
        title: "Open a .sqlrite journal",
        filters: [{ name: "SQLRite", extensions: ["sqlrite"] }],
      });
      if (!picked || typeof picked !== "string") return;
      const r = await api.openDatabase(picked);
      dbPath = r.path;
      selectedId = null;
      selectedEntry = null;
      isNew = false;
      tagFilter = null;
      searchQuery = "";
      searchHits = [];
      await refreshAll();
    } catch (e) {
      globalError = String(e);
    }
  }

  async function exportDbAs() {
    try {
      const picked = await saveFileDialog({
        title: "Export journal database",
        defaultPath: "journal-export.sqlrite",
        filters: [{ name: "SQLRite", extensions: ["sqlrite"] }],
      });
      if (!picked) return;
      await api.exportDb(picked);
      globalError = `Exported DB to ${picked}`;
    } catch (e) {
      globalError = String(e);
    }
  }

  async function exportMarkdown() {
    try {
      const picked = await openFileDialog({
        directory: true,
        title: "Export entries as markdown",
      });
      if (!picked || typeof picked !== "string") return;
      const summary = await api.exportMarkdown(picked);
      globalError = `Exported ${summary.entry_count} entries to ${summary.dest}`;
    } catch (e) {
      globalError = String(e);
    }
  }

  // Debounce: search every keystroke would be fine here (in-memory FTS
  // is fast), but adding a small debounce keeps the spinner UX smooth.
  let searchTimer: ReturnType<typeof setTimeout> | null = null;
  function scheduleSearch() {
    if (searchTimer) clearTimeout(searchTimer);
    searchTimer = setTimeout(runSearch, 150);
  }
  async function runSearch() {
    const q = searchQuery.trim();
    if (!q) {
      searchHits = [];
      return;
    }
    try {
      searchHits = await api.searchEntries(q);
    } catch (e) {
      globalError = String(e);
    }
  }

  async function clickTag(name: string) {
    tagFilter = tagFilter === name ? null : name;
    selectedId = null;
    selectedEntry = null;
    isNew = false;
    searchQuery = "";
    searchHits = [];
    await refreshAll();
  }
</script>

<main class="flex flex-col h-screen">
  <!-- Header -->
  <header
    class="flex items-center justify-between px-4 py-2 bg-panel border-b border-border select-none"
  >
    <div class="flex items-baseline gap-3">
      <span class="text-accent text-lg">◆</span>
      <span class="font-semibold tracking-wide">SQLRite Journal</span>
      {#if dbPath}
        <span class="text-fg-muted font-mono text-xs">{dbPath}</span>
      {/if}
    </div>
    <div class="flex gap-2">
      <button class="px-3 py-1 text-xs bg-panel-2 border border-border rounded hover:border-accent-dim" onclick={startNew}
        >+ New</button
      >
      <button class="px-3 py-1 text-xs bg-panel-2 border border-border rounded hover:border-accent-dim" onclick={pickAndOpen}
        >Open…</button
      >
      <button
        class="px-3 py-1 text-xs bg-panel-2 border border-border rounded hover:border-accent-dim"
        onclick={() => (showAsk = true)}>Ask my journal</button
      >
      <button class="px-3 py-1 text-xs bg-panel-2 border border-border rounded hover:border-accent-dim" onclick={exportDbAs}
        >Export DB…</button
      >
      <button class="px-3 py-1 text-xs bg-panel-2 border border-border rounded hover:border-accent-dim" onclick={exportMarkdown}
        >Export .md…</button
      >
      <button
        class="px-2 py-1 text-xs bg-panel-2 border border-border rounded hover:border-accent-dim"
        onclick={() => (showSettings = true)}
        aria-label="Settings"
        title="Settings"
      >⚙</button>
    </div>
  </header>

  {#if globalError}
    <div class="px-4 py-2 text-error font-mono text-xs bg-panel-2 border-b border-border">
      {globalError}
      <button class="ml-2 text-fg-muted hover:text-fg" onclick={() => (globalError = null)} aria-label="Dismiss">×</button>
    </div>
  {/if}

  <div class="flex-1 min-h-0 grid grid-cols-[300px_1fr]">
    <!-- Sidebar -->
    <aside class="bg-panel border-r border-border flex flex-col min-h-0">
      <div class="px-3 py-2 border-b border-border">
        <input
          type="search"
          placeholder="Search entries…"
          bind:value={searchQuery}
          oninput={scheduleSearch}
          class="w-full bg-panel-2 border border-border rounded px-2 py-1 text-fg font-mono text-xs outline-none focus:border-accent"
        />
        {#if tagFilter}
          <div class="text-[10px] text-accent mt-1.5 font-mono">
            filter: #{tagFilter}
            <button class="text-fg-muted ml-1" onclick={() => clickTag(tagFilter!)} aria-label="Clear filter">×</button>
          </div>
        {/if}
      </div>

      <div class="flex-1 overflow-y-auto p-3">
        <EntryList
          {entries}
          {searchHits}
          {selectedId}
          onSelect={selectEntry}
        />
      </div>

      <div class="px-3 py-3 border-t border-border">
        <div class="text-[10px] uppercase tracking-wider text-fg-muted mb-1.5">Tags</div>
        {#if tags.length === 0}
          <div class="text-fg-muted text-xs italic">No tags yet.</div>
        {:else}
          <div class="flex flex-wrap gap-1">
            {#each tags as t}
              <button
                type="button"
                class="px-2 py-0.5 text-[11px] font-mono rounded border border-border hover:border-accent-dim"
                class:bg-accent-dim={tagFilter === t.name}
                onclick={() => clickTag(t.name)}
              >#{t.name} <span class="text-fg-muted">{t.entry_count}</span></button>
            {/each}
          </div>
        {/if}
        {#if stats}
          <div class="text-[10px] text-fg-muted mt-3 font-mono">
            {stats.total_entries} entries · {stats.distinct_dates} days · {stats.total_tags} tags
          </div>
        {/if}
      </div>
    </aside>

    <!-- Main area -->
    <section class="bg-bg min-h-0">
      {#if isNew}
        <EntryEditor
          entry={null}
          isNew={true}
          onSaved={onSaved}
          onDeleted={onDeleted}
          onCanceled={closeEditor}
        />
      {:else if selectedEntry}
        <EntryEditor
          entry={selectedEntry}
          isNew={false}
          onSaved={onSaved}
          onDeleted={onDeleted}
          onCanceled={closeEditor}
        />
      {:else}
        <div class="h-full flex items-center justify-center text-fg-muted text-sm italic">
          Select an entry or click <span class="mx-1 font-mono">New</span> to start writing.
        </div>
      {/if}
    </section>
  </div>

  {#if showAsk}
    <AskPanel onClose={() => (showAsk = false)} />
  {/if}

  {#if showSettings}
    <SettingsPanel
      onClose={() => (showSettings = false)}
      onSaved={() => {
        /* keep the panel open so the user sees the "saved" toast */
      }}
    />
  {/if}
</main>
