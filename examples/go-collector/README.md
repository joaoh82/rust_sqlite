# Edge / IoT event collector — Go + SQLRite

A small Go service that simulates an **edge / IoT event collector**: it
accepts telemetry over HTTP from many concurrent producers, writes each
event into a local file-backed [SQLRite](../../README.md) database, and
runs a background goroutine that drains the buffer to a remote sink. The
`.sqlrite` file is the **durable buffer between an unreliable network and
the upstream system** — it survives reboots, can be queried with SQL
on-device for diagnostics, and can be handed to `sqlrite-mcp` so an
operator can ask questions in natural language.

This is the Go entry in the [SQLR-38 example-apps umbrella](../README.md#end-to-end-example-apps).
It exercises the **Go SDK** (cgo via [`sqlrite-ffi`](../../sqlrite-ffi))
and SQLRite's **Phase 11 concurrent writes** (`BEGIN CONCURRENT` / MVCC,
[SQLR-22](../../docs/concurrent-writes.md)).

> **What this example proves — read this first.** Concurrent writes here
> is a **capability and correctness** feature, *not* a throughput win.
> An HTTP handler and a background uploader each hold their own
> independent transaction against one database — something a
> single-writer engine cannot express — and the engine detects
> row-level conflicts and tells the caller to retry. We measured the
> throughput honestly and, **on SQLRite v0, `BEGIN CONCURRENT` is
> slightly *slower* than a single mutex** for this workload (see
> [Measured throughput](#measured-throughput) for the numbers and
> [why](#why-concurrent-isnt-faster-yet)). We ship the numbers as we
> found them rather than an aspirational headline.

---

## Architecture

```
HTTP producers ──▶ Go service ──▶ events.sqlrite  (durable buffer)
  (many, concurrent)   │                  │
                       │                  ├──▶ uploader goroutine ──▶ sink (log / webhook)
                       │                  │        (its own BEGIN CONCURRENT txn)
                       │                  │
                       └── POST /events ──┘
                            BEGIN CONCURRENT txn per write

   GET /healthz · GET /stats          sqlrite-mcp --read-only (against a snapshot)
                                              └─▶ operator: "any errors in the last hour?"
```

Two writers run against one database at the same time:

- The **HTTP write path** (`POST /events`) inserts each event in its own
  `BEGIN CONCURRENT` transaction, pulled from the `database/sql` pool.
- The **uploader goroutine** scans the backlog, ships a batch, and writes
  its checkpoint (mark events uploaded + an `upload_runs` audit row + a
  `devices` upsert) in its own `BEGIN CONCURRENT` transaction(s).

They share the same backing engine through the Go driver's process-level
**sibling-handle registry** (Phase 11.11c) — every `sql.Open` /
pool-connection for the same file path mints a sibling off one shared
`Arc<Mutex<Database>>`, so each can hold its own concurrent transaction.

---

## Quick start

### Prerequisite — build the engine's C library once

The Go driver links `libsqlrite_c` (the [`sqlrite-ffi`](../../sqlrite-ffi)
cdylib) over cgo. From the repo root:

```bash
cargo build --release -p sqlrite-ffi
```

This example points cgo at `target/release/libsqlrite_c.{so,dylib}` via
the `sdk/go` driver's `#cgo` directives — no `LD_LIBRARY_PATH` dance.

### Run the collector

```bash
cd examples/go-collector
go run ./cmd/collector -db events.sqlrite -addr :8080
# or: make run
```

### Send events

```bash
# Single event (server stamps ts if you omit it):
curl -s -X POST localhost:8080/events \
  -H 'content-type: application/json' \
  -d '{"device_id":"sensor-001","kind":"telemetry","payload":{"temp_c":21.4}}'
# → {"accepted":1,"ids":[1]}

# A batch (JSON array):
curl -s -X POST localhost:8080/events \
  -H 'content-type: application/json' \
  -d '[{"device_id":"sensor-001","kind":"telemetry"},{"device_id":"sensor-002","kind":"error","payload":{"code":"E07"}}]'

# Ops:
curl -s localhost:8080/healthz   # 200 healthy / 503 when uploader is failing or buffer full
curl -s localhost:8080/stats     # counters + uploader health
```

### Drive concurrent load (and prove no drops)

```bash
# Fire 64 concurrent producers for 30s and assert every write landed:
go run ./cmd/loadgen -target http://localhost:8080 -workers 64 -duration 30s
# or: make loadtest
```

The load generator reports achieved req/s and **fails the run if any
write was dropped** (a non-200, non-503 response). 503s are counted
separately — they're backpressure, not data loss; a well-behaved producer
retries them.

---

## HTTP API

| Method · Path | Body | Response |
|---|---|---|
| `POST /events` | one event object, or a JSON array of them | `200 {"accepted":N,"ids":[…]}`; `400` on bad JSON / missing `device_id`/`kind` / malformed `payload`; `503` when the buffer is full |
| `GET /healthz` | — | `200`/`503` + `{ok, uploader, backlog, backlog_ok}` |
| `GET /stats`   | — | `200` + `{store, uploader}` counters |

Event shape:

```jsonc
{
  "device_id": "sensor-001",   // required
  "kind": "telemetry",         // required (e.g. telemetry | error | heartbeat)
  "payload": { "temp_c": 21 }, // optional; validated as JSON, stored in a JSON column
  "ts": 1748722000000          // optional unix-millis; server-stamps receipt time if 0
}
```

`id` is assigned by the server and returned. Auth / TLS are out of scope
(v1 assumes a sidecar / reverse proxy).

---

## Schema

```sql
events(id PK, device_id, kind, payload_json JSON, ts, uploaded_at)
       -- uploaded_at IS NULL  ⇒  the upload backlog
devices(id PK, device_key, label, last_seen_at)
upload_runs(id PK, started_at, finished_at, event_count, status, error)
       -- the uploader's per-cycle audit trail (success AND failure rows)
```

With `-indexed`, a single-column B-tree index on `events(device_id)` is
created at first launch (it speeds per-device diagnostic queries; the
write-side cost is measured below).

---

## Measured throughput

All numbers below are **measured on this machine, not estimated** —
reproduce them with the commands shown. They are the honest result, not
the result we hoped for.

> **Environment.** Apple M1 Pro (10 cores), macOS 14 (Darwin 23.5),
> Go 1.24, `sqlrite-engine` 0.10.2, release `libsqlrite_c`. Absolute
> numbers will differ on your hardware; the **ratios between modes** are
> the point.

### 1. Insert throughput — `make bench`

A fresh DB per cell; N goroutines insert events (one durable commit each)
as fast as they can.

```bash
go run ./cmd/loadgen -bench -workers 16 -duration 8s -devices 50
```

| write mode | indexed | events/sec | vs serialized |
|---|---|---:|---:|
| serialized (single mutex) | no  | **156** | 1.00× |
| concurrent (`BEGIN CONCURRENT`) | no  | 142 | 0.91× |
| concurrent (`BEGIN CONCURRENT`) | yes | 136 | 0.87× |

`BEGIN CONCURRENT` is ~9% **slower** than a single mutex here, and the
secondary index costs another ~4%. Throughput is ~150 durable
single-event commits/sec regardless of mode (see [why](#why-concurrent-isnt-faster-yet)).

### 2. Insert tail latency under a concurrent checkpoint writer — `-contention`

A background goroutine runs a wide multi-statement transaction in a loop
(the uploader's shape) while producers insert events and time each
insert. This is where you'd *expect* concurrent writes to win by avoiding
head-of-line blocking.

```bash
go run ./cmd/loadgen -contention -workers 16 -duration 8s
```

| write mode | p50 | p90 | p99 | max |
|---|---:|---:|---:|---:|
| serialized | **146 ms** | **155 ms** | **164 ms** | **181 ms** |
| concurrent | 184 ms | 265 ms | 333 ms | 431 ms |

Concurrent mode's tail is ~2× worse, not better — the per-transaction
snapshot clone (below) dominates any head-of-line-blocking it removes.

### 3. Disjoint-row batched writers (MVCC's textbook best case) — `-disjoint`

Each writer owns its own row range and updates it in 20-statement
transactions, so there are **zero conflicts**. Both modes amortize the
per-commit cost over the batch; serialized uses an explicit `BEGIN…COMMIT`
(a fair single-writer baseline, not autocommit-per-row).

```bash
go run ./cmd/loadgen -disjoint -workers 8 -duration 8s
```

| write mode | updates/sec | conflicts |
|---|---:|---:|
| serialized | **2,655** | 0 |
| concurrent | 2,018 | 0 |

Even in the case MVCC is built for, v0 `BEGIN CONCURRENT` is ~0.76×.

### Correctness, which is the actual win

```bash
go run ./cmd/loadgen -target http://localhost:8080 -workers 32 -duration 6s
# accepted (200): 723  (≈120 events/sec)   backpressure (503): 0   failed: 0
# OK: no writes dropped.
# server /stats → events_written: 723, events_uploaded: 723, backlog: 0, commit_conflicts: 0
```

32 producers and a background uploader, all writing one database
concurrently, **zero dropped events** — and when two transactions *do*
collide on a row, the loser gets `ErrBusy` and the store's retry loop
re-runs it. That correctness-under-concurrency is the thing a
single-writer engine fundamentally cannot give you.

### Why concurrent isn't faster yet

All three SQLRite v0 limitations are documented in
[`docs/concurrent-writes.md` → Limitations](../../docs/concurrent-writes.md#limitations):

1. **Every operation still serializes through one per-database
   `Arc<Mutex<Database>>`.** Phase 11 made multi-writer a *capability*,
   not a throughput win — "every operation still serializes through the
   per-database mutex."
2. **`BEGIN CONCURRENT` deep-clones the table set twice per
   transaction** (working copy + begin snapshot). For single-statement
   writes that's pure overhead with no contention to relieve; for the
   contention test it's what fattens the tail.
3. **Every commit triggers an O(N) bottom-up B-tree rebuild via the
   legacy save path.** This caps durable commit throughput at ~150/sec
   here regardless of mode, and won't amortize to checkpoint time until
   the parked checkpoint-drain follow-up lands.

The follow-ups that would flip these numbers — column-level
copy-on-write table cloning, and folding MVCC commits into the pager at
checkpoint time instead of rebuilding on every commit — are explicitly
carved out in the plan doc. Until they land, **prefer the default
serialized path for raw throughput; reach for `BEGIN CONCURRENT` when you
need independent in-process writers with conflict detection.**

The collector defaults to **concurrent** mode because demonstrating that
capability is the point of this example. Switch with `-mode serialized`.

---

## How it works (engine constraints worth knowing)

The Go SDK binds only to SQLRite's public surface, so this example also
documents the v0 sharp edges it had to design around — each verified
against the engine, each with a code comment pointing here:

| Constraint | Where it bites | How the collector handles it |
|---|---|---|
| **No parameter binding** in the Go driver | every write | All values are inlined through `internal/store/sqlquote.go`, the single chokepoint that escapes text (doubled `''`) and validates JSON. |
| **`CREATE TABLE IF NOT EXISTS` not honored**; `sqlrite_master` not queryable | reopening a populated DB | `migrate()` probes for the `events` table with a `SELECT` and only runs DDL on a fresh database. |
| **`CREATE INDEX` rejected under `journal_mode = mvcc`** | the optional index | All DDL runs in WAL mode *before* the MVCC switch; the index choice is fixed at DB-creation time. |
| **`BEGIN CONCURRENT` commit batch capped at 4 KiB** (the encoded row image, not just the SQL) | a large checkpoint, and any single oversized row | Two guards: event payloads are bounded at ingest (`maxPayloadBytes`, returns `400`) so any one row commits; and `CommitUpload` marks rows in adaptively-sized chunks that halve on a cap error down to one-per-commit (`writeAdaptive`). Relaxes the checkpoint from atomic to incremental → at-least-once delivery. |
| **`AUTOINCREMENT` rowids collide under MVCC** | concurrent inserts | Event ids are assigned application-side from an atomic counter seeded off `MAX(id)` at open. |
| **`IS NULL` never uses an index** | the backlog scan | `WHERE uploaded_at IS NULL` is a full scan by design — fine for a bounded edge buffer; the optional index is on `device_id` instead. |

Delivery semantics: the uploader **ships before it marks**, and marks
incrementally, so a crash mid-checkpoint can re-ship a batch but never
loses one — standard **at-least-once** delivery.

---

## Configuration

```
-db              database file path (default events.sqlrite)
-addr            HTTP listen address (default :8080)
-mode            concurrent | serialized (default concurrent)
-indexed         create a secondary index on events(device_id) (default false)
-max-conns       database/sql pool ceiling (default 8)
-max-backlog     reject writes with 503 once backlog reaches this; 0 = unlimited (default 50000)
-upload-interval uploader drain interval (default 1s)
-upload-batch    max events per uploader cycle (default 200)
-sink            log | webhook (default log)
-webhook-url     sink URL (required when -sink=webhook)
-flaky-every     wrap the sink to fail every Nth cycle (demo backpressure; 0 = off)
```

Demonstrate backpressure during a simulated outage:

```bash
go run ./cmd/collector -flaky-every 3 -max-backlog 500
# watch /stats: backlog climbs while the sink fails, then drains on recovery;
# POST /events returns 503 once backlog hits 500.
```

---

## Docker

The build context **must be the repository root** (the image compiles
`libsqlrite_c` from source, then the Go binaries against it):

```bash
# From the repo root:
docker build -f examples/go-collector/Dockerfile -t sqlrite-collector .
docker run -p 8080:8080 -v sqlrite-data:/data sqlrite-collector

# Or via compose (also from the repo root):
docker compose -f examples/go-collector/docker-compose.yml up --build
```

The DB lives on a volume so the durable buffer survives container
restarts — the same property a real edge box relies on.

### Build & distribution matrix

cgo binaries **cannot be cross-compiled** with the plain Go toolchain, so
the supported distribution story is per-arch images, not a fat binary:

| Target | How |
|---|---|
| linux/amd64, linux/arm64 | `docker buildx build --platform linux/amd64,linux/arm64 …` — the Rust + Go stages both honor the target platform |
| macOS (dev) | `make build` against a locally-built `libsqlrite_c` (host arch) |
| static musl binary | not provided in v1 — `libsqlrite_c` is a cdylib; a fully-static build would need a musl Rust target + `CGO_ENABLED=1` cross toolchain (follow-up) |

For a non-Docker deployment, ship the per-platform `libsqlrite_c` tarball
from a `sdk/go/v*` GitHub release alongside the collector binary and point
the dynamic linker at it (see [`sdk/go/README.md`](../../sdk/go/README.md)).

---

## "Ask the collector" (optional MCP sidecar)

[`sqlrite-mcp`](../../docs/mcp.md) exposes a database to Claude Desktop /
any MCP client read-only, so an operator can ask *"any device offline >
10 min?"* in natural language.

> **Cross-process caveat.** SQLRite's MVCC is **in-process only** —
> cross-process access still serializes through an exclusive file lock
> ([Limitations](../../docs/concurrent-writes.md#limitations)). So
> `sqlrite-mcp` **cannot** open the live `events.sqlrite` while the
> collector holds it for writing. Point it at a **snapshot** instead:

```bash
# Snapshot the buffer (stop the collector, or just copy the file):
cp events.sqlrite snapshot.sqlrite
sqlrite-mcp --read-only snapshot.sqlrite      # cargo install sqlrite-mcp
```

The `docker-compose.yml` ships a `mcp` profile that does exactly this
against a snapshot copy — see the comments there. Then wire its stdio
into your MCP client per [`docs/mcp.md`](../../docs/mcp.md).

---

## Layout

```
cmd/collector/      the service: HTTP front door + uploader wiring
cmd/loadgen/        load generator + the three measurement experiments
internal/store/     the durable buffer (schema, writes, both write modes)
internal/uploader/  background drain goroutine + pluggable Sink
internal/server/    HTTP handlers + backpressure
Dockerfile          multi-stage: build libsqlrite_c → build Go → slim runtime
docker-compose.yml  collector + optional read-only MCP sidecar (profile)
Makefile            lib / build / test / run / loadtest / bench / docker
```

## Tests

```bash
make test     # builds libsqlrite_c, then go test ./...
```

Covers the quote-escaping chokepoint, reopen-without-data-loss, the
chunked large-checkpoint path (which would otherwise hit the 4 KiB MVCC
cap), a deterministic `BEGIN CONCURRENT` write-write conflict + retry, the
many-writers-no-drops invariant, the uploader's success/failure/recovery
cycle, and the HTTP layer (validation, batch accept, 503 backpressure,
healthz/stats). CI builds + tests this example on Linux and macOS against
the in-repo engine (`go-collector` job in [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)).

## See also

- [`docs/concurrent-writes.md`](../../docs/concurrent-writes.md) — the MVCC / `BEGIN CONCURRENT` reference, including the limitations this example documents
- [`sdk/go/README.md`](../../sdk/go/README.md) — the Go driver, sibling handles, and the retryable-error sentinels
- [`examples/go/`](../go/) — the bare-bones Go SDK quick-start tour (this is the full app)
