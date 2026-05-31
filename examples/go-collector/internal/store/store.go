// Package store is the durable buffer at the heart of the collector:
// a thin layer over a file-backed SQLRite database that an HTTP write
// path and a background uploader hammer concurrently.
//
// The whole example exists to exercise SQLRite's Phase 11 concurrent
// writes (SQLR-22). Two write strategies live behind one API:
//
//   - WriteMode == Concurrent: every write runs in its own
//     `BEGIN CONCURRENT` transaction on a sibling connection pulled
//     from the `database/sql` pool, with the canonical retry-on-Busy
//     loop. This is the mode the collector ships with.
//   - WriteMode == Serialized: every write runs through a single
//     connection guarded by a Go mutex — the naive "one big lock
//     around the DB" shape you'd be stuck with before concurrent
//     writes shipped. It exists so the loadgen can measure the
//     difference honestly (see cmd/loadgen).
//
// Engine constraints that shaped the schema and SQL (all from
// docs/supported-sql.md + docs/concurrent-writes.md):
//
//   - No parameter binding in the Go SDK → values are inlined via the
//     helpers in sqlquote.go.
//   - `CREATE TABLE IF NOT EXISTS` is not honored and `sqlrite_master`
//     isn't queryable → migrate() probes for the events table with a
//     SELECT and only runs DDL on a fresh database.
//   - `CREATE INDEX` is rejected once `journal_mode = mvcc` → all DDL,
//     including the optional secondary index, runs at migrate time
//     before MVCC is switched on.
//   - A single BEGIN CONCURRENT commit batch is capped at 4 KiB → event
//     payloads are bounded at ingest (see maxPayloadBytes) so any one
//     row commits, and the uploader's checkpoint marks rows in
//     adaptively-sized chunks that halve on a cap error (see
//     CommitUpload / writeAdaptive) rather than one wide transaction.
//   - AUTOINCREMENT rowids collide under MVCC (two concurrent inserts
//     can allocate the same rowid → Busy) → event ids are assigned
//     application-side from an atomic counter seeded off MAX(id).
//   - `IS NULL` never uses an index → the uploader's backlog scan is a
//     full scan by design; the optional index is on device_id, which
//     genuinely accelerates per-device diagnostic queries.
package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"sync"
	"sync/atomic"

	sqlrite "github.com/joaoh82/rust_sqlite/sdk/go"
)

// WriteMode selects how writes reach the engine. See the package doc.
type WriteMode int

const (
	// Concurrent runs each write in its own BEGIN CONCURRENT transaction
	// on a pooled sibling connection, retrying on Busy.
	Concurrent WriteMode = iota
	// Serialized funnels every write through one connection under a Go
	// mutex — the pre-Phase-11 "giant lock" baseline.
	Serialized
)

func (m WriteMode) String() string {
	if m == Serialized {
		return "serialized"
	}
	return "concurrent"
}

// Errors surfaced to callers.
var (
	// ErrBadPayload wraps a client payload that isn't valid JSON.
	ErrBadPayload = errors.New("invalid event payload")
	// ErrRetryExhausted means a BEGIN CONCURRENT write hit Busy more
	// times than the configured budget. In practice this never fires
	// for disjoint-row workloads; it's a backstop against livelock.
	ErrRetryExhausted = errors.New("write retry budget exhausted")
)

// Event is one telemetry record. The id is assigned by the store; the
// client supplies the rest. TS is unix-millis — clients may stamp it,
// or leave it 0 and the server fills in receipt time.
type Event struct {
	ID       int64           `json:"id,omitempty"`
	DeviceID string          `json:"device_id"`
	Kind     string          `json:"kind"`
	Payload  json.RawMessage `json:"payload,omitempty"`
	TS       int64           `json:"ts,omitempty"`
}

// UploadRun is the audit-trail row the uploader writes per cycle.
type UploadRun struct {
	StartedAt  int64
	FinishedAt int64
	EventCount int
	Status     string // "success" | "error"
	Error      string
}

