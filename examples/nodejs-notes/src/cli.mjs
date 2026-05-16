// Top-level CLI dispatcher. Subcommands:
//
//   init <dir>     — create / replace the notes DB by ingesting <dir>.
//   refresh        — incremental re-ingest based on mtime + hash.
//   search "<q>"   — debug retrieval the way Claude would over MCP.
//   serve          — spawn sqlrite-mcp --read-only against the DB.
//   stats          — quick row counts.
//   config         — print the Claude Desktop wiring snippet.
//
// Flag parsing uses node:util's parseArgs so we have no external
// dep just for argv handling. Each subcommand owns its own option
// schema. Unknown / missing args print usage.

import { parseArgs } from 'node:util';

import { NotesDB } from './db.mjs';
import { ingest, refresh } from './ingest.mjs';
import { makeEmbedder } from './embeddings.mjs';
import { search, renderResults } from './search.mjs';
import { spawnMcpServer } from './serve.mjs';
import { renderInstructions } from './claude-config.mjs';
import {
  resolveDbPath,
  resolveDir,
  defaultDbPath,
  DEFAULT_EMBEDDING_DIM,
  DEFAULT_CHUNK_TOKENS,
  DEFAULT_CHUNK_OVERLAP,
} from './config.mjs';

const VERSION = '0.1.0';

const USAGE = `sqlrite-notes ${VERSION} — chat with your markdown notes via Claude Desktop + SQLRite MCP.

Usage:
  sqlrite-notes <command> [options]

Commands:
  init <dir>               Build (or rebuild) the notes index from <dir>.
  refresh                  Incremental re-ingest based on file mtime/hash.
  search "<query>"         Run hybrid retrieval against the index (debug).
  serve                    Spawn sqlrite-mcp --read-only against the DB.
  stats                    Print row counts.
  config                   Print the Claude Desktop config snippet.
  help                     Show this message.

Common options:
  --db <path>              Path to the SQLRite database file.
                           Default: ${defaultDbPath()}
  --embedder hash|openai   Embedding provider (default: hash, offline).
  --dim <N>                Vector dimension (default: ${DEFAULT_EMBEDDING_DIM}).
  --openai-model <name>    OpenAI embedding model (default: text-embedding-3-small).

Init / refresh options:
  --chunk-tokens <N>       Target chunk size in tokens (default: ${DEFAULT_CHUNK_TOKENS}).
  --chunk-overlap <N>      Chunk overlap in tokens (default: ${DEFAULT_CHUNK_OVERLAP}).

Search options:
  -k <N>                   Number of results to return (default: 5).
  -w <0..1>                BM25 vs vector weight (default: 0.5).

Environment:
  OPENAI_API_KEY                Required when --embedder openai.
  SQLRITE_NOTES_EMBEDDER        Default embedder (hash | openai).
  SQLRITE_NOTES_OPENAI_MODEL    Override OpenAI model id.
  SQLRITE_MCP_BIN               Explicit path to sqlrite-mcp for 'serve'.
`;

/**
 * Entry point. Returns the process exit code (0 = OK).
 *
 * @param {string[]} argv  arguments after `node bin/sqlrite-notes.mjs`
 */
export async function run(argv) {
  const [command, ...rest] = argv;
  if (!command || command === 'help' || command === '--help' || command === '-h') {
    process.stdout.write(USAGE);
    return 0;
  }
  if (command === 'version' || command === '--version' || command === '-V') {
    process.stdout.write(`sqlrite-notes ${VERSION}\n`);
    return 0;
  }

  switch (command) {
    case 'init':
      return cmdInit(rest);
    case 'refresh':
      return cmdRefresh(rest);
    case 'search':
      return cmdSearch(rest);
    case 'serve':
      return cmdServe(rest);
    case 'stats':
      return cmdStats(rest);
    case 'config':
      return cmdConfig(rest);
    default:
      process.stderr.write(`unknown command: ${command}\n\n`);
      process.stderr.write(USAGE);
      return 2;
  }
}

// ------------------------------------------------------------------
// init

async function cmdInit(argv) {
  const { values, positionals } = parseArgs({
    args: argv,
    allowPositionals: true,
    options: {
      db: { type: 'string' },
      embedder: { type: 'string' },
      dim: { type: 'string' },
      'openai-model': { type: 'string' },
      'chunk-tokens': { type: 'string' },
      'chunk-overlap': { type: 'string' },
    },
  });
  if (positionals.length === 0) {
    process.stderr.write('init: missing <dir>\n\nusage: sqlrite-notes init <dir> [--db path] [--embedder hash|openai]\n');
    return 2;
  }
  const root = resolveDir(positionals[0]);
  const dbPath = resolveDbPath(values.db);
  const dim = parseDim(values.dim);
  const embedder = makeEmbedder({
    kind: values.embedder,
    dim,
    model: values['openai-model'],
  });
  const db = new NotesDB(dbPath, { dim: embedder.dim });

  try {
    process.stdout.write(`sqlrite-notes ${VERSION}\n`);
    process.stdout.write(`  db:       ${dbPath}\n`);
    process.stdout.write(`  source:   ${root}\n`);
    process.stdout.write(`  embedder: ${embedder.name} (dim=${embedder.dim})\n`);

    const stats = await ingest({
      db,
      root,
      embedder,
      logger: (s) => process.stdout.write(`${s}\n`),
      chunkOpts: parseChunkOpts(values),
    });
    process.stdout.write(`\ningested ${stats.files} file(s), ${stats.chunks} chunk(s) in ${stats.elapsedMs} ms\n`);
    process.stdout.write('\n');
    process.stdout.write(renderInstructions({ dbPath }));
    process.stdout.write('\n');
    return 0;
  } finally {
    db.close();
  }
}

