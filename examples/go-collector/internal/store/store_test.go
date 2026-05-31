package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"sync"
	"testing"

	sqlrite "github.com/joaoh82/rust_sqlite/sdk/go"
)

func tempStore(t *testing.T, mode WriteMode, indexed bool) *Store {
	t.Helper()
	path := filepath.Join(t.TempDir(), "test.sqlrite")
	st, err := Open(context.Background(), Options{Path: path, Mode: mode, Indexed: indexed, MaxOpenConns: 8})
	if err != nil {
		t.Fatalf("open store: %v", err)
	}
	t.Cleanup(func() { st.Close() })
	return st
}

func ev(device, kind string, ts int64) Event {
	return Event{DeviceID: device, Kind: kind, Payload: json.RawMessage(`{"v":1,"note":"it's ok"}`), TS: ts}
}

func TestInsertAndFetchPending(t *testing.T) {
	st := tempStore(t, Concurrent, false)
	ctx := context.Background()

	for i := 0; i < 5; i++ {
		if _, err := st.InsertEvent(ctx, ev("sensor-1", "telemetry", int64(i))); err != nil {
			t.Fatalf("insert %d: %v", i, err)
		}
	}
	if got := st.Backlog(); got != 5 {
		t.Fatalf("backlog = %d, want 5", got)
	}
	pending, err := st.FetchPending(ctx, 10)
	if err != nil {
		t.Fatalf("fetch pending: %v", err)
	}
	if len(pending) != 5 {
		t.Fatalf("pending = %d, want 5", len(pending))
	}
	// Oldest-first, ids assigned 1..5, payload round-trips.
	for i, p := range pending {
		if p.ID != int64(i+1) {
			t.Errorf("pending[%d].ID = %d, want %d", i, p.ID, i+1)
		}
		if !json.Valid(p.Payload) {
			t.Errorf("pending[%d] payload not valid JSON: %s", i, p.Payload)
		}
	}
}

func TestQuoteEscaping(t *testing.T) {
	st := tempStore(t, Concurrent, false)
	ctx := context.Background()
	// device id + a payload string both carrying single quotes must
	// survive the inline-SQL round trip intact.
	in := Event{DeviceID: "o'brien-rig", Kind: "te'st", Payload: json.RawMessage(`{"msg":"don't panic"}`), TS: 1}
	id, err := st.InsertEvent(ctx, in)
	if err != nil {
		t.Fatalf("insert with quotes: %v", err)
	}
	pending, _ := st.FetchPending(ctx, 10)
	if len(pending) != 1 || pending[0].ID != id {
		t.Fatalf("expected 1 pending event with id %d, got %+v", id, pending)
	}
	if pending[0].DeviceID != "o'brien-rig" {
		t.Errorf("device id mangled: %q", pending[0].DeviceID)
	}
	var got map[string]string
	if err := json.Unmarshal(pending[0].Payload, &got); err != nil {
		t.Fatalf("payload not round-tripped: %v (%s)", err, pending[0].Payload)
	}
	if got["msg"] != "don't panic" {
		t.Errorf("payload msg = %q, want %q", got["msg"], "don't panic")
	}
}

func TestBadPayloadRejected(t *testing.T) {
	st := tempStore(t, Concurrent, false)
	bad := Event{DeviceID: "d", Kind: "k", Payload: json.RawMessage(`{not json`), TS: 1}
	_, err := st.InsertEvent(context.Background(), bad)
	if err == nil {
		t.Fatal("expected error for malformed JSON payload")
	}
	if !errors.Is(err, ErrBadPayload) {
		t.Fatalf("want ErrBadPayload, got %v", err)
	}
}

// TestOversizedPayloadRejected guards the hot path against a payload big
// enough to overflow the MVCC 4 KiB commit-record cap. Without the
// ingest-side size check this would fail opaquely at COMMIT; we want a
// clean ErrBadPayload at insert time instead.
func TestOversizedPayloadRejected(t *testing.T) {
	st := tempStore(t, Concurrent, false)
	big := make([]byte, 5000)
	for i := range big {
		big[i] = 'a'
	}
	payload, _ := json.Marshal(map[string]string{"blob": string(big)})
	_, err := st.InsertEvent(context.Background(), Event{DeviceID: "d", Kind: "k", Payload: payload, TS: 1})
	if err == nil {
		t.Fatal("expected error for oversized payload")
	}
	if !errors.Is(err, ErrBadPayload) {
		t.Fatalf("want ErrBadPayload, got %v", err)
	}
	// A payload just under the cap must still be accepted.
	ok := make([]byte, 2000)
	for i := range ok {
		ok[i] = 'b'
	}
	okPayload, _ := json.Marshal(map[string]string{"blob": string(ok)})
	if _, err := st.InsertEvent(context.Background(), Event{DeviceID: "d", Kind: "k", Payload: okPayload, TS: 2}); err != nil {
		t.Fatalf("under-cap payload should be accepted: %v", err)
	}
}