// Options configures Open.
type Options struct {
	Path         string    // database file path (must be file-backed for sibling sharing)
	Mode         WriteMode // Concurrent (default) or Serialized
	Indexed      bool      // create a secondary index on events(device_id) at setup
	MaxOpenConns int       // database/sql pool ceiling (default 8)
	MaxRetries   int       // BEGIN CONCURRENT retry budget (default 100)
	MarkChunk    int       // rows per checkpoint commit (default 20; see CommitUpload)
}

// Store owns the database handle and the in-process counters that back
// /stats and the backpressure decision without a round-trip to disk.
type Store struct {
	db   *sql.DB
	opts Options

	nextID    atomic.Int64 // app-assigned event ids
	nextRunID atomic.Int64 // app-assigned upload_runs ids
	nextDevID atomic.Int64 // app-assigned devices ids

	backlog   atomic.Int64 // events with uploaded_at IS NULL
	written   atomic.Int64 // total events accepted
	uploaded  atomic.Int64 // total events marked uploaded
	conflicts atomic.Int64 // BEGIN CONCURRENT Busy retries observed

	// Serialized mode pins one connection under serMu.
	serMu   sync.Mutex
	serConn *sql.Conn

	// devices in-memory key→rowid map, maintained by the uploader off
	// the hot path. Guarded by devMu.
	devMu sync.Mutex
	devs  map[string]int64
}

// Open creates/opens the database, applies the schema on first use, and
// (in Concurrent mode) switches the database into MVCC. DDL runs before
// the MVCC switch because CREATE INDEX is rejected once
// journal_mode = mvcc; see migrate for the fresh-vs-reopen detection.
func Open(ctx context.Context, opts Options) (*Store, error) {
	if opts.MaxOpenConns <= 0 {
		opts.MaxOpenConns = 8
	}
	if opts.MaxRetries <= 0 {
		opts.MaxRetries = 100
	}
	if opts.MarkChunk <= 0 {
		// Keep each checkpoint commit comfortably under the engine's
		// 4 KiB MVCC commit-batch frame cap (see CommitUpload). A wide
		// event row encodes to ~120 bytes in the MVCC log, so 20 rows
		// (~2.4 KiB) leaves healthy headroom.
		opts.MarkChunk = 20
	}
	db, err := sql.Open(sqlrite.DriverName, opts.Path)
	if err != nil {
		return nil, fmt.Errorf("open %q: %w", opts.Path, err)
	}
	db.SetMaxOpenConns(opts.MaxOpenConns)
	db.SetMaxIdleConns(opts.MaxOpenConns)

	s := &Store{db: db, opts: opts, devs: make(map[string]int64)}

	if err := s.migrate(ctx); err != nil {
		_ = db.Close()
		return nil, err
	}
	if err := s.seed(ctx); err != nil {
		_ = db.Close()
		return nil, err
	}
	if opts.Mode == Serialized {
		c, err := db.Conn(ctx)
		if err != nil {
			_ = db.Close()
			return nil, fmt.Errorf("pin serialized conn: %w", err)
		}
		s.serConn = c
	}
	return s, nil
}

// Close releases the pinned connection (if any) and the pool.
func (s *Store) Close() error {
	if s.serConn != nil {
		_ = s.serConn.Close()
	}
	return s.db.Close()
}

