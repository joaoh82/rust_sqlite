// Command collector is the SQLRite edge/IoT event collector (SQLR-43).
//
// It accepts telemetry over HTTP from many concurrent producers, writes
// each event into a local file-backed SQLRite database using Phase 11
// BEGIN CONCURRENT transactions, and drains the buffer to a pluggable
// sink from a background goroutine — all writing the same database
// concurrently.
//
//	go run ./cmd/collector -db events.sqlrite -addr :8080
//
// Prerequisite: build the engine's C library once from the repo root so
// cgo can link it:
//
//	cargo build --release -p sqlrite-ffi
//
// See the README for the full run + demo + throughput story.
package main

import (
	"context"
	"errors"
	"flag"
	"log"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/joaoh82/rust_sqlite/examples/go-collector/internal/server"
	"github.com/joaoh82/rust_sqlite/examples/go-collector/internal/store"
	"github.com/joaoh82/rust_sqlite/examples/go-collector/internal/uploader"
)

var (
	errWebhookURL  = errors.New("-webhook-url is required when -sink=webhook")
	errUnknownSink = errors.New("unknown -sink (want: log | webhook)")
)

func main() {
	var (
		dbPath     = flag.String("db", "events.sqlrite", "database file path")
		addr       = flag.String("addr", ":8080", "HTTP listen address")
		mode       = flag.String("mode", "concurrent", "write mode: concurrent | serialized")
		indexed    = flag.Bool("indexed", false, "create a secondary index on events(device_id)")
		maxConns   = flag.Int("max-conns", 8, "database/sql connection pool ceiling")
		maxBacklog = flag.Int64("max-backlog", 50000, "reject writes (503) once backlog reaches this; 0 = unlimited")
		interval   = flag.Duration("upload-interval", time.Second, "uploader drain interval")
		batch      = flag.Int("upload-batch", 200, "max events per uploader cycle")
		sinkKind   = flag.String("sink", "log", "upload sink: log | webhook")
		webhookURL = flag.String("webhook-url", "", "webhook sink URL (required when -sink=webhook)")
		flakyEvery = flag.Int64("flaky-every", 0, "wrap the sink to fail every Nth cycle (demo backpressure); 0 = off")
	)
	flag.Parse()

	logger := log.New(os.Stdout, "collector ", log.LstdFlags|log.Lmsgprefix)

	wm := store.Concurrent
	if *mode == "serialized" {
		wm = store.Serialized
	}

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	st, err := store.Open(ctx, store.Options{
		Path:         *dbPath,
		Mode:         wm,
		Indexed:      *indexed,
		MaxOpenConns: *maxConns,
	})
	if err != nil {
		logger.Fatalf("open store: %v", err)
	}
	defer st.Close()
	logger.Printf("store ready: path=%s mode=%s indexed=%v", *dbPath, wm, *indexed)

	sink, err := buildSink(*sinkKind, *webhookURL, *flakyEvery, logger)
	if err != nil {
		logger.Fatalf("configure sink: %v", err)
	}

	up := uploader.New(st, sink, uploader.Config{
		Interval:  *interval,
		BatchSize: *batch,
		Logger:    logger,
	})
	go up.Run(ctx)

	srv := server.New(st, up, server.Config{MaxBacklog: *maxBacklog, Logger: logger})
	httpServer := &http.Server{
		Addr:              *addr,
		Handler:           srv.Handler(),
		ReadHeaderTimeout: 5 * time.Second,
	}

	go func() {
		logger.Printf("listening on %s", *addr)
		if err := httpServer.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			logger.Fatalf("http server: %v", err)
		}
	}()

	<-ctx.Done()
	logger.Printf("shutting down…")

	// Stop accepting new requests, then let the uploader's final drain
	// (triggered by the cancelled ctx) run.
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := httpServer.Shutdown(shutdownCtx); err != nil {
		logger.Printf("http shutdown: %v", err)
	}
	// Give the uploader goroutine a moment to finish its final drain.
	time.Sleep(500 * time.Millisecond)
}

func buildSink(kind, url string, flakyEvery int64, logger *log.Logger) (uploader.Sink, error) {
	var base uploader.Sink
	switch kind {
	case "log", "":
		base = uploader.LogSink{Logger: logger}
	case "webhook":
		if url == "" {
			return nil, errWebhookURL
		}
		base = uploader.WebhookSink{URL: url, Client: &http.Client{Timeout: 10 * time.Second}}
	default:
		return nil, errUnknownSink
	}
	if flakyEvery > 0 {
		return &uploader.FlakySink{Inner: base, FailEvery: flakyEvery}, nil
	}
	return base, nil
}
