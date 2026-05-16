import test from 'node:test';
import assert from 'node:assert/strict';

import { buildConfig, renderInstructions } from '../src/claude-config.mjs';

test('buildConfig — default command is "sqlrite-notes"', () => {
  const cfg = buildConfig({ dbPath: '/tmp/notes.sqlrite' });
  assert.deepEqual(cfg, {
    mcpServers: {
      'sqlrite-notes': {
        command: 'sqlrite-notes',
        args: ['serve', '--db', '/tmp/notes.sqlrite'],
      },
    },
  });
});

test('buildConfig — explicit binPath wins', () => {
  const cfg = buildConfig({
    dbPath: '/tmp/notes.sqlrite',
    binPath: '/opt/sqlrite-notes/bin/sqlrite-notes.mjs',
  });
  assert.equal(
    cfg.mcpServers['sqlrite-notes'].command,
    '/opt/sqlrite-notes/bin/sqlrite-notes.mjs',
  );
});

test('renderInstructions — embeds JSON block and Claude Desktop path', () => {
  const out = renderInstructions({ dbPath: '/tmp/notes.sqlrite' });
  assert.match(out, /mcpServers/);
  assert.match(out, /sqlrite-notes/);
  assert.match(out, /"command": "sqlrite-notes"/);
  assert.match(out, /serve/);
  assert.match(out, /modelcontextprotocol/); // inspector hint
});