// migrate creates the schema on a fresh database and is a no-op on
// reopen. Two engine constraints (both verified against the v0 engine)
// shape this:
//
//   - `CREATE TABLE IF NOT EXISTS` is NOT honored — a second create of
//     an existing table errors "table already exists" — and the
//     `sqlrite_master` catalog isn't queryable. So we detect a fresh
//     database by probing for the events table with a cheap SELECT and
//     only run DDL when it's absent.
//   - `CREATE INDEX` is rejected once `journal_mode = mvcc`. All DDL
//     (tables + the optional index) therefore runs on the fresh path,
//     in WAL mode, *before* the MVCC switch. On reopen the index already
//     exists, so we never re-issue it.
func (s *Store) migrate(ctx context.Context) error {
	fresh := !s.tableExists(ctx, "events")

	if fresh {
		ddl := []string{
			`CREATE TABLE events (
				id           INTEGER PRIMARY KEY,
				device_id    TEXT NOT NULL,
				kind         TEXT NOT NULL,
				payload_json JSON,
				ts           INTEGER NOT NULL,
				uploaded_at  INTEGER
			)`,
			`CREATE TABLE devices (
				id           INTEGER PRIMARY KEY,
				device_key   TEXT NOT NULL,
				label        TEXT,
				last_seen_at INTEGER
			)`,
			`CREATE TABLE upload_runs (
				id          INTEGER PRIMARY KEY,
				started_at  INTEGER NOT NULL,
				finished_at INTEGER,
				event_count INTEGER NOT NULL,
				status      TEXT NOT NULL,
				error       TEXT
			)`,
		}
		if s.opts.Indexed {
			// Single-column B-tree index (composite indexes are
			// unsupported). Accelerates per-device diagnostic queries
			// (`WHERE device_id = '...'`); the trade is extra index
			// maintenance on every concurrent write, which the loadgen
			// measures. The index choice is fixed at DB-creation time —
			// reopening with a different -indexed flag does not add or
			// drop it (we'd have to CREATE INDEX under MVCC, which the
			// engine rejects).
			ddl = append(ddl,
				`CREATE INDEX idx_events_device ON events (device_id)`)
		}
		for _, q := range ddl {
			if _, err := s.db.ExecContext(ctx, q); err != nil {
				return fmt.Errorf("migrate: %w", err)
			}
		}
	}

	// PRAGMA journal_mode is idempotent — safe to set on both the fresh
	// and reopen paths regardless of whether the mode persisted.
	if s.opts.Mode == Concurrent {
		if _, err := s.db.ExecContext(ctx, "PRAGMA journal_mode = mvcc"); err != nil {
			return fmt.Errorf("enable mvcc: %w", err)
		}
	}
	return nil
}

// tableExists probes for a table with a zero-row SELECT. The engine has
// no queryable catalog and rejects `CREATE TABLE IF NOT EXISTS`, so this
// probe is how we tell a fresh database from a reopened one. A query
// error (the engine returns "Table '<name>' not found") means absent.
func (s *Store) tableExists(ctx context.Context, name string) bool {
	rows, err := s.db.QueryContext(ctx, fmt.Sprintf("SELECT id FROM %s LIMIT 1", name))
	if err != nil {
		return false
	}
	_ = rows.Close()
	return true
}

// seed primes the atomic id counters and the backlog gauge from
// whatever's already on disk, so reopening a populated buffer doesn't
// reuse ids or lose the un-uploaded count.
func (s *Store) seed(ctx context.Context) error {
	maxID, err := s.scalarMax(ctx, "events")
	if err != nil {
		return err
	}
	s.nextID.Store(maxID)

	maxRun, err := s.scalarMax(ctx, "upload_runs")
	if err != nil {
		return err
	}
	s.nextRunID.Store(maxRun)

	maxDev, err := s.scalarMax(ctx, "devices")
	if err != nil {
		return err
	}
	s.nextDevID.Store(maxDev)

	var backlog int64
	row := s.db.QueryRowContext(ctx, "SELECT COUNT(*) FROM events WHERE uploaded_at IS NULL")
	if err := row.Scan(&backlog); err != nil {
		return fmt.Errorf("seed backlog: %w", err)
	}
	s.backlog.Store(backlog)

	// Load known devices into the in-memory map.
	rows, err := s.db.QueryContext(ctx, "SELECT id, device_key FROM devices")
	if err != nil {
		return fmt.Errorf("seed devices: %w", err)
	}
	defer rows.Close()
	for rows.Next() {
		var id int64
		var key string
		if err := rows.Scan(&id, &key); err != nil {
			return fmt.Errorf("seed devices scan: %w", err)
		}
		s.devs[key] = id
	}
	return rows.Err()
}

// scalarMax returns MAX(id) for a table, or 0 when the table is empty
// (MAX over no rows is NULL). COALESCE isn't supported by the engine,
// so we scan into a nullable.
func (s *Store) scalarMax(ctx context.Context, table string) (int64, error) {
	var v sql.NullInt64
	row := s.db.QueryRowContext(ctx, fmt.Sprintf("SELECT MAX(id) FROM %s", table))
	if err := row.Scan(&v); err != nil {
		return 0, fmt.Errorf("max(id) from %s: %w", table, err)
	}
	if !v.Valid {
		return 0, nil
	}
	return v.Int64, nil
}

