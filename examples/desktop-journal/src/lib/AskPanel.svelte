<script lang="ts">
  import { api, type AskResult } from "./api";

  type Props = {
    onClose: () => void;
  };

  let { onClose }: Props = $props();

  let question = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);
  let result = $state<AskResult | null>(null);

  async function ask() {
    if (!question.trim()) return;
    busy = true;
    error = null;
    result = null;
    try {
      result = await api.askJournal(question);
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  function onKeydown(ev: KeyboardEvent) {
    if ((ev.metaKey || ev.ctrlKey) && ev.key === "Enter") {
      ev.preventDefault();
      void ask();
    }
  }
</script>

<div class="fixed inset-0 bg-black/40 flex items-end z-50" role="dialog" aria-modal="true">
  <div class="w-full max-h-[80vh] bg-panel border-t border-border flex flex-col">
    <div class="flex items-center justify-between px-5 py-3 border-b border-border">
      <h2 class="text-base font-semibold">Ask my journal</h2>
      <button
        type="button"
        onclick={onClose}
        class="text-fg-muted hover:text-fg text-lg leading-none"
        aria-label="Close ask panel"
      >×</button>
    </div>

    <div class="px-5 py-4 border-b border-border">
      <textarea
        bind:value={question}
        onkeydown={onKeydown}
        placeholder='e.g. "what days did I write about running?" — ⌘↵ to submit'
        rows="3"
        class="w-full bg-panel-2 border border-border rounded px-3 py-2 text-fg font-mono text-sm outline-none focus:border-accent"
      ></textarea>
      <div class="mt-2 flex items-center justify-between">
        <span class="text-xs text-fg-muted">
          Read-only by design — the LLM-generated SQL is gated to <span class="font-mono">SELECT</span> /
          <span class="font-mono">WITH</span> only.
        </span>
        <button
          type="button"
          onclick={ask}
          disabled={busy || !question.trim()}
          class="bg-accent text-bg font-medium rounded px-4 py-1.5 text-sm disabled:opacity-50"
        >{busy ? "Asking…" : "Ask"}</button>
      </div>
    </div>

    <div class="flex-1 overflow-auto px-5 py-4">
      {#if error}
        <div class="text-error font-mono text-sm whitespace-pre-wrap">{error}</div>
        {#if error.toLowerCase().includes("api key")}
          <div class="text-xs text-fg-muted mt-3">
            Open the <span class="font-mono">⚙ Settings</span> dialog in
            the header and paste an Anthropic key from
            <span class="font-mono">console.anthropic.com</span>. Falls
            back to the <span class="font-mono">SQLRITE_LLM_API_KEY</span>
            env var if nothing's saved.
          </div>
        {/if}
      {/if}
      {#if result}
        <div class="space-y-4">
          <div>
            <div class="text-[10px] uppercase tracking-wider text-fg-muted mb-1">Generated SQL</div>
            <pre class="bg-panel-2 border border-border rounded p-3 font-mono text-xs overflow-auto">{result.sql}</pre>
          </div>
          {#if result.explanation}
            <div>
              <div class="text-[10px] uppercase tracking-wider text-fg-muted mb-1">Explanation</div>
              <div class="text-sm">{result.explanation}</div>
            </div>
          {/if}
          <div>
            <div class="text-[10px] uppercase tracking-wider text-fg-muted mb-1">
              Results ({result.rows.length})
            </div>
            {#if result.rows.length === 0}
              <div class="text-fg-muted italic text-sm">No rows returned.</div>
            {:else}
              <div class="overflow-auto border border-border rounded">
                <table class="w-full font-mono text-xs">
                  <thead class="bg-panel-2 sticky top-0">
                    <tr>
                      {#each result.columns as col}
                        <th class="text-left px-3 py-1.5 border-b border-border text-fg-muted">{col}</th>
                      {/each}
                    </tr>
                  </thead>
                  <tbody>
                    {#each result.rows as row, i}
                      <tr class:bg-panel={i % 2 === 0}>
                        {#each row as cell}
                          <td class="px-3 py-1.5 border-b border-border whitespace-nowrap">{cell}</td>
                        {/each}
                      </tr>
                    {/each}
                  </tbody>
                </table>
              </div>
            {/if}
          </div>
        </div>
      {/if}
      {#if !result && !error && !busy}
        <div class="text-fg-muted text-sm italic">
          Ask a question about your entries. The LLM sees only your schema
          (column names + types) — never your content — and responds with a
          <span class="font-mono">SELECT</span>. We then run that query
          locally and show you both the SQL and the rows it returned.
        </div>
      {/if}
    </div>
  </div>
</div>
