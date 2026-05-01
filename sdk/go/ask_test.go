// Phase 7g.6 — tests for the natural-language → SQL surface.
//
// Run after `cargo build --release -p sqlrite-ffi` so `libsqlrite_c`
// is available for cgo to link against:
//
//     cd sdk/go && go test -run TestAsk
//
// Coverage mirrors the Python (test_ask.py) and Node.js (test_ask.mjs)
// SDKs:
//
//  1. AskConfig / AskConfigFromEnv — defaults, env-var precedence,
//     String() doesn't leak the API key.
//  2. Ask error paths — missing API key, closed db, nil db.
//  3. Happy path against a localhost httptest.Server.
//  4. AskRun executes the generated SQL.
//  5. API 4xx surfacing.
//
// `httptest.Server` runs on its own goroutine — Go's runtime
// schedules it independently of the goroutine making the cgo call,
// so we avoid the "GIL deadlock" Python had + the "main event loop
// deadlock" Node had. Cleanest of the three SDKs from a test-setup
// perspective.

package sqlrite_test

import (
	"database/sql"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"

	sqlrite "github.com/joaoh82/rust_sqlite/sdk/go"
)

// successBody is the JSON the mock LLM endpoint returns for a happy-
// path test. The inner SQL is `SELECT id, name FROM users` — a shape
// the engine actually supports (no aggregates, just a bare projection).
const successBody = `{
  "id": "msg_test",
  "type": "message",
  "role": "assistant",
  "model": "claude-sonnet-4-6",
  "content": [
    {
      "type": "text",
      "text": "{\"sql\": \"SELECT id, name FROM users\", \"explanation\": \"lists users\"}"
    }
  ],
  "stop_reason": "end_turn",
  "usage": {
    "input_tokens": 1234,
    "output_tokens": 56,
    "cache_creation_input_tokens": 1000,
    "cache_read_input_tokens": 0
  }
}`

// captured holds what the mock server saw on its last request — tests
// inspect this to verify the FFI assembled the correct payload.
type captured struct {
	Path    string
	Headers http.Header
	Body    map[string]any
}

// startMockServer spins up an httptest.Server that captures one POST
// and serves canned status+body. Closes itself on test teardown.
func startMockServer(t *testing.T, status int, body string) (*httptest.Server, *captured) {
	t.Helper()
	cap := &captured{}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		bodyBytes, _ := io.ReadAll(r.Body)
		var parsed map[string]any
		_ = json.Unmarshal(bodyBytes, &parsed)
		cap.Path = r.URL.Path
		cap.Headers = r.Header.Clone()
		cap.Body = parsed
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(status)
		_, _ = w.Write([]byte(body))
	}))
	t.Cleanup(srv.Close)
	return srv, cap
}

// withEnvIsolation snapshots SQLRITE_LLM_* vars, clears them, runs
// fn, then restores. Important because tests mutate env to verify
// AskConfigFromEnv behavior.
func withEnvIsolation(t *testing.T, fn func()) {
	t.Helper()
	keys := []string{
		"SQLRITE_LLM_PROVIDER",
		"SQLRITE_LLM_API_KEY",
		"SQLRITE_LLM_MODEL",
		"SQLRITE_LLM_MAX_TOKENS",
		"SQLRITE_LLM_CACHE_TTL",
	}
	saved := make(map[string]string, len(keys))
	for _, k := range keys {
		saved[k] = os.Getenv(k)
		os.Unsetenv(k)
	}
	t.Cleanup(func() {
		for _, k := range keys {
			if v, ok := saved[k]; ok && v != "" {
				os.Setenv(k, v)
			} else {
				os.Unsetenv(k)
			}
		}
	})
	fn()
}

// ---------------------------------------------------------------------------
// AskConfig + AskConfigFromEnv

func TestAskConfigFromEnvDefaults(t *testing.T) {
	withEnvIsolation(t, func() {
		cfg, err := sqlrite.AskConfigFromEnv()
		if err != nil {
			t.Fatalf("AskConfigFromEnv: %v", err)
		}
		if cfg.Provider != "anthropic" {
			t.Errorf("provider = %q, want %q", cfg.Provider, "anthropic")
		}
		if cfg.Model != "claude-sonnet-4-6" {
			t.Errorf("model = %q, want %q", cfg.Model, "claude-sonnet-4-6")
		}
		if cfg.MaxTokens != 1024 {
			t.Errorf("maxTokens = %d, want 1024", cfg.MaxTokens)
		}
		if cfg.CacheTTL != "5m" {
			t.Errorf("cacheTTL = %q, want %q", cfg.CacheTTL, "5m")
		}
		if cfg.APIKey != "" {
			t.Errorf("apiKey should be empty by default, got %q", cfg.APIKey)
		}
	})
}

