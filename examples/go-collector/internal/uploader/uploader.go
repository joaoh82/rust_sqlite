package uploader

import (
	"context"
	"log"
	"sync"
	"sync/atomic"
	"time"

	"github.com/joaoh82/rust_sqlite/examples/go-collector/internal/store"
)

// Config tunes the uploader loop.
type Config struct {
	Interval  time.Duration // how often to drain (default 1s)
	BatchSize int           // max events per cycle (default 200)
	Logger    *log.Logger
}

// Uploader is the background goroutine that drains the buffer to a Sink.
// It writes upload checkpoints (upload_runs + uploaded_at marks) through
// the same store the HTTP path writes events into — that concurrency is
// the whole point of the example.
type Uploader struct {
	store *store.Store
	sink  Sink
	cfg   Config
	log   *log.Logger

	mu        sync.Mutex
	healthy   bool
	lastErr   string
	lastRunAt time.Time

	runs     atomic.Int64
	failures atomic.Int64
}

// New builds an Uploader. It starts out healthy (no failures yet).
func New(s *store.Store, sink Sink, cfg Config) *Uploader {
	if cfg.Interval <= 0 {
		cfg.Interval = time.Second
	}
	if cfg.BatchSize <= 0 {
		cfg.BatchSize = 200
	}
	lg := cfg.Logger
	if lg == nil {
		lg = log.Default()
	}
	return &Uploader{store: s, sink: sink, cfg: cfg, log: lg, healthy: true}
}

// Run drains on a ticker until ctx is cancelled. Blocks; run it in a
// goroutine. A final drain attempt runs on shutdown so a clean stop
// doesn't strand a buffered tail.
func (u *Uploader) Run(ctx context.Context) {
	ticker := time.NewTicker(u.cfg.Interval)
	defer ticker.Stop()
	u.log.Printf("uploader started: sink=%s interval=%s batch=%d",
		u.sink.Name(), u.cfg.Interval, u.cfg.BatchSize)
	for {
		select {
		case <-ctx.Done():
			// Best-effort final drain with a fresh, short-lived context.
			drainCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			u.cycle(drainCtx)
			cancel()
			u.log.Printf("uploader stopped after %d runs (%d failures)",
				u.runs.Load(), u.failures.Load())
			return
		case <-ticker.C:
			u.cycle(ctx)
		}
	}
}

// cycle runs one drain: fetch a batch, ship it, record the checkpoint.
// A successful ship marks the batch uploaded; a failed ship records the
// failure in upload_runs and leaves the events buffered for next time.
func (u *Uploader) cycle(ctx context.Context) {
	startedAt := time.Now().UnixMilli()
	batch, err := u.store.FetchPending(ctx, u.cfg.BatchSize)
	if err != nil {
		u.markUnhealthy("fetch pending: " + err.Error())
		return
	}
	if len(batch) == 0 {
		u.markHealthy()
		return
	}

	ids := make([]int64, len(batch))
	for i, ev := range batch {
		ids[i] = ev.ID
	}

	u.runs.Add(1)
	if uploadErr := u.sink.Upload(ctx, batch); uploadErr != nil {
		u.failures.Add(1)
		run := store.UploadRun{
			StartedAt:  startedAt,
			FinishedAt: time.Now().UnixMilli(),
			EventCount: len(batch),
			Status:     "error",
			Error:      uploadErr.Error(),
		}
		// Record the failed run; do NOT mark events uploaded (nil ids).
		if err := u.store.CommitUpload(ctx, run, nil, batch); err != nil {
			u.log.Printf("uploader: failed to record errored run: %v", err)
		}
		u.markUnhealthy("upload: " + uploadErr.Error())
		return
	}

	run := store.UploadRun{
		StartedAt:  startedAt,
		FinishedAt: time.Now().UnixMilli(),
		EventCount: len(batch),
		Status:     "success",
	}
	if err := u.store.CommitUpload(ctx, run, ids, batch); err != nil {
		// The events shipped but we couldn't checkpoint — they'll be
		// re-shipped next cycle. At-least-once delivery; document it.
		u.markUnhealthy("commit checkpoint: " + err.Error())
		return
	}
	u.markHealthy()
}

func (u *Uploader) markHealthy() {
	u.mu.Lock()
	u.healthy = true
	u.lastErr = ""
	u.lastRunAt = time.Now()
	u.mu.Unlock()
}

func (u *Uploader) markUnhealthy(msg string) {
	u.mu.Lock()
	u.healthy = false
	u.lastErr = msg
	u.lastRunAt = time.Now()
	u.mu.Unlock()
	u.log.Printf("uploader unhealthy: %s", msg)
}

// Health is a snapshot of the uploader's state for /healthz and /stats.
type Health struct {
	Healthy   bool   `json:"healthy"`
	LastError string `json:"last_error,omitempty"`
	Runs      int64  `json:"runs"`
	Failures  int64  `json:"failures"`
	SinkName  string `json:"sink"`
}

// Health returns the current uploader health snapshot.
func (u *Uploader) Health() Health {
	u.mu.Lock()
	defer u.mu.Unlock()
	return Health{
		Healthy:   u.healthy,
		LastError: u.lastErr,
		Runs:      u.runs.Load(),
		Failures:  u.failures.Load(),
		SinkName:  u.sink.Name(),
	}
}

// Healthy reports whether the last cycle succeeded.
func (u *Uploader) Healthy() bool {
	u.mu.Lock()
	defer u.mu.Unlock()
	return u.healthy
}
