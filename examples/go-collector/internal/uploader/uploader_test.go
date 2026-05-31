package uploader

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"log"
	"path/filepath"
	"sync"
	"testing"

	"github.com/joaoh82/rust_sqlite/examples/go-collector/internal/store"
)

type fakeSink struct {
	mu   sync.Mutex
	got  []store.Event
	fail bool
}

func (f *fakeSink) Name() string { return "fake" }

func (f *fakeSink) Upload(_ context.Context, batch []store.Event) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.fail {
		return errors.New("simulated failure")
	}
	f.got = append(f.got, batch...)
	return nil
}

func (f *fakeSink) count() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.got)
}

func newStore(t *testing.T) *store.Store {
	t.Helper()
	path := filepath.Join(t.TempDir(), "up.sqlrite")
	st, err := store.Open(context.Background(), store.Options{Path: path, Mode: store.Concurrent})
	if err != nil {
		t.Fatalf("open store: %v", err)
	}
	t.Cleanup(func() { st.Close() })
	return st
}

func quietLogger() *log.Logger { return log.New(io.Discard, "", 0) }

func seedEvents(t *testing.T, st *store.Store, n int) {
	t.Helper()
	for i := 0; i < n; i++ {
		_, err := st.InsertEvent(context.Background(), store.Event{
			DeviceID: "sensor-1", Kind: "telemetry",
			Payload: json.RawMessage(`{"v":1}`), TS: int64(i + 1),
		})
		if err != nil {
			t.Fatalf("seed insert: %v", err)
		}
	}
}

func TestUploaderDrainsBuffer(t *testing.T) {
	st := newStore(t)
	seedEvents(t, st, 5)
	sink := &fakeSink{}
	u := New(st, sink, Config{Logger: quietLogger()})

	u.cycle(context.Background())

	if sink.count() != 5 {
		t.Fatalf("sink received %d events, want 5", sink.count())
	}
	if got := st.Backlog(); got != 0 {
		t.Fatalf("backlog after drain = %d, want 0", got)
	}
	if !u.Healthy() {
		t.Fatal("uploader should be healthy after a clean drain")
	}
}

func TestUploaderFailureLeavesBuffer(t *testing.T) {
	st := newStore(t)
	seedEvents(t, st, 3)
	sink := &fakeSink{fail: true}
	u := New(st, sink, Config{Logger: quietLogger()})

	u.cycle(context.Background())

	if got := st.Backlog(); got != 3 {
		t.Fatalf("backlog after failed drain = %d, want 3 (events must stay buffered)", got)
	}
	if u.Healthy() {
		t.Fatal("uploader should be unhealthy after an upload failure")
	}
	if h := u.Health(); h.Failures != 1 {
		t.Fatalf("failures = %d, want 1", h.Failures)
	}
	// A subsequent successful cycle clears the backlog and recovers.
	sink.fail = false
	u.cycle(context.Background())
	if got := st.Backlog(); got != 0 {
		t.Fatalf("backlog after recovery = %d, want 0", got)
	}
	if !u.Healthy() {
		t.Fatal("uploader should recover to healthy after a clean drain")
	}
}

func TestFlakySink(t *testing.T) {
	inner := &fakeSink{}
	flaky := &FlakySink{Inner: inner, FailEvery: 2}
	ctx := context.Background()
	if err := flaky.Upload(ctx, nil); err != nil {
		t.Fatalf("call 1 should succeed: %v", err)
	}
	if err := flaky.Upload(ctx, nil); err == nil {
		t.Fatal("call 2 should fail (FailEvery=2)")
	}
	if err := flaky.Upload(ctx, nil); err != nil {
		t.Fatalf("call 3 should succeed: %v", err)
	}
}
