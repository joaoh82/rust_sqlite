<script lang="ts">
  import { marked } from "marked";
  import { api, type Entry, todayISO } from "./api";

  type Props = {
    entry: Entry | null;
    isNew: boolean;
    onSaved: (id: number) => void;
    onDeleted: () => void;
    onCanceled: () => void;
  };

  let { entry, isNew, onSaved, onDeleted, onCanceled }: Props = $props();

  // Initialise to safe defaults; the `$effect` below populates from
  // `entry` on mount and re-syncs whenever the parent swaps the prop
  // (different entry selected, save round-trip, etc.). Defaulting
  // here instead of reading `entry?.…` keeps svelte-check's
  // state_referenced_locally check happy — and it's also a hair
  // safer since none of these fields capture stale prop snapshots.
  let date = $state(todayISO());
  let title = $state("");
  let content = $state("");
  let tagText = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);
  let showPreview = $state(false);

  // Re-derive form state whenever the parent swaps the entry prop —
  // navigating between entries should reset the form, not keep stale
  // local edits from the prior selection.
  $effect(() => {
    date = entry?.date ?? todayISO();
    title = entry?.title ?? "";
    content = entry?.content ?? "";
    tagText = entry?.tags.join(", ") ?? "";
    error = null;
    showPreview = false;
  });

  const renderedHtml = $derived(content ? marked.parse(content) : "");

  function parseTags(s: string): string[] {
    return s
      .split(",")
      .map((t) => t.trim())
      .filter((t) => t.length > 0);
  }

  async function save() {
    busy = true;
    error = null;
    try {
      const tags = parseTags(tagText);
      if (isNew || !entry) {
        const id = await api.createEntry(date, title, content, tags);
        onSaved(id);
      } else {
        await api.updateEntry(entry.id, date, title, content, tags);
        onSaved(entry.id);
      }
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function del() {
    if (!entry) return;
    if (!confirm("Delete this entry? This cannot be undone.")) return;
    busy = true;
    error = null;
    try {
      await api.deleteEntry(entry.id);
      onDeleted();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  function onKeydown(ev: KeyboardEvent) {
    if ((ev.metaKey || ev.ctrlKey) && ev.key === "s") {
      ev.preventDefault();
      void save();
    }
  }
</script>

<!-- The ⌘S handler used to sit on this wrapper, but svelte-check
     (correctly) warned about keyboard listeners on non-interactive
     elements. Moved onto the textarea below so it only fires while
     the user has the editor focused — which is the right scope
     anyway. -->
<div class="flex flex-col h-full">
  <div class="flex items-center gap-3 px-4 py-3 border-b border-border bg-panel">
    <input
      type="date"
      bind:value={date}
      class="bg-panel-2 border border-border rounded px-2 py-1 text-fg font-mono text-sm"
    />
    <input
      type="text"
      placeholder="Title"
      bind:value={title}
      class="flex-1 bg-panel-2 border border-border rounded px-3 py-1 text-fg text-base"
    />
    <button
      type="button"
      onclick={() => (showPreview = !showPreview)}
      class="bg-panel-2 border border-border rounded px-3 py-1 text-xs hover:border-accent-dim"
    >
      {showPreview ? "Edit" : "Preview"}
    </button>
    <button
      type="button"
      onclick={save}
      disabled={busy}
      class="bg-accent text-bg font-medium rounded px-4 py-1 text-sm disabled:opacity-50"
    >{isNew ? "Save" : "Save"}</button>
    {#if !isNew && entry}
      <button
        type="button"
        onclick={del}
        disabled={busy}
        class="bg-panel-2 border border-border rounded px-3 py-1 text-xs hover:border-error hover:text-error"
      >Delete</button>
    {/if}
    <button
      type="button"
      onclick={onCanceled}
      class="bg-panel-2 border border-border rounded px-3 py-1 text-xs"
    >Close</button>
  </div>

  <div class="px-4 py-2 border-b border-border bg-panel">
    <label for="entry-tags" class="block text-[10px] uppercase tracking-wider text-fg-muted mb-1">Tags</label>
    <input
      id="entry-tags"
      type="text"
      placeholder="comma, separated, tags"
      bind:value={tagText}
      class="w-full bg-panel-2 border border-border rounded px-3 py-1 text-fg font-mono text-xs"
    />
  </div>

  {#if error}
    <div class="px-4 py-2 text-error font-mono text-xs border-b border-border bg-panel">{error}</div>
  {/if}

  <div class="flex-1 min-h-0 flex">
    {#if showPreview}
      <div class="flex-1 overflow-auto p-6 prose-mini">
        {@html renderedHtml}
      </div>
    {:else}
      <textarea
        bind:value={content}
        onkeydown={onKeydown}
        placeholder={"Write here. Markdown is supported.\n\n## Use # for headings\n- bullets\n- like this"}
        class="flex-1 bg-panel text-fg p-6 font-mono text-[13px] leading-6 outline-none resize-none border-0"
      ></textarea>
    {/if}
  </div>

  <div class="px-4 py-1.5 border-t border-border bg-panel text-[11px] text-fg-muted font-mono flex justify-between">
    <span>Markdown · Tailwind v4 · SQLRite-backed</span>
    <span>⌘S to save</span>
  </div>
</div>
