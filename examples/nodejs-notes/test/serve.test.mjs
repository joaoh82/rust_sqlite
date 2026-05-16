import test from 'node:test';
import assert from 'node:assert/strict';
import { writeFileSync, chmodSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { locateMcpBinary } from '../src/serve.mjs';

test('locateMcpBinary honors SQLRITE_MCP_BIN when the file exists', () => {
  const dir = mkdtempSync(join(tmpdir(), 'sqlrite-mcp-bin-test-'));
  try {
    const fakeBin = join(dir, 'fake-mcp');
    writeFileSync(fakeBin, '#!/bin/sh\necho fake\n');
    chmodSync(fakeBin, 0o755);

    const prev = process.env.SQLRITE_MCP_BIN;
    process.env.SQLRITE_MCP_BIN = fakeBin;
    try {
      assert.equal(locateMcpBinary(), fakeBin);
    } finally {
      if (prev === undefined) delete process.env.SQLRITE_MCP_BIN;
      else process.env.SQLRITE_MCP_BIN = prev;
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('locateMcpBinary throws if SQLRITE_MCP_BIN points at a missing file', () => {
  const prev = process.env.SQLRITE_MCP_BIN;
  process.env.SQLRITE_MCP_BIN = '/definitely/not/real/sqlrite-mcp';
  try {
    assert.throws(() => locateMcpBinary(), /SQLRITE_MCP_BIN/);
  } finally {
    if (prev === undefined) delete process.env.SQLRITE_MCP_BIN;
    else process.env.SQLRITE_MCP_BIN = prev;
  }
});
