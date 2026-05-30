// Typed thin wrappers around the `ask` settings command surface exposed
// by src-tauri/src/main.rs. The rest of the playground calls `invoke`
// inline from App.svelte; the settings surface earns its own module
// because the three-valued update shape needs a little massaging before
// it crosses the IPC boundary.

import { invoke } from "@tauri-apps/api/core";

export type AskSettings = {
  has_api_key: boolean;
  model: string;
  max_tokens: number;
  env_api_key_present: boolean;
};

// Three-valued: undefined → leave untouched, "" → clear, value → set.
// Frontend code uses `null` to mean "leave untouched" so we map it to
// the absent field when sending; Rust's serde sees that as None.
export type AskSettingsUpdate = {
  anthropic_api_key?: string | null;
  model?: string | null;
  max_tokens?: number | null;
};

function normaliseUpdate(u: AskSettingsUpdate): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  if (u.anthropic_api_key !== null && u.anthropic_api_key !== undefined) {
    out.anthropic_api_key = u.anthropic_api_key;
  }
  if (u.model !== null && u.model !== undefined) {
    out.model = u.model;
  }
  if (u.max_tokens !== null && u.max_tokens !== undefined) {
    out.max_tokens = u.max_tokens;
  }
  return out;
}

export const api = {
  getAskSettings: () => invoke<AskSettings>("get_ask_settings"),
  updateAskSettings: (update: AskSettingsUpdate) =>
    invoke<AskSettings>("update_ask_settings", {
      update: normaliseUpdate(update),
    }),
};
