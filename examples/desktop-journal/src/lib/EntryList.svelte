<script lang="ts">
  import type { EntrySummary, SearchHit } from "./api";

  type Props = {
    entries: EntrySummary[];
    searchHits: SearchHit[];
    selectedId: number | null;
    onSelect: (id: number) => void;
  };

  let { entries, searchHits, selectedId, onSelect }: Props = $props();
  let inSearch = $derived(searchHits.length > 0);
</script>

<ul class="space-y-1">
  {#if inSearch}
    {#each searchHits as hit (hit.id)}
      <li>
        <button
          type="button"
          class="w-full text-left rounded px-2 py-1.5 hover:bg-panel-2 border border-transparent"
          class:bg-accent-dim={selectedId === hit.id}
          class:border-accent={selectedId === hit.id}
          onclick={() => onSelect(hit.id)}
        >
          <div class="flex items-baseline justify-between gap-2">
            <span class="font-medium truncate">{hit.title}</span>
            <span class="text-xs text-fg-muted font-mono">{hit.date}</span>
          </div>
          <div
            class="text-xs text-fg-muted mt-0.5 line-clamp-2"
          >{@html hit.snippet_html}</div>
        </button>
      </li>
    {/each}
  {:else if entries.length === 0}
    <li class="text-fg-muted italic px-2 py-3">
      No entries yet. Click <span class="font-mono">New</span> to write one.
    </li>
  {:else}
    {#each entries as e (e.id)}
      <li>
        <button
          type="button"
          class="w-full text-left rounded px-2 py-1.5 hover:bg-panel-2 border border-transparent"
          class:bg-accent-dim={selectedId === e.id}
          class:border-accent={selectedId === e.id}
          onclick={() => onSelect(e.id)}
        >
          <div class="flex items-baseline justify-between gap-2">
            <span class="font-medium truncate">{e.title || "(untitled)"}</span>
            <span class="text-xs text-fg-muted font-mono">{e.date}</span>
          </div>
          {#if e.excerpt}
            <div class="text-xs text-fg-muted mt-0.5 truncate">{e.excerpt}</div>
          {/if}
          {#if e.tags.length > 0}
            <div class="text-[10px] text-accent mt-0.5 font-mono">
              {e.tags.map((t) => "#" + t).join(" ")}
            </div>
          {/if}
        </button>
      </li>
    {/each}
  {/if}
</ul>