func TestAskConfigFromEnvOverrides(t *testing.T) {
	withEnvIsolation(t, func() {
		os.Setenv("SQLRITE_LLM_API_KEY", "env-key")
		os.Setenv("SQLRITE_LLM_MODEL", "claude-haiku-4-5")
		os.Setenv("SQLRITE_LLM_MAX_TOKENS", "512")
		os.Setenv("SQLRITE_LLM_CACHE_TTL", "1h")

		cfg, err := sqlrite.AskConfigFromEnv()
		if err != nil {
			t.Fatalf("AskConfigFromEnv: %v", err)
		}
		if cfg.APIKey != "env-key" {
			t.Errorf("apiKey = %q, want env-key", cfg.APIKey)
		}
		if cfg.Model != "claude-haiku-4-5" {
			t.Errorf("model = %q, want claude-haiku-4-5", cfg.Model)
		}
		if cfg.MaxTokens != 512 {
			t.Errorf("maxTokens = %d, want 512", cfg.MaxTokens)
		}
		if cfg.CacheTTL != "1h" {
			t.Errorf("cacheTTL = %q, want 1h", cfg.CacheTTL)
		}
	})
}

func TestAskConfigFromEnvInvalidMaxTokens(t *testing.T) {
	withEnvIsolation(t, func() {
		os.Setenv("SQLRITE_LLM_MAX_TOKENS", "not-an-int")
		_, err := sqlrite.AskConfigFromEnv()
		if err == nil {
			t.Fatal("expected error for invalid SQLRITE_LLM_MAX_TOKENS")
		}
		if !strings.Contains(err.Error(), "MAX_TOKENS") {
			t.Errorf("error %q should mention MAX_TOKENS", err)
		}
	})
}

func TestAskConfigStringDoesNotLeakAPIKey(t *testing.T) {
	cfg := &sqlrite.AskConfig{
		Provider: "anthropic",
		APIKey:   "sk-ant-supersecret",
		Model:    "claude-sonnet-4-6",
	}
	s := cfg.String()
	if strings.Contains(s, "sk-ant-supersecret") {
		t.Errorf("String() leaked the API key value: %s", s)
	}
	if !strings.Contains(s, "<set>") {
		t.Errorf("String() should mark apiKey as <set>: %s", s)
	}

	cfg2 := &sqlrite.AskConfig{}
	if !strings.Contains(cfg2.String(), "<unset>") {
		t.Errorf("empty config String() should show <unset>: %s", cfg2.String())
	}
}

// ---------------------------------------------------------------------------
// Ask error paths

func TestAskNilDb(t *testing.T) {
	_, err := sqlrite.Ask(nil, "anything", nil)
	if err == nil {
		t.Fatal("expected error for nil db")
	}
	if !strings.Contains(err.Error(), "db is nil") {
		t.Errorf("error %q should mention nil db", err)
	}
}

func TestAskMissingApiKey(t *testing.T) {
	withEnvIsolation(t, func() {
		db := openMem(t)
		_, err := sqlrite.Ask(db, "How many users?", nil)
		if err == nil {
			t.Fatal("expected error when API key is missing")
		}
		if !strings.Contains(err.Error(), "missing API key") {
			t.Errorf("error %q should mention missing API key", err)
		}
	})
}

func TestAskOnClosedDb(t *testing.T) {
	withEnvIsolation(t, func() {
		// Open + close, then try to ask. Even with an explicit cfg we
		// expect the error — the conn is gone.
		db, err := sql.Open(sqlrite.DriverName, ":memory:")
		if err != nil {
			t.Fatalf("sql.Open: %v", err)
		}
		db.Close()
		cfg := &sqlrite.AskConfig{APIKey: "test-key"}
		_, err = sqlrite.Ask(db, "anything", cfg)
		if err == nil {
			t.Fatal("expected error for closed db")
		}
	})
}

// ---------------------------------------------------------------------------
// Happy path against httptest.Server