// ------------------------------------------------------------------
// refresh

async function cmdRefresh(argv) {
  const { values, positionals } = parseArgs({
    args: argv,
    allowPositionals: true,
    options: {
      db: { type: 'string' },
      embedder: { type: 'string' },
      dim: { type: 'string' },
      'openai-model': { type: 'string' },
      'chunk-tokens': { type: 'string' },
      'chunk-overlap': { type: 'string' },
      source: { type: 'string' },
    },
  });
  // <dir> is optional for refresh — if omitted, we re-ingest the same
  // tree that init recorded. (For now we just require it; we don't
  // store the source dir in the DB. Documented in the README.)
  const rootArg = values.source ?? positionals[0];
  if (!rootArg) {
    process.stderr.write(
      'refresh: pass the source directory as a positional (or --source <dir>).\n' +
        'We don\'t yet persist the source path inside the DB — see the README\n' +
        '"Known simplifications" section.\n',
    );
    return 2;
  }
  const root = resolveDir(rootArg);
  const dbPath = resolveDbPath(values.db);
  const dim = parseDim(values.dim);
  const embedder = makeEmbedder({
    kind: values.embedder,
    dim,
    model: values['openai-model'],
  });
  const db = new NotesDB(dbPath, { dim: embedder.dim });
  try {
    const stats = await refresh({
      db,
      root,
      embedder,
      logger: (s) => process.stdout.write(`${s}\n`),
      chunkOpts: parseChunkOpts(values),
    });
    process.stdout.write(
      `refreshed: ${stats.files} updated, ${stats.skipped} unchanged, ${stats.deleted} deleted (${stats.elapsedMs} ms)\n`,
    );
    return 0;
  } finally {
    db.close();
  }
}

// ------------------------------------------------------------------
// search

async function cmdSearch(argv) {
  const { values, positionals } = parseArgs({
    args: argv,
    allowPositionals: true,
    options: {
      db: { type: 'string' },
      embedder: { type: 'string' },
      dim: { type: 'string' },
      'openai-model': { type: 'string' },
      k: { type: 'string', short: 'k' },
      w: { type: 'string', short: 'w' },
    },
  });
  const query = positionals.join(' ').trim();
  if (!query) {
    process.stderr.write('search: missing query string.\n\nusage: sqlrite-notes search "<query>" [-k N] [-w 0..1]\n');
    return 2;
  }
  const dbPath = resolveDbPath(values.db);
  const dim = parseDim(values.dim);
  const embedder = makeEmbedder({
    kind: values.embedder,
    dim,
    model: values['openai-model'],
  });
  const db = new NotesDB(dbPath, { dim: embedder.dim, readOnly: true });
  try {
    const hits = await search({
      db,
      embedder,
      query,
      k: parseInt2(values.k, 5),
      weight: parseFloat2(values.w, 0.5),
    });
    process.stdout.write(renderResults(query, hits));
    return 0;
  } finally {
    db.close();
  }
}

// ------------------------------------------------------------------
// serve

async function cmdServe(argv) {
  const { values } = parseArgs({
    args: argv,
    options: {
      db: { type: 'string' },
    },
  });
  const dbPath = resolveDbPath(values.db);
  // sqlrite-mcp opens its own database, so we don't touch it here —
  // just pass the resolved path through.
  const code = await spawnMcpServer({ dbPath });
  return code;
}

// ------------------------------------------------------------------
// stats

async function cmdStats(argv) {
  const { values } = parseArgs({
    args: argv,
    options: {
      db: { type: 'string' },
    },
  });
  const dbPath = resolveDbPath(values.db);
  const db = new NotesDB(dbPath, { readOnly: true });
  try {
    const s = db.stats();
    process.stdout.write(`db: ${dbPath}\n`);
    process.stdout.write(`documents:    ${s.documents}\n`);
    process.stdout.write(`chunks:       ${s.chunks}\n`);
    process.stdout.write(`embedding dim: ${s.dim}\n`);
    return 0;
  } finally {
    db.close();
  }
}

// ------------------------------------------------------------------
// config

async function cmdConfig(argv) {
  const { values } = parseArgs({
    args: argv,
    options: {
      db: { type: 'string' },
      bin: { type: 'string' },
    },
  });
  const dbPath = resolveDbPath(values.db);
  process.stdout.write(renderInstructions({ dbPath, binPath: values.bin }));
  process.stdout.write('\n');
  return 0;
}

// ------------------------------------------------------------------
// shared option parsing

function parseDim(raw) {
  if (raw === undefined) return DEFAULT_EMBEDDING_DIM;
  const n = parseInt(raw, 10);
  if (!Number.isFinite(n) || n <= 0) {
    throw new Error(`--dim: invalid value ${JSON.stringify(raw)}`);
  }
  return n;
}

function parseChunkOpts(values) {
  return {
    targetTokens: parseInt2(values['chunk-tokens'], DEFAULT_CHUNK_TOKENS),
    overlapTokens: parseInt2(values['chunk-overlap'], DEFAULT_CHUNK_OVERLAP),
  };
}

function parseInt2(raw, fallback) {
  if (raw === undefined) return fallback;
  const n = parseInt(raw, 10);
  if (!Number.isFinite(n) || n < 0) {
    throw new Error(`invalid integer: ${JSON.stringify(raw)}`);
  }
  return n;
}

function parseFloat2(raw, fallback) {
  if (raw === undefined) return fallback;
  const n = parseFloat(raw);
  if (!Number.isFinite(n)) {
    throw new Error(`invalid number: ${JSON.stringify(raw)}`);
  }
  return n;
}