// InsertEvent durably appends one event and returns its assigned id.
// This is the hot path — a single INSERT per call, one durable commit.
func (s *Store) InsertEvent(ctx context.Context, ev Event) (int64, error) {
	jsonLit, err := quoteJSON(ev.Payload)
	if err != nil {
		return 0, fmt.Errorf("%w: %v", ErrBadPayload, err)
	}
	id := s.nextID.Add(1)
	q := fmt.Sprintf(
		"INSERT INTO events (id, device_id, kind, payload_json, ts, uploaded_at) "+
			"VALUES (%d, %s, %s, %s, %d, NULL)",
		id, quoteText(ev.DeviceID), quoteText(ev.Kind), jsonLit, ev.TS,
	)
	if err := s.write(ctx, q); err != nil {
		return 0, err
	}
	s.written.Add(1)
	s.backlog.Add(1)
	return id, nil
}

// FetchPending returns up to limit un-uploaded events, oldest first.
// Reads run outside any transaction, so they see the latest committed
// state. The WHERE uploaded_at IS NULL is a full scan by design (NULLs
// aren't indexed) — fine for a bounded edge buffer.
func (s *Store) FetchPending(ctx context.Context, limit int) ([]Event, error) {
	q := fmt.Sprintf(
		"SELECT id, device_id, kind, payload_json, ts FROM events "+
			"WHERE uploaded_at IS NULL ORDER BY id LIMIT %d", limit)
	rows, err := s.db.QueryContext(ctx, q)
	if err != nil {
		return nil, fmt.Errorf("fetch pending: %w", err)
	}
	defer rows.Close()

	var out []Event
	for rows.Next() {
		var ev Event
		var payload sql.NullString
		if err := rows.Scan(&ev.ID, &ev.DeviceID, &ev.Kind, &payload, &ev.TS); err != nil {
			return nil, fmt.Errorf("scan pending: %w", err)
		}
		if payload.Valid {
			ev.Payload = json.RawMessage(payload.String)
		}
		out = append(out, ev)
	}
	return out, rows.Err()
}

// CommitUpload records the outcome of one uploader cycle: it marks the
// shipped events uploaded (when any), upserts the devices they came
// from, and always writes an upload_runs audit row — even for a failed
// run, so the failure is durably recorded. Passing markIDs == nil
// records a failed/empty run without marking anything uploaded.
//
// Why this isn't one big transaction. SQLRite's MVCC durability frame
// caps a single BEGIN CONCURRENT commit batch at 4 KiB (verified: a
// wide-row UPDATE over ~150 rows overflows it with
// "encoded batch exceeds 4096-byte frame body cap"). So instead of one
// transaction marking the whole batch, we mark in chunks of MarkChunk
// rows — each chunk its own transaction — which keeps every commit
// under the cap. This relaxes the checkpoint from atomic to
// incremental: a failure partway leaves the earlier chunks marked and
// the rest buffered for the next cycle. Combined with the uploader
// shipping before marking, that's standard at-least-once delivery (a
// row can ship twice but never be lost). The audit row is written in
// the final chunk.
//
// In Serialized mode there is no MVCC frame and therefore no cap, but
// chunking is still correct (just more BEGIN/COMMITs), so we take the
// same path in both modes for simplicity.
func (s *Store) CommitUpload(ctx context.Context, run UploadRun, markIDs []int64, batch []Event) error {
	// Per-row mark statements (not a single IN-list) so chunking by
	// statement count maps directly to rows-per-commit.
	var marks []string
	for _, id := range markIDs {
		marks = append(marks, fmt.Sprintf(
			"UPDATE events SET uploaded_at = %d WHERE id = %d", run.FinishedAt, id))
	}

	// Tail statements: device upserts + the audit row, run after the
	// marks in their own (final) chunk(s).
	tail := s.deviceUpserts(batch)
	runID := s.nextRunID.Add(1)
	tail = append(tail, fmt.Sprintf(
		"INSERT INTO upload_runs (id, started_at, finished_at, event_count, status, error) "+
			"VALUES (%d, %d, %d, %d, %s, %s)",
		runID, run.StartedAt, run.FinishedAt, run.EventCount,
		quoteText(run.Status), nullableText(run.Error)))

	// Mark events in adaptively-sized chunks. writeAdaptive returns the
	// committed prefix count (== events marked, since marks are 1:1 with
	// rows) even on error, so the gauges stay accurate on a partial
	// failure.
	marked, err := s.writeAdaptive(ctx, marks, s.opts.MarkChunk)
	if marked > 0 {
		s.uploaded.Add(int64(marked))
		s.backlog.Add(-int64(marked))
	}
	if err != nil {
		return err
	}

	// Tail (device upserts + audit row) — same adaptive chunking. These
	// aren't event marks, so their committed count doesn't touch the
	// backlog/uploaded gauges.
	if _, err := s.writeAdaptive(ctx, tail, s.opts.MarkChunk); err != nil {
		return err
	}
	return nil
}

