// Command loadgen drives the collector two ways:
//
//   - HTTP correctness load (-target): fire N concurrent producers at a
//     running collector's POST /events for a duration, assert every
//     write was accepted (no drops), and report achieved req/s. This is
//     the "concurrent-writes correctness" demo the README leans on.
//
//   - In-process throughput matrix (-bench): open the store directly
//     and measure sustained events/sec for each {write-mode} × {indexed}
//     combination. This isolates the database write path from HTTP
//     overhead and produces the measured numbers in the README. It is
//     the honest answer to "what did concurrent writes buy us, and what
//     does a secondary index cost?"
//
// Examples:
//
//	go run ./cmd/loadgen -target http://localhost:8080 -workers 64 -duration 30s
//	go run ./cmd/loadgen -bench -workers 32 -duration 10s
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"sync"
	"sync/atomic"
	"time"

	"github.com/joaoh82/rust_sqlite/examples/go-collector/internal/store"
)

func main() {
	var (
		target     = flag.String("target", "", "collector base URL for HTTP load (e.g. http://localhost:8080)")
		bench      = flag.Bool("bench", false, "run the in-process throughput matrix instead of HTTP load")
		contention = flag.Bool("contention", false, "measure insert latency under a concurrent checkpoint writer (serialized vs concurrent)")
		disjoint   = flag.Bool("disjoint", false, "MVCC best case: writers update disjoint row ranges in batched transactions (fair single-writer BEGIN/COMMIT baseline)")
		workers    = flag.Int("workers", 32, "number of concurrent producers")
		duration   = flag.Duration("duration", 10*time.Second, "how long to drive load")
		devices    = flag.Int("devices", 100, "number of distinct simulated devices")
	)
	flag.Parse()

	switch {
	case *bench:
		runBench(*workers, *duration, *devices)
	case *contention:
		runContention(*workers, *duration, *devices)
	case *disjoint:
		runDisjoint(*workers, *duration)
	case *target != "":
		os.Exit(runHTTP(*target, *workers, *duration, *devices))
	default:
		fmt.Fprintln(os.Stderr, "specify -target <url> (HTTP load), -bench (throughput matrix), or -contention (latency under a concurrent writer)")
		flag.Usage()
		os.Exit(2)
	}
}

// sampleEvent builds a realistic-ish telemetry event for device n.
func sampleEvent(deviceN, seq int) store.Event {
	payload, _ := json.Marshal(map[string]any{
		"temp_c":   20 + (seq % 15),
		"humidity": 40 + (seq % 50),
		"seq":      seq,
		"note":     "it's fine", // exercises the quote-escaping path
	})
	kind := "telemetry"
	if seq%17 == 0 {
		kind = "error"
	}
	return store.Event{
		DeviceID: fmt.Sprintf("sensor-%03d", deviceN),
		Kind:     kind,
		Payload:  payload,
		TS:       time.Now().UnixMilli(),
	}
}

// ---------------------------------------------------------------------------
// HTTP correctness load

func runHTTP(target string, workers int, duration time.Duration, devices int) int {
	url := target + "/events"
	client := &http.Client{Timeout: 10 * time.Second}

	var ok, failed, busy503 atomic.Int64
	ctx, cancel := context.WithTimeout(context.Background(), duration)
	defer cancel()

	var wg sync.WaitGroup
	start := time.Now()
	for w := 0; w < workers; w++ {
		wg.Add(1)
		go func(worker int) {
			defer wg.Done()
			seq := 0
			for ctx.Err() == nil {
				ev := sampleEvent((worker+seq)%devices, seq)
				seq++
				body, _ := json.Marshal(ev)
				req, _ := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(body))
				req.Header.Set("Content-Type", "application/json")
				resp, err := client.Do(req)
				if err != nil {
					if ctx.Err() != nil {
						return // shutdown, not a drop
					}
					failed.Add(1)
					continue
				}
				io.Copy(io.Discard, resp.Body)
				resp.Body.Close()
				switch {
				case resp.StatusCode == http.StatusOK:
					ok.Add(1)
				case resp.StatusCode == http.StatusServiceUnavailable:
					busy503.Add(1) // backpressure, not a drop — producer should retry
				default:
					failed.Add(1)
				}
			}
		}(w)
	}
	wg.Wait()
	elapsed := time.Since(start)

	accepted := ok.Load()
	fmt.Printf("\nHTTP load against %s\n", url)
	fmt.Printf("  workers:        %d\n", workers)
	fmt.Printf("  duration:       %s\n", elapsed.Round(time.Millisecond))
	fmt.Printf("  accepted (200): %d  (%.0f events/sec)\n", accepted, float64(accepted)/elapsed.Seconds())
	fmt.Printf("  backpressure (503): %d\n", busy503.Load())
	fmt.Printf("  failed:         %d\n", failed.Load())

	// Cross-check against the server's own counters.
	if stats := fetchStats(client, target); stats != "" {
		fmt.Printf("  server /stats:  %s\n", stats)
	}

	if failed.Load() > 0 {
		fmt.Println("\nFAIL: some writes were dropped (non-200, non-503).")
		return 1
	}
	fmt.Println("\nOK: no writes dropped.")
	return 0
}