func TestCommitUploadMarksAndAudits(t *testing.T) {
	st := tempStore(t, Concurrent, false)
	ctx := context.Background()
	for i := 0; i < 3; i++ {
		st.InsertEvent(ctx, ev("sensor-x", "telemetry", int64(i+1)))
	}
	pending, _ := st.FetchPending(ctx, 10)
	ids := []int64{pending[0].ID, pending[1].ID, pending[2].ID}

	run := UploadRun{StartedAt: 1, FinishedAt: 2, EventCount: 3, Status: "success"}
	if err := st.CommitUpload(ctx, run, ids, pending); err != nil {
		t.Fatalf("commit upload: %v", err)
	}
	if got := st.Backlog(); got != 0 {
		t.Fatalf("backlog after upload = %d, want 0", got)
	}
	left, _ := st.FetchPending(ctx, 10)
	if len(left) != 0 {
		t.Fatalf("still %d pending after upload", len(left))
	}
	// Audit row + device upsert landed.
	if n := scalar(t, st, "SELECT COUNT(*) FROM upload_runs"); n != 1 {
		t.Errorf("upload_runs count = %d, want 1", n)
	}
	if n := scalar(t, st, "SELECT COUNT(*) FROM devices"); n != 1 {
		t.Errorf("devices count = %d, want 1", n)
	}
}

func TestSeedReopenContinuesIDs(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "reopen.sqlrite")

	st, err := Open(ctx, Options{Path: path, Mode: Concurrent})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	for i := 0; i < 4; i++ {
		st.InsertEvent(ctx, ev("d", "k", int64(i+1)))
	}
	st.Close()

	st2, err := Open(ctx, Options{Path: path, Mode: Concurrent})
	if err != nil {
		t.Fatalf("reopen: %v", err)
	}
	defer st2.Close()
	if got := st2.Backlog(); got != 4 {
		t.Fatalf("reopened backlog = %d, want 4", got)
	}
	id, err := st2.InsertEvent(ctx, ev("d", "k", 99))
	if err != nil {
		t.Fatalf("insert after reopen: %v", err)
	}
	if id != 5 {
		t.Fatalf("next id after reopen = %d, want 5 (no reuse)", id)
	}
}

// TestLargeCheckpointUnderMVCCCap drives a checkpoint far larger than
// the engine's 4 KiB MVCC commit-batch cap. A naive single-transaction
// mark would fail with "encoded batch exceeds 4096-byte frame body cap";
// the chunked CommitUpload must drain all of it.
func TestLargeCheckpointUnderMVCCCap(t *testing.T) {
	st := tempStore(t, Concurrent, false)
	ctx := context.Background()

	const n = 300 // well past the ~100-row single-commit ceiling
	for i := 0; i < n; i++ {
		// Wide-ish payload so each row's MVCC record is realistic.
		ev := Event{
			DeviceID: fmt.Sprintf("sensor-%03d", i%50),
			Kind:     "telemetry",
			Payload:  json.RawMessage(`{"temp_c":21,"humidity":47,"seq":12345,"note":"all systems nominal here"}`),
			TS:       int64(i + 1),
		}
		if _, err := st.InsertEvent(ctx, ev); err != nil {
			t.Fatalf("seed insert %d: %v", i, err)
		}
	}
	pending, err := st.FetchPending(ctx, n)
	if err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if len(pending) != n {
		t.Fatalf("pending = %d, want %d", len(pending), n)
	}
	ids := make([]int64, len(pending))
	for i, p := range pending {
		ids[i] = p.ID
	}

	run := UploadRun{StartedAt: 1, FinishedAt: 2, EventCount: n, Status: "success"}
	if err := st.CommitUpload(ctx, run, ids, pending); err != nil {
		t.Fatalf("commit large checkpoint: %v", err)
	}
	if got := st.Backlog(); got != 0 {
		t.Fatalf("backlog after large checkpoint = %d, want 0 (chunking failed under the MVCC cap)", got)
	}
	left, _ := st.FetchPending(ctx, n)
	if len(left) != 0 {
		t.Fatalf("%d events still pending after large checkpoint", len(left))
	}
}

