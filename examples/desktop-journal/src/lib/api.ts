// Typed thin wrappers around the Tauri command surface exposed by
// src-tauri/src/main.rs. Co-locating the types here keeps the Svelte
// components from having to know about the IPC shape.

import { invoke } from "@tauri-apps/api/core";

export type EntrySummary = {
  id: number;
  date: string;
  title: string;
  excerpt: string;
  updated_at: number;
  tags: string[];
};

export type Entry = {
  id: number;
  date: string;
  title: string;
  content: string;
  created_at: number;
  updated_at: number;
  tags: string[];
};

export type TagSummary = {
  name: string;
  entry_count: number;
};

export type SearchHit = {
  id: number;
  date: string;
  title: string;
  snippet_html: string;
  score: number;
};

export type Stats = {
  total_entries: number;
  distinct_dates: number;
  total_tags: number;
};

export type AskResult = {
  sql: string;
  explanation: string;
  columns: string[];
  rows: string[][];
};

export type ExportSummary = {
  entry_count: number;
  dest: string;
};

export type OpenedDb = {
  path: string;
  entry_count: number;
};

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
  openDatabase: (path: string) =>
    invoke<OpenedDb>("open_database", { path }),
  currentDbPath: () => invoke<string | null>("current_db_path"),
  listEntries: (tag?: string | null) =>
    invoke<EntrySummary[]>("list_entries", { tag: tag ?? null }),
  getEntry: (id: number) => invoke<Entry>("get_entry", { id }),
  createEntry: (date: string, title: string, content: string, tags: string[]) =>
    invoke<number>("create_entry", { date, title, content, tags }),
  updateEntry: (
    id: number,
    date: string,
    title: string,
    content: string,
    tags: string[],
  ) => invoke<void>("update_entry", { id, date, title, content, tags }),
  deleteEntry: (id: number) => invoke<void>("delete_entry", { id }),
  listTags: () => invoke<TagSummary[]>("list_tags"),
  searchEntries: (query: string) =>
    invoke<SearchHit[]>("search_entries", { query }),
  stats: () => invoke<Stats>("stats"),
  askJournal: (question: string) =>
    invoke<AskResult>("ask_journal", { question }),
  exportDb: (dest: string) => invoke<void>("export_db", { dest }),
  exportMarkdown: (dir: string) =>
    invoke<ExportSummary>("export_markdown", { dir }),
  getAskSettings: () => invoke<AskSettings>("get_ask_settings"),
  updateAskSettings: (update: AskSettingsUpdate) =>
    invoke<AskSettings>("update_ask_settings", { update: normaliseUpdate(update) }),
};

export function todayISO(): string {
  const d = new Date();
  const yyyy = d.getFullYear().toString().padStart(4, "0");
  const mm = (d.getMonth() + 1).toString().padStart(2, "0");
  const dd = d.getDate().toString().padStart(2, "0");
  return `${yyyy}-${mm}-${dd}`;
}

export function formatTimestamp(unix: number): string {
  const d = new Date(unix * 1000);
  return d.toLocaleString();
}
