// `serve` — spawn `sqlrite-mcp --read-only <db>` with stdio inherited.
//
// The point of this command is to remove the "find the binary, then
// write the right `args` array" step from Claude Desktop config:
// users wire ONE thing (`sqlrite-notes serve`) and never have to
// know where `sqlrite-mcp` lives. The MCP client speaks JSON-RPC
// over our stdio; we just shovel it to/from the child.

import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';

/**
 * Try a sequence of well-known locations to find a `sqlrite-mcp`
 * binary. Order:
 *
 *   1. `SQLRITE_MCP_BIN` env var (explicit override).
 *   2. `which sqlrite-mcp` via `PATH`.
 *   3. `~/.cargo/bin/sqlrite-mcp` (cargo install default).
 *
 * @returns {string | null}
 */
export function locateMcpBinary() {
  const env = process.env.SQLRITE_MCP_BIN;
  if (env) {
    if (!existsSync(env)) {
      throw new Error(
        `SQLRITE_MCP_BIN=${env} is set but the file doesn't exist.`,
      );
    }
    return env;
  }

  // PATH lookup. `process.env.PATH` is the only thing we can portably
  // check without shelling out; spawning `which` adds latency for no
  // benefit since `spawn(name)` will already use PATH on Unix.
  const pathDirs = (process.env.PATH ?? '').split(process.platform === 'win32' ? ';' : ':');
  const exeName = process.platform === 'win32' ? 'sqlrite-mcp.exe' : 'sqlrite-mcp';
  for (const dir of pathDirs) {
    if (!dir) continue;
    const candidate = join(dir, exeName);
    if (existsSync(candidate)) return candidate;
  }

  // Cargo install fallback.
  const cargoBin = join(homedir(), '.cargo', 'bin', exeName);
  if (existsSync(cargoBin)) return cargoBin;

  return null;
}

/**
 * Spawn `sqlrite-mcp --read-only <db>` with stdio inherited. Returns
 * a Promise that resolves with the child's exit code.
 *
 * @param {{ dbPath: string, extraArgs?: string[], stderr?: NodeJS.WritableStream }} args
 * @returns {Promise<number>}
 */
export function spawnMcpServer({ dbPath, extraArgs = [], stderr }) {
  const bin = locateMcpBinary();
  if (!bin) {
    throw new Error(
      'sqlrite-mcp binary not found.\n' +
        '\n' +
        'Install it one of these ways:\n' +
        '  cargo install sqlrite-mcp\n' +
        '  # or download from https://github.com/joaoh82/rust_sqlite/releases\n' +
        '\n' +
        'You can also override the lookup with SQLRITE_MCP_BIN=/path/to/sqlrite-mcp.\n',
    );
  }

  // Build args. `--read-only` is the whole reason this wrapper exists:
  // we never want Claude (or any other MCP client) to mutate the notes
  // DB out from under the ingest pipeline.
  const args = [dbPath, '--read-only', ...extraArgs];

  return new Promise((resolve, reject) => {
    const child = spawn(bin, args, {
      // stdin / stdout MUST be inherited so the MCP client can talk to
      // the child directly. stderr we pipe to wherever the caller asks
      // (default: our own stderr).
      stdio: ['inherit', 'inherit', stderr ? 'pipe' : 'inherit'],
      env: process.env,
    });
    if (stderr && child.stderr) {
      child.stderr.pipe(stderr);
    }
    child.on('error', reject);
    child.on('exit', (code, signal) => {
      if (signal) {
        // Propagate the signal as a non-zero exit code so Claude
        // Desktop sees the failure cleanly.
        resolve(128 + (signalToNumber(signal) ?? 1));
      } else {
        resolve(code ?? 0);
      }
    });
    // Forward SIGINT / SIGTERM to the child so Ctrl-C in the parent
    // shuts the child down rather than orphaning it.
    const forward = (sig) => {
      if (!child.killed) child.kill(sig);
    };
    process.once('SIGINT', () => forward('SIGINT'));
    process.once('SIGTERM', () => forward('SIGTERM'));
  });
}

function signalToNumber(sig) {
  const map = { SIGINT: 2, SIGTERM: 15, SIGKILL: 9, SIGHUP: 1 };
  return map[sig];
}