// TestConcurrentWritersNoDrops is the correctness heart of the example:
// many goroutines writing through BEGIN CONCURRENT must all land.
func TestConcurrentWritersNoDrops(t *testing.T) {
	st := tempStore(t, Concurrent, false)
	ctx := context.Background()

	const writers, each = 8, 50
	var wg sync.WaitGroup
	errs := make(chan error, writers*each)
	for w := 0; w < writers; w++ {
		wg.Add(1)
		go func(w int) {
			defer wg.Done()
			for i := 0; i < each; i++ {
				if _, err := st.InsertEvent(ctx, ev(fmt.Sprintf("sensor-%d", w), "telemetry", int64(i))); err != nil {
					errs <- err
				}
			}
		}(w)
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		t.Fatalf("concurrent insert error: %v", err)
	}

	want := int64(writers * each)
	if got, _ := st.CountEvents(ctx); got != want {
		t.Fatalf("CountEvents = %d, want %d (events dropped!)", got, want)
	}
	if got := st.Backlog(); got != want {
		t.Fatalf("backlog = %d, want %d", got, want)
	}
}

// TestForcedConflictRetry deterministically drives a write-write
// conflict and proves the COMMIT surfaces a retryable Busy, then a
// fresh BEGIN CONCURRENT succeeds — the retry loop the store relies on.
func TestForcedConflictRetry(t *testing.T) {
	st := tempStore(t, Concurrent, false)
	ctx := context.Background()
	st.InsertEvent(ctx, ev("d", "orig", 1)) // id = 1

	c1, err := st.db.Conn(ctx)
	if err != nil {
		t.Fatalf("conn1: %v", err)
	}
	defer c1.Close()
	c2, err := st.db.Conn(ctx)
	if err != nil {
		t.Fatalf("conn2: %v", err)
	}
	defer c2.Close()

	mustExec(t, c1, ctx, "BEGIN CONCURRENT")
	mustExec(t, c2, ctx, "BEGIN CONCURRENT")
	mustExec(t, c1, ctx, "UPDATE events SET kind = 'a' WHERE id = 1")
	mustExec(t, c2, ctx, "UPDATE events SET kind = 'b' WHERE id = 1")
	mustExec(t, c1, ctx, "COMMIT") // winner

	_, err = c2.ExecContext(ctx, "COMMIT") // loser → Busy
	if err == nil {
		t.Fatal("expected Busy on conflicting COMMIT, got nil")
	}
	if !sqlrite.IsRetryable(err) {
		t.Fatalf("expected retryable error, got %v", err)
	}

	// Retry on a fresh BEGIN CONCURRENT lands.
	mustExec(t, c2, ctx, "BEGIN CONCURRENT")
	mustExec(t, c2, ctx, "UPDATE events SET kind = 'b' WHERE id = 1")
	mustExec(t, c2, ctx, "COMMIT")

	if got := scalarText(t, st, "SELECT kind FROM events WHERE id = 1"); got != "b" {
		t.Fatalf("final kind = %q, want %q", got, "b")
	}
}

func TestSerializedModeWrites(t *testing.T) {
	st := tempStore(t, Serialized, false)
	ctx := context.Background()
	for i := 0; i < 10; i++ {
		if _, err := st.InsertEvent(ctx, ev("d", "k", int64(i))); err != nil {
			t.Fatalf("serialized insert: %v", err)
		}
	}
	if got, _ := st.CountEvents(ctx); got != 10 {
		t.Fatalf("serialized CountEvents = %d, want 10", got)
	}
}

// --- helpers ---

func scalar(t *testing.T, st *Store, q string) int64 {
	t.Helper()
	var n int64
	if err := st.db.QueryRowContext(context.Background(), q).Scan(&n); err != nil {
		t.Fatalf("scalar %q: %v", q, err)
	}
	return n
}

func scalarText(t *testing.T, st *Store, q string) string {
	t.Helper()
	var s string
	if err := st.db.QueryRowContext(context.Background(), q).Scan(&s); err != nil {
		t.Fatalf("scalarText %q: %v", q, err)
	}
	return s
}

func mustExec(t *testing.T, c *sql.Conn, ctx context.Context, q string) {
	t.Helper()
	if _, err := c.ExecContext(ctx, q); err != nil {
		t.Fatalf("exec %q: %v", q, err)
	}
}
