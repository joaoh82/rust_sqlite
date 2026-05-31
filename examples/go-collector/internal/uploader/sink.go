// Package uploader drains the durable buffer to a remote sink in the
// background, concurrently with the HTTP write path. The Sink interface
// is deliberately tiny and pluggable — the point of the example is the
// concurrent buffer, not any particular upstream.
package uploader

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"sync/atomic"
	"time"

	"github.com/joaoh82/rust_sqlite/examples/go-collector/internal/store"
)

// Sink is where batches of buffered events go once the network is up.
// Implementations must be safe for the single uploader goroutine.
type Sink interface {
	Name() string
	Upload(ctx context.Context, batch []store.Event) error
}

// LogSink is the zero-config default: it logs the batch size and
// succeeds. Useful for the demo and for running without any upstream.
type LogSink struct {
	Logger *log.Logger
}

func (s LogSink) Name() string { return "log" }

func (s LogSink) Upload(_ context.Context, batch []store.Event) error {
	if s.Logger != nil {
		s.Logger.Printf("uploaded %d events", len(batch))
	}
	return nil
}

// WebhookSink POSTs the batch as a JSON array to a URL. Any non-2xx
// response (or transport error) fails the cycle, leaving the events in
// the buffer to be retried next tick — exactly the unreliable-network
// story this example is about.
type WebhookSink struct {
	URL    string
	Client *http.Client
}

func (s WebhookSink) Name() string { return "webhook(" + s.URL + ")" }

func (s WebhookSink) Upload(ctx context.Context, batch []store.Event) error {
	body, err := json.Marshal(batch)
	if err != nil {
		return fmt.Errorf("marshal batch: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, s.URL, bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("build request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	client := s.Client
	if client == nil {
		client = &http.Client{Timeout: 10 * time.Second}
	}
	resp, err := client.Do(req)
	if err != nil {
		return fmt.Errorf("post batch: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("sink returned %s", resp.Status)
	}
	return nil
}

// FlakySink wraps another sink and fails every Nth call, to demonstrate
// backpressure and buffer growth during an upstream outage. Deterministic
// (counter-based) so tests can rely on it.
type FlakySink struct {
	Inner     Sink
	FailEvery int64 // fail when call count % FailEvery == 0; 0 disables
	calls     atomic.Int64
}

func (s *FlakySink) Name() string { return "flaky/" + s.Inner.Name() }

func (s *FlakySink) Upload(ctx context.Context, batch []store.Event) error {
	n := s.calls.Add(1)
	if s.FailEvery > 0 && n%s.FailEvery == 0 {
		return fmt.Errorf("flaky sink: simulated upstream failure on call %d", n)
	}
	return s.Inner.Upload(ctx, batch)
}
