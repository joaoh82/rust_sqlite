#!/usr/bin/env bash
# Run the SQLRite benchmark suite + aggregate criterion's per-bench
# JSON into one results envelope under benchmarks/results/.
#
# Invoked by `make bench` (lean: SQLRite + SQLite). The DuckDB-extended
# variant is `make bench-duckdb`; same script with FEATURES=duckdb.
#
# Usage:
#   scripts/run.sh                 # default: lean profile
#   FEATURES=duckdb scripts/run.sh # adds the DuckDB driver
#   OUTPUT=path/to.json scripts/run.sh
#
# Exits non-zero on failure; prints the path to the emitted results JSON.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

FEATURES="${FEATURES:-}"
OUTPUT="${OUTPUT:-}"
RUN_STARTED_AT="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
START_EPOCH="$(date -u '+%s')"

echo "==> running cargo bench (-p sqlrite-benchmarks${FEATURES:+ --features $FEATURES})"
if [ -n "$FEATURES" ]; then
  cargo bench -p sqlrite-benchmarks --features "$FEATURES"
else
  cargo bench -p sqlrite-benchmarks
fi

END_EPOCH="$(date -u '+%s')"
DURATION_SECS="$((END_EPOCH - START_EPOCH))"

echo "==> aggregating criterion output → results JSON"
AGG_ARGS=(
  --criterion-dir "target/criterion"
  --run-started-at "$RUN_STARTED_AT"
  --run-duration-secs "$DURATION_SECS"
)
if [ -n "$OUTPUT" ]; then
  AGG_ARGS+=(--output "$OUTPUT")
fi

cargo run -p sqlrite-benchmarks --bin aggregate --quiet -- "${AGG_ARGS[@]}"