func TestAskHappyPath(t *testing.T) {
	withEnvIsolation(t, func() {
		db := openMem(t)
		if _, err := db.Exec(`CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)`); err != nil {
			t.Fatalf("create table: %v", err)
		}

		srv, cap := startMockServer(t, 200, successBody)
		cfg := &sqlrite.AskConfig{APIKey: "test-key", BaseURL: srv.URL}
		resp, err := sqlrite.Ask(db, "How many users are over 30?", cfg)
		if err != nil {
			t.Fatalf("Ask: %v", err)
		}

		if resp.SQL != "SELECT id, name FROM users" {
			t.Errorf("sql = %q", resp.SQL)
		}
		if resp.Explanation != "lists users" {
			t.Errorf("explanation = %q", resp.Explanation)
		}
		if resp.Usage.InputTokens != 1234 {
			t.Errorf("input_tokens = %d, want 1234", resp.Usage.InputTokens)
		}
		if resp.Usage.CacheCreationInputTokens != 1000 {
			t.Errorf("cache_creation = %d, want 1000", resp.Usage.CacheCreationInputTokens)
		}

		// Verify what the FFI sent.
		if cap.Body["model"] != "claude-sonnet-4-6" {
			t.Errorf("body.model = %v, want claude-sonnet-4-6", cap.Body["model"])
		}
		if cap.Body["max_tokens"].(float64) != 1024 {
			t.Errorf("body.max_tokens = %v, want 1024", cap.Body["max_tokens"])
		}
		// Schema dump in the system block.
		systemArr := cap.Body["system"].([]any)
		schemaBlock := systemArr[1].(map[string]any)
		if !strings.Contains(schemaBlock["text"].(string), "CREATE TABLE users") {
			t.Errorf("system block 1 missing CREATE TABLE: %v", schemaBlock["text"])
		}
		// Cache marker.
		cc := schemaBlock["cache_control"].(map[string]any)
		if cc["type"] != "ephemeral" {
			t.Errorf("cache_control.type = %v, want ephemeral", cc["type"])
		}
		// Auth headers.
		if cap.Headers.Get("X-Api-Key") != "test-key" {
			t.Errorf("X-Api-Key = %q", cap.Headers.Get("X-Api-Key"))
		}
		if cap.Headers.Get("Anthropic-Version") != "2023-06-01" {
			t.Errorf("Anthropic-Version = %q", cap.Headers.Get("Anthropic-Version"))
		}
		// User question
		messages := cap.Body["messages"].([]any)
		first := messages[0].(map[string]any)
		if first["content"] != "How many users are over 30?" {
			t.Errorf("user message = %v", first["content"])
		}
	})
}

func TestAskRunExecutesGeneratedSQL(t *testing.T) {
	withEnvIsolation(t, func() {
		db := openMem(t)
		if _, err := db.Exec(`CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)`); err != nil {
			t.Fatalf("create table: %v", err)
		}
		if _, err := db.Exec(`INSERT INTO users (name, age) VALUES ('alice', 30)`); err != nil {
			t.Fatalf("insert: %v", err)
		}
		if _, err := db.Exec(`INSERT INTO users (name, age) VALUES ('bob', 25)`); err != nil {
			t.Fatalf("insert: %v", err)
		}

		srv, _ := startMockServer(t, 200, successBody)
		cfg := &sqlrite.AskConfig{APIKey: "test-key", BaseURL: srv.URL}
		rows, err := sqlrite.AskRun(db, "list users", cfg)
		if err != nil {
			t.Fatalf("AskRun: %v", err)
		}
		defer rows.Close()

		var names []string
		for rows.Next() {
			var id int64
			var name string
			if err := rows.Scan(&id, &name); err != nil {
				t.Fatalf("scan: %v", err)
			}
			names = append(names, name)
		}
		if len(names) != 2 {
			t.Errorf("expected 2 rows, got %d", len(names))
		}
	})
}

func TestAskRunOnEmptySQLResponseErrors(t *testing.T) {
	withEnvIsolation(t, func() {
		db := openMem(t)
		// Mock returns sql="" — model declined.
		declineBody := strings.Replace(
			successBody,
			`{\"sql\": \"SELECT id, name FROM users\", \"explanation\": \"lists users\"}`,
			`{\"sql\": \"\", \"explanation\": \"schema lacks a widgets table\"}`,
			1,
		)
		srv, _ := startMockServer(t, 200, declineBody)
		cfg := &sqlrite.AskConfig{APIKey: "test-key", BaseURL: srv.URL}
		_, err := sqlrite.AskRun(db, "how many widgets?", cfg)
		if err == nil {
			t.Fatal("expected error for empty SQL response")
		}
		if !strings.Contains(err.Error(), "declined") {
			t.Errorf("error %q should mention 'declined'", err)
		}
		if !strings.Contains(err.Error(), "widgets table") {
			t.Errorf("error %q should include the model's explanation", err)
		}
	})
}

// ---------------------------------------------------------------------------
// API error surfacing

func TestAsk4xxResponseSurfaces(t *testing.T) {
	withEnvIsolation(t, func() {
		db := openMem(t)
		errBody := `{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens too large"}}`
		srv, _ := startMockServer(t, 400, errBody)
		cfg := &sqlrite.AskConfig{APIKey: "test-key", BaseURL: srv.URL}
		_, err := sqlrite.Ask(db, "anything", cfg)
		if err == nil {
			t.Fatal("expected error for 400 response")
		}
		if !strings.Contains(err.Error(), "400") {
			t.Errorf("error %q should mention status 400", err)
		}
		if !strings.Contains(err.Error(), "invalid_request_error") {
			t.Errorf("error %q should include Anthropic error type", err)
		}
		if !strings.Contains(err.Error(), "max_tokens too large") {
			t.Errorf("error %q should include Anthropic error message", err)
		}
	})
}