// writeAdaptive runs stmts as a sequence of transactions, starting at
// `chunk` statements per commit and **halving on a frame-cap error**
// down to a floor of one statement per commit. This is how the
// checkpoint stays under SQLRite's 4 KiB MVCC commit-batch cap without
// the caller having to know the encoded size of each row up front: it
// optimistically batches, and only pays the split cost when a batch
// actually overflows. Because every single event row is bounded under
// the cap (see maxPayloadBytes), the one-statement floor always commits.
//
// Returns the number of statements successfully committed (a prefix of
// stmts) so the caller can keep its gauges accurate even when a later
// chunk fails for a non-cap reason.
func (s *Store) writeAdaptive(ctx context.Context, stmts []string, chunk int) (int, error) {
	if chunk < 1 {
		chunk = 1
	}
	done := 0
	for done < len(stmts) {
		end := done + chunk
		if end > len(stmts) {
			end = len(stmts)
		}
		err := s.write(ctx, stmts[done:end]...)
		if err != nil {
			if isFrameCapError(err) && end-done > 1 {
				// This batch overflowed the MVCC frame; shrink and retry
				// the same prefix. The smaller chunk size sticks for the
				// rest of the checkpoint.
				chunk = (end - done) / 2
				continue
			}
			return done, err
		}
		done = end
	}
	return done, nil
}

// isFrameCapError reports whether err is SQLRite's MVCC commit-batch
// size-cap error. The FFI surfaces it as a General error with a stable
// message fragment; matching the fragment is the only handle we have
// (it isn't a distinct status code).
func isFrameCapError(err error) bool {
	return err != nil && strings.Contains(err.Error(), "frame body cap")
}

// deviceUpserts builds the per-device INSERT/UPDATE statements for the
// distinct devices in a batch, assigning row ids for unseen devices.
// Caller runs them inside the upload transaction.
func (s *Store) deviceUpserts(batch []Event) []string {
	latest := make(map[string]int64) // device_key → newest ts in batch
	for _, ev := range batch {
		if ev.TS > latest[ev.DeviceID] {
			latest[ev.DeviceID] = ev.TS
		}
	}
	s.devMu.Lock()
	defer s.devMu.Unlock()

	var stmts []string
	for key, ts := range latest {
		if id, ok := s.devs[key]; ok {
			stmts = append(stmts, fmt.Sprintf(
				"UPDATE devices SET last_seen_at = %d WHERE id = %d", ts, id))
			continue
		}
		id := s.nextDevID.Add(1)
		s.devs[key] = id
		stmts = append(stmts, fmt.Sprintf(
			"INSERT INTO devices (id, device_key, label, last_seen_at) "+
				"VALUES (%d, %s, %s, %d)",
			id, quoteText(key), quoteText(key), ts))
	}
	return stmts
}

// RunTxn executes an arbitrary set of statements as one transaction
// through the configured write strategy. Exposed for the loadgen
// experiments (cmd/loadgen) so they can drive multi-statement
// transactions of varying shapes; the collector itself goes through the
// typed methods above.
func (s *Store) RunTxn(ctx context.Context, stmts ...string) error {
	return s.write(ctx, stmts...)
}

