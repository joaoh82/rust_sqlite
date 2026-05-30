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

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<div
  class="overlay"
  role="dialog"
  aria-modal="true"
  aria-label="Settings"
>
  <div class="dialog">
    <div class="dialog-header">
      <h2>Settings</h2>
      <button
        type="button"
        class="close"
        onclick={onClose}
        aria-label="Close settings"
      >×</button>
    </div>

    <div class="dialog-body">
      <div class="section-label">Used by the Ask… button</div>

      <label for="ask-api-key">Anthropic API key</label>
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
      />
      <div class="checkbox-row">
        <input type="checkbox" id="clear-key" bind:checked={clearKey} />
        <label for="clear-key" class="checkbox-label">Clear the saved key</label>
      </div>
      {#if current}
        <div class="status-line">
          Status: {current.has_api_key
            ? "key configured in settings"
            : current.env_api_key_present
              ? "key picked up from SQLRITE_LLM_API_KEY env var"
              : "no key configured"}.
        </div>
      {/if}

      <label for="ask-model">Model</label>
      <input id="ask-model" type="text" bind:value={model} />
      <div class="field-hint">
        Defaults to <span class="mono">claude-sonnet-4-6</span>. Use any
        current Anthropic model id.
      </div>

      <label for="ask-max-tokens">Max tokens</label>
      <input
        id="ask-max-tokens"
        class="narrow"
        type="number"
        min="64"
        max="8192"
        bind:value={maxTokens}
      />

      <p class="security-note">
        Stored as plain JSON in the app's data directory
        (<span class="mono">com.sqlrite.desktop/settings.json</span>). Better
        than an env var for desktop UX; a production app should reach for the
        OS keychain (e.g. tauri-plugin-keyring). The API key never crosses
        into the webview after this dialog closes — only the Rust backend
        reads it.
      </p>

      {#if error}
        <div class="msg error">{error}</div>
      {/if}
      {#if info}
        <div class="msg info">{info}</div>
      {/if}
    </div>

    <div class="dialog-footer">
      <button type="button" onclick={onClose}>Close</button>
      <button type="button" class="primary" onclick={save} disabled={busy}>
        {busy ? "Saving…" : "Save"}
      </button>
    </div>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 50;
  }

  .dialog {
    width: 100%;
    max-width: 520px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    box-shadow: 0 16px 48px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
  }

  .dialog-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 18px;
    border-bottom: 1px solid var(--border);
  }

  .dialog-header h2 {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
  }

  .close {
    width: 26px;
    height: 26px;
    padding: 0;
    font-size: 16px;
    line-height: 1;
    color: var(--fg-muted);
  }
  .close:hover:not(:disabled) {
    color: var(--fg);
    border-color: var(--accent);
  }

  .dialog-body {
    padding: 16px 18px;
  }

  .section-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-muted);
    margin-bottom: 12px;
  }

  .dialog-body label {
    display: block;
    font-size: 13px;
    margin: 14px 0 6px;
  }
  .dialog-body label:first-of-type {
    margin-top: 0;
  }

  .dialog-body input[type="text"],
  .dialog-body input[type="password"],
  .dialog-body input[type="number"] {
    width: 100%;
    background: var(--panel-2);
    color: var(--fg);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 7px 10px;
    font-family: var(--mono);
    font-size: 12px;
    outline: none;
  }
  .dialog-body input:focus {
    border-color: var(--accent);
  }
  .dialog-body input:disabled {
    opacity: 0.5;
  }
  .dialog-body input.narrow {
    width: 130px;
  }

  .checkbox-row {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 8px;
  }
  .checkbox-row input {
    accent-color: var(--accent);
  }
  .checkbox-label {
    display: inline;
    margin: 0;
    font-size: 12px;
    color: var(--fg-muted);
    cursor: pointer;
  }

  .status-line {
    margin-top: 8px;
    font-size: 12px;
    color: var(--fg-muted);
  }

  .field-hint {
    margin-top: 6px;
    font-size: 12px;
    color: var(--fg-muted);
  }

  .security-note {
    margin: 18px 0 0;
    font-size: 12px;
    line-height: 1.5;
    color: var(--fg-muted);
  }

  .mono {
    font-family: var(--mono);
  }

  .msg {
    margin-top: 12px;
    font-family: var(--mono);
    font-size: 12px;
    white-space: pre-wrap;
  }
  .msg.error {
    color: var(--error);
  }
  .msg.info {
    color: var(--accent);
  }

  .dialog-footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 12px 18px;
    border-top: 1px solid var(--border);
  }

  .primary {
    background: var(--accent);
    color: var(--bg);
    border-color: var(--accent);
    font-weight: 600;
  }
  .primary:hover:not(:disabled) {
    border-color: var(--accent);
  }
</style>