func fetchStats(client *http.Client, target string) string {
	resp, err := client.Get(target + "/stats")
	if err != nil {
		return ""
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(resp.Body)
	return string(bytes.TrimSpace(b))
}

// ---------------------------------------------------------------------------
// In-process throughput matrix

type benchResult struct {
	mode    store.WriteMode
	indexed bool
	events  int64
	elapsed time.Duration
	stats   store.Stats
}

func runBench(workers int, duration time.Duration, devices int) {
	combos := []struct {
		mode    store.WriteMode
		indexed bool
	}{
		{store.Serialized, false},
		{store.Concurrent, false},
		{store.Concurrent, true},
	}

	tmp, err := os.MkdirTemp("", "go-collector-bench")
	if err != nil {
		log.Fatalf("temp dir: %v", err)
	}
	defer os.RemoveAll(tmp)

	fmt.Printf("\nThroughput matrix (workers=%d, duration=%s, devices=%d)\n", workers, duration, devices)
	fmt.Println("Each cell: a fresh DB, N goroutines inserting events as fast as they can.")
	fmt.Println()
	fmt.Printf("%-22s %-8s %14s %12s\n", "write mode", "indexed", "events/sec", "conflicts")
	fmt.Println("-------------------------------------------------------------------")

	var results []benchResult
	for i, c := range combos {
		dbPath := filepath.Join(tmp, fmt.Sprintf("bench-%d.sqlrite", i))
		res := benchOne(dbPath, c.mode, c.indexed, workers, duration, devices)
		results = append(results, res)
		fmt.Printf("%-22s %-8v %14.0f %12d\n",
			res.mode.String(), res.indexed,
			float64(res.events)/res.elapsed.Seconds(),
			res.stats.Conflicts)
	}

	// Headline comparison: concurrent vs serialized (both index-less).
	var ser, con float64
	for _, r := range results {
		if r.mode == store.Serialized && !r.indexed {
			ser = float64(r.events) / r.elapsed.Seconds()
		}
		if r.mode == store.Concurrent && !r.indexed {
			con = float64(r.events) / r.elapsed.Seconds()
		}
	}
	if ser > 0 {
		fmt.Printf("\nconcurrent / serialized speedup: %.2fx\n", con/ser)
	}
}

func benchOne(dbPath string, mode store.WriteMode, indexed bool, workers int, duration time.Duration, devices int) benchResult {
	ctx := context.Background()
	st, err := store.Open(ctx, store.Options{
		Path:         dbPath,
		Mode:         mode,
		Indexed:      indexed,
		MaxOpenConns: workers,
	})
	if err != nil {
		log.Fatalf("open store (%s, indexed=%v): %v", mode, indexed, err)
	}
	defer st.Close()

	runCtx, cancel := context.WithTimeout(ctx, duration)
	defer cancel()

	var count atomic.Int64
	var wg sync.WaitGroup
	start := time.Now()
	for w := 0; w < workers; w++ {
		wg.Add(1)
		go func(worker int) {
			defer wg.Done()
			seq := 0
			for runCtx.Err() == nil {
				ev := sampleEvent((worker+seq)%devices, seq)
				seq++
				if _, err := st.InsertEvent(runCtx, ev); err != nil {
					if runCtx.Err() != nil {
						return
					}
					log.Printf("insert: %v", err)
					return
				}
				count.Add(1)
			}
		}(w)
	}
	wg.Wait()
	elapsed := time.Since(start)

	return benchResult{
		mode:    mode,
		indexed: indexed,
		events:  count.Load(),
		elapsed: elapsed,
		stats:   st.Stats(),
	}
}

// ---------------------------------------------------------------------------
// Latency under contention — the honest headline measurement.
//
// This is where concurrent writes actually pay off for the collector.
// A "checkpointer" goroutine repeatedly runs a big multi-statement
// write transaction (the shape the uploader uses: UPDATE a wide row
// range + an audit INSERT), while inserter goroutines push single
// events and time each insert.
//
//   - Serialized mode holds one lock for the checkpointer's ENTIRE
//     transaction, so every insert that arrives mid-checkpoint stalls
//     until it commits → fat tail latency (head-of-line blocking).
//   - Concurrent mode gives the checkpointer its own BEGIN CONCURRENT
//     transaction; insert statements interleave with the checkpointer's
//     statements instead of waiting for the whole transaction → a much
//     tighter tail.
//
// Raw throughput is ~the same either way (see -bench); the difference
// this measures is insert *tail latency* while a background writer is
// busy — the property the example is really about.
func runContention(workers int, duration time.Duration, devices int) {
	const seed = 1000      // pre-inserted rows the checkpointer churns
	const checkpoint = 500 // rows touched per checkpoint transaction

	fmt.Printf("\nInsert latency under a concurrent checkpoint writer "+
		"(workers=%d, duration=%s)\n", workers, duration)
	fmt.Println("A background writer runs a wide multi-statement transaction in a loop")
	fmt.Println("while producers insert events. Lower tail latency is better.")
	fmt.Println()
	fmt.Printf("%-14s %12s %12s %12s %12s\n", "write mode", "p50", "p90", "p99", "max")
	fmt.Println("----------------------------------------------------------------------")

	for _, mode := range []store.WriteMode{store.Serialized, store.Concurrent} {
		lat := contentionOne(mode, workers, duration, devices, seed, checkpoint)
		fmt.Printf("%-14s %12s %12s %12s %12s\n",
			mode.String(),
			pctl(lat, 0.50).Round(time.Microsecond),
			pctl(lat, 0.90).Round(time.Microsecond),
			pctl(lat, 0.99).Round(time.Microsecond),
			pctl(lat, 1.0).Round(time.Microsecond),
		)
	}
}

func contentionOne(mode store.WriteMode, workers int, duration time.Duration, devices, seed, checkpoint int) []time.Duration {
	ctx := context.Background()
	tmp, _ := os.MkdirTemp("", "contention")
	defer os.RemoveAll(tmp)

	st, err := store.Open(ctx, store.Options{
		Path:         filepath.Join(tmp, "c.sqlrite"),
		Mode:         mode,
		MaxOpenConns: workers + 2,
	})
	if err != nil {
		log.Fatalf("open: %v", err)
	}
	defer st.Close()

	// Seed rows for the checkpointer to churn.
	for i := 0; i < seed; i++ {
		st.InsertEvent(ctx, sampleEvent(i%devices, i))
	}
	pending, _ := st.FetchPending(ctx, checkpoint)
	ids := make([]int64, len(pending))
	for i, p := range pending {
		ids[i] = p.ID
	}

	runCtx, cancel := context.WithTimeout(ctx, duration)
	defer cancel()

	// Checkpointer: repeated wide multi-statement transactions.
	var cpWG sync.WaitGroup
	cpWG.Add(1)
	go func() {
		defer cpWG.Done()
		for runCtx.Err() == nil {
			run := store.UploadRun{StartedAt: 1, FinishedAt: 2, EventCount: len(ids), Status: "success"}
			// nil batch → no device upserts; just the wide UPDATE + audit row.
			_ = st.CommitUpload(runCtx, run, ids, nil)
		}
	}()

	// Inserters: time each insert.
	var mu sync.Mutex
	var samples []time.Duration
	var insWG sync.WaitGroup
	for w := 0; w < workers; w++ {
		insWG.Add(1)
		go func(w int) {
			defer insWG.Done()
			local := make([]time.Duration, 0, 1024)
			seq := 0
			for runCtx.Err() == nil {
				start := time.Now()
				if _, err := st.InsertEvent(runCtx, sampleEvent((w+seq)%devices, seq)); err != nil {
					if runCtx.Err() != nil {
						break
					}
					continue
				}
				local = append(local, time.Since(start))
				seq++
			}
			mu.Lock()
			samples = append(samples, local...)
			mu.Unlock()
		}(w)
	}
	insWG.Wait()
	cancel()
	cpWG.Wait()
	return samples
}

// ---------------------------------------------------------------------------
// Disjoint-row batched writers — the textbook MVCC best case.
//
// The docs claim "disjoint-row writers run in parallel." This is the
// fairest test of that: each writer owns its own contiguous range of
// rows and updates them in batched transactions, so there are zero
// conflicts. Both modes amortize the O(N) per-commit save over the
// whole batch:
//
//   - serialized issues an explicit BEGIN … COMMIT under one lock, so
//     writers fully serialize (the honest single-writer baseline — not
//     the autocommit-per-row strawman).
//   - concurrent issues BEGIN CONCURRENT on independent sibling
//     connections, so the transactions can interleave.
//
// If v0 MVCC buys throughput anywhere, it's here. (Spoiler: the global
// per-database mutex + the per-transaction table clone mean it doesn't,
// yet — but this measures it honestly rather than asserting it.)
func runDisjoint(workers int, duration time.Duration) {
	const rangeSize = 100 // rows each writer owns
	const batch = 20      // updates per transaction

	fmt.Printf("\nDisjoint-row batched writers (workers=%d, duration=%s, "+
		"range=%d rows/writer, %d updates/txn)\n", workers, duration, rangeSize, batch)
	fmt.Println("Each writer updates only its own rows — zero conflicts. Higher is better.")
	fmt.Println()
	fmt.Printf("%-14s %14s %12s\n", "write mode", "updates/sec", "conflicts")
	fmt.Println("--------------------------------------------------")

	for _, mode := range []store.WriteMode{store.Serialized, store.Concurrent} {
		ups, conflicts, elapsed := disjointOne(mode, workers, duration, rangeSize, batch)
		fmt.Printf("%-14s %14.0f %12d\n", mode.String(),
			float64(ups)/elapsed.Seconds(), conflicts)
	}
}

func disjointOne(mode store.WriteMode, workers int, duration time.Duration, rangeSize, batch int) (int64, int64, time.Duration) {
	ctx := context.Background()
	tmp, _ := os.MkdirTemp("", "disjoint")
	defer os.RemoveAll(tmp)

	st, err := store.Open(ctx, store.Options{
		Path:         filepath.Join(tmp, "d.sqlrite"),
		Mode:         mode,
		MaxOpenConns: workers + 2,
	})
	if err != nil {
		log.Fatalf("open: %v", err)
	}
	defer st.Close()

	// Seed workers*rangeSize rows with known ids 1..M.
	total := workers * rangeSize
	for i := 0; i < total; i++ {
		if _, err := st.InsertEvent(ctx, sampleEvent(i, i)); err != nil {
			log.Fatalf("seed: %v", err)
		}
	}

	runCtx, cancel := context.WithTimeout(ctx, duration)
	defer cancel()

	var updates atomic.Int64
	var wg sync.WaitGroup
	start := time.Now()
	for w := 0; w < workers; w++ {
		wg.Add(1)
		go func(w int) {
			defer wg.Done()
			base := int64(w*rangeSize) + 1 // ids base..base+rangeSize-1
			off := 0
			for runCtx.Err() == nil {
				stmts := make([]string, 0, batch+2)
				if mode == store.Serialized {
					stmts = append(stmts, "BEGIN")
				}
				for b := 0; b < batch; b++ {
					id := base + int64((off+b)%rangeSize)
					stmts = append(stmts, fmt.Sprintf("UPDATE events SET ts = ts + 1 WHERE id = %d", id))
				}
				if mode == store.Serialized {
					stmts = append(stmts, "COMMIT")
				}
				off += batch
				if err := st.RunTxn(runCtx, stmts...); err != nil {
					if runCtx.Err() != nil {
						return
					}
					log.Printf("txn: %v", err)
					return
				}
				updates.Add(int64(batch))
			}
		}(w)
	}
	wg.Wait()
	elapsed := time.Since(start)
	return updates.Load(), st.Stats().Conflicts, elapsed
}

// pctl returns the q-quantile (0..1) of a latency sample. q=1.0 is max.
func pctl(d []time.Duration, q float64) time.Duration {
	if len(d) == 0 {
		return 0
	}
	cp := make([]time.Duration, len(d))
	copy(cp, d)
	sort.Slice(cp, func(i, j int) bool { return cp[i] < cp[j] })
	idx := int(q * float64(len(cp)-1))
	if idx < 0 {
		idx = 0
	}
	if idx >= len(cp) {
		idx = len(cp) - 1
	}
	return cp[idx]
}