// write dispatches one or more statements through the configured write
// strategy. Concurrent wraps them in a retrying BEGIN CONCURRENT;
// Serialized funnels them through the single locked connection.
func (s *Store) write(ctx context.Context, stmts ...string) error {
	if s.opts.Mode == Serialized {
		return s.serializedWrite(ctx, stmts...)
	}
	return s.concurrentWrite(ctx, stmts...)
}

func (s *Store) serializedWrite(ctx context.Context, stmts ...string) error {
	s.serMu.Lock()
	defer s.serMu.Unlock()
	for _, q := range stmts {
		if _, err := s.serConn.ExecContext(ctx, q); err != nil {
			return fmt.Errorf("serialized write: %w", err)
		}
	}
	return nil
}

// concurrentWrite is the canonical SQLRite retry loop (see
// docs/concurrent-writes.md): pin a sibling connection, BEGIN
// CONCURRENT, apply the statements, COMMIT; on a retryable Busy the
// engine has already rolled the transaction back, so we just loop with
// a fresh BEGIN CONCURRENT.
func (s *Store) concurrentWrite(ctx context.Context, stmts ...string) error {
	c, err := s.db.Conn(ctx)
	if err != nil {
		return fmt.Errorf("acquire conn: %w", err)
	}
	defer c.Close()

	for attempt := 0; attempt < s.opts.MaxRetries; attempt++ {
		if _, err := c.ExecContext(ctx, "BEGIN CONCURRENT"); err != nil {
			return fmt.Errorf("begin concurrent: %w", err)
		}
		err := s.applyAndCommit(ctx, c, stmts)
		if err == nil {
			return nil
		}
		if sqlrite.IsRetryable(err) {
			// Engine already dropped the transaction on a Busy COMMIT.
			s.conflicts.Add(1)
			continue
		}
		// Hard error: a statement (not the commit) failed, leaving the
		// transaction open. Roll back before surfacing.
		_, _ = c.ExecContext(ctx, "ROLLBACK")
		return err
	}
	return fmt.Errorf("%w after %d attempts", ErrRetryExhausted, s.opts.MaxRetries)
}

func (s *Store) applyAndCommit(ctx context.Context, c *sql.Conn, stmts []string) error {
	for _, q := range stmts {
		if _, err := c.ExecContext(ctx, q); err != nil {
			return err
		}
	}
	_, err := c.ExecContext(ctx, "COMMIT")
	return err
}

// nullableText renders s as a SQL text literal, or NULL when empty.
func nullableText(s string) string {
	if s == "" {
		return "NULL"
	}
	return quoteText(s)
}

// ---------------------------------------------------------------------------
// Stats / gauges (cheap, atomic — no disk round-trip)

// Stats is a point-in-time snapshot for /stats and /healthz.
type Stats struct {
	Mode       string `json:"mode"`
	Indexed    bool   `json:"indexed"`
	Written    int64  `json:"events_written"`
	Uploaded   int64  `json:"events_uploaded"`
	Backlog    int64  `json:"backlog"`
	Conflicts  int64  `json:"commit_conflicts"`
	DeviceSeen int    `json:"devices_seen"`
}

// Stats returns the in-memory gauges.
func (s *Store) Stats() Stats {
	s.devMu.Lock()
	n := len(s.devs)
	s.devMu.Unlock()
	return Stats{
		Mode:       s.opts.Mode.String(),
		Indexed:    s.opts.Indexed,
		Written:    s.written.Load(),
		Uploaded:   s.uploaded.Load(),
		Backlog:    s.backlog.Load(),
		Conflicts:  s.conflicts.Load(),
		DeviceSeen: n,
	}
}

// Backlog returns the number of un-uploaded events.
func (s *Store) Backlog() int64 { return s.backlog.Load() }

// CountEvents runs a COUNT(*) against the database — the authoritative
// total, used by tests to cross-check the in-memory gauges.
func (s *Store) CountEvents(ctx context.Context) (int64, error) {
	var n int64
	row := s.db.QueryRowContext(ctx, "SELECT COUNT(*) FROM events")
	if err := row.Scan(&n); err != nil {
		return 0, err
	}
	return n, nil
}
