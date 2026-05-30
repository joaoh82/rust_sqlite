<script lang="ts">
  import { onMount } from "svelte";
  import { api, type AskSettings } from "./api";

  type Props = {
    onClose: () => void;
    onSaved: () => void;
  };

  let { onClose, onSaved }: Props = $props();

  let current = $state<AskSettings | null>(null);
  let apiKey = $state("");
  let clearKey = $state(false);
  let model = $state("");
  let maxTokens = $state<number | "">("");
  let busy = $state(false);
  let error = $state<string | null>(null);
  let info = $state<string | null>(null);

  onMount(async () => {
    try {
      current = await api.getAskSettings();
      model = current.model;
      maxTokens = current.max_tokens;
    } catch (e) {
      error = String(e);
    }
  });

  async function save() {
    busy = true;
    error = null;
    info = null;
    try {
      const update: Parameters<typeof api.updateAskSettings>[0] = {};
      if (clearKey) {
        update.anthropic_api_key = "";
      } else if (apiKey.trim().length > 0) {
        update.anthropic_api_key = apiKey;
      }
      if (model && current && model !== current.model) {
        update.model = model;
      }
      if (
        typeof maxTokens === "number" &&
        current &&
        maxTokens !== current.max_tokens
      ) {
        update.max_tokens = maxTokens;
      }
      const next = await api.updateAskSettings(update);
      current = next;
      apiKey = "";
      clearKey = false;
      info = "Settings saved.";
      onSaved();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }
</script>

<div class="fixed inset-0 bg-black/40 flex items-center justify-center z-50" role="dialog" aria-modal="true">
  <div class="w-full max-w-lg bg-panel border border-border rounded shadow-2xl flex flex-col">
    <div class="flex items-center justify-between px-5 py-3 border-b border-border">
      <h2 class="text-base font-semibold">Settings</h2>
      <button
        type="button"
        onclick={onClose}
        class="text-fg-muted hover:text-fg text-lg leading-none"
        aria-label="Close settings"
      >×</button>
    </div>

    <div class="px-5 py-4 space-y-5">
      <section>
        <div class="text-[10px] uppercase tracking-wider text-fg-muted mb-2">
          Ask my journal
        </div>

        <label for="ask-api-key" class="block text-sm mb-1.5">Anthropic API key</label>
        <input
          id="ask-api-key"
          type="password"
          autocomplete="off"
          spellcheck="false"
          placeholder={current?.has_api_key
            ? "(saved key on disk — leave blank to keep)"
            : "sk-ant-…"}
          bind:value={apiKey}
          disabled={clearKey}
          class="w-full bg-panel-2 border border-border rounded px-3 py-1.5 text-fg font-mono text-xs outline-none focus:border-accent disabled:opacity-50"
        />
        <div class="mt-2 flex items-center gap-2 text-xs">
          <input
            type="checkbox"
            bind:checked={clearKey}
            id="clear-key"
            class="accent-accent"
          />
          <label for="clear-key" class="text-fg-muted cursor-pointer">
            Clear the saved key
          </label>
        </div>
        {#if current}
          <div class="mt-2 text-xs text-fg-muted">
            Status: {current.has_api_key
              ? "key configured in settings"
              : current.env_api_key_present
                ? "key picked up from SQLRITE_LLM_API_KEY env var"
                : "no key configured"}.
          </div>
        {/if}

        <label for="ask-model" class="block text-sm mb-1.5 mt-4">Model</label>
        <input
          id="ask-model"
          type="text"
          bind:value={model}
          class="w-full bg-panel-2 border border-border rounded px-3 py-1.5 text-fg font-mono text-xs outline-none focus:border-accent"
        />
        <div class="mt-1 text-xs text-fg-muted">
          Defaults to <span class="font-mono">claude-sonnet-4-6</span>.
          Use any current Anthropic model id.
        </div>

        <label for="ask-max-tokens" class="block text-sm mb-1.5 mt-4">Max tokens</label>
        <input
          id="ask-max-tokens"
          type="number"
          min="64"
          max="8192"
          bind:value={maxTokens}
          class="w-32 bg-panel-2 border border-border rounded px-3 py-1.5 text-fg font-mono text-xs outline-none focus:border-accent"
        />
      </section>

      <section class="text-xs text-fg-muted">
        Stored as plain JSON next to your journal file. Better than an
        env var for desktop UX; a production app should reach for the
        OS keychain (e.g. tauri-plugin-keyring). The API key never
        crosses into the webview after this dialog closes — only the
        Rust backend reads it.
      </section>

      {#if error}
        <div class="text-error font-mono text-xs whitespace-pre-wrap">{error}</div>
      {/if}
      {#if info}
        <div class="text-accent font-mono text-xs">{info}</div>
      {/if}
    </div>

    <div class="px-5 py-3 border-t border-border flex justify-end gap-2">
      <button
        type="button"
        onclick={onClose}
        class="bg-panel-2 border border-border rounded px-3 py-1.5 text-xs"
      >Close</button>
      <button
        type="button"
        onclick={save}
        disabled={busy}
        class="bg-accent text-bg font-medium rounded px-4 py-1.5 text-sm disabled:opacity-50"
      >{busy ? "Saving…" : "Save"}</button>
    </div>
  </div>
</div>
