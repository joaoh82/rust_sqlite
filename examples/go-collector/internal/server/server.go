// Package server is the HTTP front door: POST /events writes into the
// durable buffer, GET /healthz and GET /stats expose ops state, and a
// backlog ceiling drives 503 backpressure when the buffer fills.
package server

import (
	"encoding/json"
	"errors"
	"log"
	"net/http"
	"time"

	"github.com/joaoh82/rust_sqlite/examples/go-collector/internal/store"
	"github.com/joaoh82/rust_sqlite/examples/go-collector/internal/uploader"
)

// Config tunes the server's backpressure policy.
type Config struct {
	MaxBacklog int64 // reject writes with 503 once backlog reaches this (0 = unlimited)
	Logger     *log.Logger
}

// Server wires the store + uploader behind an http.Handler.
type Server struct {
	store *store.Store
	up    *uploader.Uploader
	cfg   Config
	log   *log.Logger
}

// New builds a Server.
func New(s *store.Store, up *uploader.Uploader, cfg Config) *Server {
	lg := cfg.Logger
	if lg == nil {
		lg = log.Default()
	}
	return &Server{store: s, up: up, cfg: cfg, log: lg}
}

// Handler returns the routed http.Handler.
func (s *Server) Handler() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("/events", s.handleEvents)
	mux.HandleFunc("/healthz", s.handleHealthz)
	mux.HandleFunc("/stats", s.handleStats)
	return mux
}

// eventRequest accepts either a single event object or a JSON array of
// them, so the demo client can post one-at-a-time or in small batches.
func (s *Server) handleEvents(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}

	// Backpressure: the buffer is a finite disk-backed queue. When it's
	// full we shed load with 503 rather than growing unbounded — the
	// producer is expected to retry with backoff.
	if s.cfg.MaxBacklog > 0 && s.store.Backlog() >= s.cfg.MaxBacklog {
		writeJSON(w, http.StatusServiceUnavailable, map[string]any{
			"error":   "buffer full",
			"backlog": s.store.Backlog(),
		})
		return
	}

	defer r.Body.Close()
	dec := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<20))

	// Peek the first token to decide object vs array without buffering
	// the whole body twice.
	var raw json.RawMessage
	if err := dec.Decode(&raw); err != nil {
		http.Error(w, "invalid JSON body", http.StatusBadRequest)
		return
	}

	var events []store.Event
	if len(raw) > 0 && raw[0] == '[' {
		if err := json.Unmarshal(raw, &events); err != nil {
			http.Error(w, "invalid JSON array of events", http.StatusBadRequest)
			return
		}
	} else {
		var ev store.Event
		if err := json.Unmarshal(raw, &ev); err != nil {
			http.Error(w, "invalid JSON event", http.StatusBadRequest)
			return
		}
		events = []store.Event{ev}
	}

	now := time.Now().UnixMilli()
	ids := make([]int64, 0, len(events))
	for i := range events {
		if events[i].DeviceID == "" {
			http.Error(w, "device_id is required", http.StatusBadRequest)
			return
		}
		if events[i].Kind == "" {
			http.Error(w, "kind is required", http.StatusBadRequest)
			return
		}
		if events[i].TS == 0 {
			events[i].TS = now // server-stamp receipt time
		}
		id, err := s.store.InsertEvent(r.Context(), events[i])
		if err != nil {
			if errors.Is(err, store.ErrBadPayload) {
				http.Error(w, err.Error(), http.StatusBadRequest)
				return
			}
			s.log.Printf("insert event: %v", err)
			http.Error(w, "failed to store event", http.StatusInternalServerError)
			return
		}
		ids = append(ids, id)
	}

	writeJSON(w, http.StatusOK, map[string]any{
		"accepted": len(ids),
		"ids":      ids,
	})
}

func (s *Server) handleHealthz(w http.ResponseWriter, r *http.Request) {
	health := s.up.Health()
	backlogOK := s.cfg.MaxBacklog == 0 || s.store.Backlog() < s.cfg.MaxBacklog
	ok := health.Healthy && backlogOK

	status := http.StatusOK
	if !ok {
		status = http.StatusServiceUnavailable
	}
	writeJSON(w, status, map[string]any{
		"ok":         ok,
		"uploader":   health,
		"backlog":    s.store.Backlog(),
		"backlog_ok": backlogOK,
	})
}

func (s *Server) handleStats(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{
		"store":    s.store.Stats(),
		"uploader": s.up.Health(),
	})
}

func writeJSON(w http.ResponseWriter, status int, body any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(body)
}
