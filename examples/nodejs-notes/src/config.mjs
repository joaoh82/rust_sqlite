// Defaults + small helpers around config paths and the database
// location. Everything is overridable via flags on the CLI; this
// module just picks reasonable fallbacks.

import { homedir, platform } from 'node:os';
import { resolve, join } from 'node:path';

export const DEFAULT_EMBEDDING_DIM = 384;
export const DEFAULT_CHUNK_TOKENS = 400;
export const DEFAULT_CHUNK_OVERLAP = 60;

/** Resolve the default DB path: ~/.sqlrite-notes/notes.sqlrite */
export function defaultDbPath() {
  return join(homedir(), '.sqlrite-notes', 'notes.sqlrite');
}

/**
 * Resolve a user-supplied directory path. Expands `~` and resolves
 * relative paths against the current working directory.
 *
 * @param {string} input
 * @returns {string}
 */
export function resolveDir(input) {
  if (!input) throw new Error('resolveDir(): empty path');
  let expanded = input;
  if (expanded === '~' || expanded.startsWith('~/')) {
    expanded = join(homedir(), expanded.slice(1));
  }
  return resolve(expanded);
}

/**
 * Resolve a user-supplied DB path. Same expansion rules as
 * `resolveDir` but doesn't require the parent directory to exist —
 * the caller (db.mjs) will mkdir as needed.
 *
 * @param {string | undefined} input
 * @returns {string}
 */
export function resolveDbPath(input) {
  return resolveDir(input ?? defaultDbPath());
}

/**
 * Best-guess location of Claude Desktop's config file.
 * Used only for the `init`'s "wire me up" hint — we never read or
 * write the file from the CLI.
 *
 * @returns {string}
 */
export function claudeDesktopConfigPath() {
  if (platform() === 'darwin') {
    return join(
      homedir(),
      'Library',
      'Application Support',
      'Claude',
      'claude_desktop_config.json',
    );
  }
  if (platform() === 'win32') {
    const appData =
      process.env.APPDATA ?? join(homedir(), 'AppData', 'Roaming');
    return join(appData, 'Claude', 'claude_desktop_config.json');
  }
  // Linux — Claude Desktop's Linux build is in beta; this is the
  // documented path. Falls back to ~/.config if XDG_CONFIG_HOME unset.
  const xdg = process.env.XDG_CONFIG_HOME ?? join(homedir(), '.config');
  return join(xdg, 'Claude', 'claude_desktop_config.json');
}
