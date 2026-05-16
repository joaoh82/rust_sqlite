#!/usr/bin/env node
// Entry point — keep this thin so installations can `chmod +x`
// the file and dispatch into the real CLI module.
import { run } from '../src/cli.mjs';

run(process.argv.slice(2)).then(
  (code) => process.exit(code ?? 0),
  (err) => {
    process.stderr.write(`sqlrite-notes: ${err.message ?? err}\n`);
    if (process.env.SQLRITE_NOTES_DEBUG) {
      process.stderr.write(`${err.stack ?? ''}\n`);
    }
    process.exit(1);
  },
);
