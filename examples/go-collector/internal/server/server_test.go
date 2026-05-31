package server

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"log"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"testing"

	"github.com/joaoh82/rust_sqlite/examples/go-collector/internal/store"
	"github.com/joaoh82/rust_sqlite/examples/go-collector/internal/uploader"
)

func newTestServer(t *testing.T, maxBacklog int64) (*httptest.Server, *store.Store) {
	t.Helper()
	path := filepath.Join(t.TempDir(), "srv.sqlrite")
	st, err := store.Open(context.Background(), store.Options{Path: path, Mode: store.Concurrent})
	if err != nil {
		t.Fatalf("open store: %v", err)
	}
	// Uploader is built but not Run() — backlog stays put so we can
	// exercise backpressure deterministically. A non-running uploader
	// reports healthy by default.
	up := uploader.New(st, uploader.LogSink{}, uploader.Config{Logger: log.New(io.Discard, "", 0)})
	srv := New(st, up, Config{MaxBacklog: maxBacklog, Logger: log.New(io.Discard, "", 0)})
	ts := httptest.NewServer(srv.Handler())
	t.Cleanup(func() { ts.Close(); st.Close() })
	return ts, st
}

func postEvent(t *testing.T, base string, body string) *http.Response {
	t.Helper()
	resp, err := http.Post(base+"/events", "application/json", bytes.NewBufferString(body))
	if err != nil {
		t.Fatalf("POST /events: %v", err)
	}
	return resp
}

func TestPostEventOK(t *testing.T) {
	ts, st := newTestServer(t, 0)
	resp := postEvent(t, ts.URL, `{"device_id":"sensor-1","kind":"telemetry","payload":{"temp":21}}`)
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status = %d, want 200", resp.StatusCode)
	}
	var out struct {
		Accepted int     `json:"accepted"`
		IDs      []int64 `json:"ids"`
	}
	json.NewDecoder(resp.Body).Decode(&out)
	if out.Accepted != 1 || len(out.IDs) != 1 {
		t.Fatalf("unexpected response: %+v", out)
	}
	if got, _ := st.CountEvents(context.Background()); got != 1 {
		t.Fatalf("CountEvents = %d, want 1", got)
	}
}

func TestPostEventArray(t *testing.T) {
	ts, _ := newTestServer(t, 0)
	resp := postEvent(t, ts.URL, `[{"device_id":"a","kind":"k"},{"device_id":"b","kind":"k"}]`)
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status = %d, want 200", resp.StatusCode)
	}
	var out struct {
		Accepted int `json:"accepted"`
	}
	json.NewDecoder(resp.Body).Decode(&out)
	if out.Accepted != 2 {
		t.Fatalf("accepted = %d, want 2", out.Accepted)
	}
}

func TestPostEventValidation(t *testing.T) {
	ts, _ := newTestServer(t, 0)
	cases := []struct {
		name string
		body string
		want int
	}{
		{"malformed json", `{not json`, http.StatusBadRequest},
		{"missing device_id", `{"kind":"k"}`, http.StatusBadRequest},
		{"missing kind", `{"device_id":"d"}`, http.StatusBadRequest},
		{"bad payload", `{"device_id":"d","kind":"k","payload":"not-an-object-but-string-is-valid-json"}`, http.StatusOK},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			resp := postEvent(t, ts.URL, c.body)
			defer resp.Body.Close()
			if resp.StatusCode != c.want {
				b, _ := io.ReadAll(resp.Body)
				t.Fatalf("status = %d, want %d (body: %s)", resp.StatusCode, c.want, b)
			}
		})
	}
}

func TestBackpressure503(t *testing.T) {
	ts, _ := newTestServer(t, 3) // buffer ceiling of 3

	for i := 0; i < 3; i++ {
		resp := postEvent(t, ts.URL, `{"device_id":"d","kind":"k"}`)
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("fill %d: status = %d, want 200", i, resp.StatusCode)
		}
		resp.Body.Close()
	}
	// Buffer is full → 503.
	resp := postEvent(t, ts.URL, `{"device_id":"d","kind":"k"}`)
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusServiceUnavailable {
		t.Fatalf("over-limit status = %d, want 503", resp.StatusCode)
	}
}

func TestHealthzAndStats(t *testing.T) {
	ts, _ := newTestServer(t, 2)

	resp, err := http.Get(ts.URL + "/healthz")
	if err != nil {
		t.Fatalf("GET /healthz: %v", err)
	}
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("healthz status = %d, want 200", resp.StatusCode)
	}
	resp.Body.Close()

	// Fill past the ceiling, then healthz should flip to 503.
	for i := 0; i < 2; i++ {
		postEvent(t, ts.URL, `{"device_id":"d","kind":"k"}`).Body.Close()
	}
	resp, _ = http.Get(ts.URL + "/healthz")
	if resp.StatusCode != http.StatusServiceUnavailable {
		t.Fatalf("healthz after fill = %d, want 503", resp.StatusCode)
	}
	resp.Body.Close()

	// /stats always 200 and reports the store snapshot.
	resp, _ = http.Get(ts.URL + "/stats")
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("stats status = %d, want 200", resp.StatusCode)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)
	if !bytes.Contains(body, []byte(`"mode":"concurrent"`)) {
		t.Fatalf("stats body missing mode: %s", body)
	}
}
