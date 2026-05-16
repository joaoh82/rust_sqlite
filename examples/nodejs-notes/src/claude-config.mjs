// Generate a copy-paste-ready `claude_desktop_config.json` snippet so
// `sqlrite-notes init` can finish with "now paste this into Claude
// Desktop". We never WRITE the file (too easy to clobber the user's
// other MCP servers) — printing is honest and obvious.

import { claudeDesktopConfigPath } from './config.mjs';

/**
 * Build the per-server config block. Uses `sqlrite-notes serve --db
 * <path>` so the wiring stays the same regardless of where the user
 * installed `sqlrite-mcp`.
 *
 * @param {{ dbPath: string, binPath?: string }} args
 */
export function buildConfig({ dbPath, binPath }) {
  const command = binPath ?? 'sqlrite-notes';
  return {
    mcpServers: {
      'sqlrite-notes': {
        command,
        args: ['serve', '--db', dbPath],
      },
    },
  };
}

/**
 * Render the JSON block and the surrounding "wire-up" instructions.
 *
 * @param {{ dbPath: string, binPath?: string }} args
 * @returns {string}
 */
export function renderInstructions({ dbPath, binPath }) {
  const cfg = buildConfig({ dbPath, binPath });
  const json = JSON.stringify(cfg, null, 2);
  const cfgPath = claudeDesktopConfigPath();
  return [
    '─── Wire up Claude Desktop ───────────────────────────────────',
    '',
    `1. Open or create:`,
    `     ${cfgPath}`,
    '',
    '2. Merge this block into the file (preserving any other',
    `   "mcpServers" you already have):`,
    '',
    indent(json),
    '',
    '3. Restart Claude Desktop. The "sqlrite-notes" tools should',
    '   appear in the tool picker on the next chat.',
    '',
    'Tip: use Anthropic\'s MCP Inspector to dry-run the server before',
    'pointing Claude Desktop at it:',
    '',
    `     npx @modelcontextprotocol/inspector sqlrite-notes serve --db ${JSON.stringify(dbPath)}`,
    '',
    '──────────────────────────────────────────────────────────────',
  ].join('\n');
}

function indent(text) {
  return text
    .split(/\r?\n/)
    .map((l) => `   ${l}`)
    .join('\n');
}
