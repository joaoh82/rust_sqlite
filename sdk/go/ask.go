// Phase 7g.6 — natural-language → SQL surface for the Go SDK.
//
// Wraps the C FFI `sqlrite_ask` function with idiomatic Go types:
//
//	import (
//	    "database/sql"
//	    sqlrite "github.com/joaoh82/rust_sqlite/sdk/go"
//	)
//
//	db, _ := sql.Open("sqlrite", "foo.sqlrite")
//	resp, err := sqlrite.Ask(db, "How many users are over 30?", nil)
//	fmt.Println(resp.SQL)         // "SELECT COUNT(*) FROM users WHERE age > 30"
//	fmt.Println(resp.Explanation) // one-sentence rationale
//
// Three precedence layers for the API key (matching the Python and
// Node SDKs):
//
//  1. Per-call AskConfig (highest — pass via the `cfg` arg)
//  2. AskConfigFromEnv() (zero-config fallback)
//  3. Built-in defaults (anthropic / claude-sonnet-4-6 / 1024 / 5m)
//
// The cgo bridge passes the config to C as a JSON string and receives
// the response as a JSON string, parsed on the Go side. That keeps
// the C ABI tiny (one function) and lets us add fields later without
// breaking the bindings.

package sqlrite

/*
#include <stdlib.h>
#include "sqlrite.h"
*/
import "C"

import (
	"context"
	"database/sql"
	"database/sql/driver"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strconv"
	"strings"
	"unsafe"
)

// ---------------------------------------------------------------------------
// AskConfig

// AskConfig configures one or more `Ask` / `AskRun` calls. All fields
// are optional; missing fields fall back to env vars and then to
// built-in defaults.
//
// Field tags map to the JSON keys the C FFI expects, so encoding the
// struct with `encoding/json` and handing it across the cgo boundary
// is a one-liner.
type AskConfig struct {
	Provider  string `json:"provider,omitempty"`
	APIKey    string `json:"api_key,omitempty"`
	Model     string `json:"model,omitempty"`
	MaxTokens uint32 `json:"max_tokens,omitempty"`
	// CacheTTL accepts "5m" (default), "1h", or "off". Aliases like
	// "5min" / "1hr" / "none" / "disabled" are also recognized — the
	// C side does the canonicalization.
	CacheTTL string `json:"cache_ttl,omitempty"`
	// BaseURL overrides the API endpoint. Production callers leave
	// empty; tests point this at a localhost mock.
	BaseURL string `json:"base_url,omitempty"`
}

// AskConfigFromEnv reads SQLRITE_LLM_* env vars and builds an
// AskConfig. Mirrors the Rust `AskConfig::from_env()` shape so the
// Go side has the same zero-config experience as every other SDK.
//
// Recognized vars:
//   - SQLRITE_LLM_PROVIDER   (default: anthropic)
//   - SQLRITE_LLM_API_KEY
//   - SQLRITE_LLM_MODEL      (default: claude-sonnet-4-6)
//   - SQLRITE_LLM_MAX_TOKENS (default: 1024)
//   - SQLRITE_LLM_CACHE_TTL  (default: 5m)
//
// A missing API key is NOT an error here — the call to Ask() raises
// the friendlier "missing API key" message later.
//
// (Note: the C side ALSO reads env vars during ask(), so passing a
// zero-value `&AskConfig{}` to Ask() works too. AskConfigFromEnv is
// useful when callers want to inspect / log / mutate the resolved
// config before sending it.)
func AskConfigFromEnv() (*AskConfig, error) {
	cfg := &AskConfig{
		Provider: envOrDefault("SQLRITE_LLM_PROVIDER", "anthropic"),
		APIKey:   os.Getenv("SQLRITE_LLM_API_KEY"),
		Model:    envOrDefault("SQLRITE_LLM_MODEL", "claude-sonnet-4-6"),
		CacheTTL: envOrDefault("SQLRITE_LLM_CACHE_TTL", "5m"),
	}
	if v := os.Getenv("SQLRITE_LLM_MAX_TOKENS"); v != "" {
		n, err := strconv.ParseUint(v, 10, 32)
		if err != nil {
			return nil, fmt.Errorf("sqlrite: SQLRITE_LLM_MAX_TOKENS not a u32: %s", v)
		}
		cfg.MaxTokens = uint32(n)
	} else {
		cfg.MaxTokens = 1024
	}
	return cfg, nil
}

// String returns a human-readable representation that **deliberately
// omits the API key value**. Lets callers `fmt.Println(cfg)` in
// debug output without leaking the secret. Shows `apiKey=<set>` or
// `apiKey=<unset>` so callers can tell whether a key is configured.
func (c *AskConfig) String() string {
	keyStatus := "<unset>"
	if c.APIKey != "" {
		keyStatus = "<set>"
	}
	return fmt.Sprintf(
		"AskConfig(provider=%q, model=%q, maxTokens=%d, cacheTtl=%q, apiKey=%s)",
		c.Provider, c.Model, c.MaxTokens, c.CacheTTL, keyStatus,
	)
}

func envOrDefault(key, dflt string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return dflt
}

// ---------------------------------------------------------------------------
// AskResponse

// AskResponse is what `Ask` returns. Carries the generated SQL, the
// model's one-sentence rationale, and token usage.
//
// **The API key is not in here** — by design.
type AskResponse struct {
	SQL         string   `json:"sql"`
	Explanation string   `json:"explanation"`
	Usage       AskUsage `json:"usage"`
}

// AskUsage is the token-usage breakdown from an `Ask` call. Inspect
// `CacheReadInputTokens` to verify prompt-caching is actually
// working — if it stays zero across repeated calls with the same
// schema, something in the prefix is invalidating the cache.
type AskUsage struct {
	InputTokens              uint64 `json:"input_tokens"`
	OutputTokens             uint64 `json:"output_tokens"`
	CacheCreationInputTokens uint64 `json:"cache_creation_input_tokens"`
	CacheReadInputTokens     uint64 `json:"cache_read_input_tokens"`
}

// ---------------------------------------------------------------------------
// Public API: Ask + AskContext + AskRun + AskRunContext

// Ask generates SQL from a natural-language question via the
// configured LLM provider. Returns an `AskResponse` with the
// generated SQL, rationale, and token usage. Does **not** execute
// the SQL — call `db.Query(resp.SQL)` (or use `AskRun` for one-shot).
//
// `cfg` may be nil to use env vars + defaults. Pass an explicit
// `*AskConfig` to override per-call.
//
// Equivalent to `AskContext(context.Background(), db, question, cfg)`.
func Ask(db *sql.DB, question string, cfg *AskConfig) (*AskResponse, error) {
	return AskContext(context.Background(), db, question, cfg)
}

// AskContext is the context-aware form of `Ask`. The context is used
// for `db.Conn(ctx)` (acquiring a connection from the pool); the
// LLM HTTP call inside the C library is currently uncancellable —
// it'll run to completion / error regardless of the context. A
// future SDK rev can plumb cancellation through `sqlrite_ask` once
// the FFI grows a cancel hook.
func AskContext(ctx context.Context, db *sql.DB, question string, cfg *AskConfig) (*AskResponse, error) {
	if db == nil {
		return nil, errors.New("sqlrite: Ask: db is nil")
	}
	dbConn, err := db.Conn(ctx)
	if err != nil {
		return nil, fmt.Errorf("sqlrite: Ask: %w", err)
	}
	defer dbConn.Close()

	var resp *AskResponse
	err = dbConn.Raw(func(driverConn any) error {
		c, ok := driverConn.(*conn)
		if !ok {
			return fmt.Errorf("sqlrite: Ask: driver connection is %T, not *sqlrite.conn — Ask requires a sqlrite-backed *sql.DB", driverConn)
		}
		r, err := c.ask(question, cfg)
		if err != nil {
			return err
		}
		resp = r
		return nil
	})
	return resp, err
}

// AskRun generates SQL **and executes it as a query**. Returns
// `*sql.Rows` ready for iteration (matching `db.Query`).
//
// `cfg` may be nil to use env vars + defaults.
//
// **Returns an error on empty SQL response** (model declined to
// generate SQL for this schema) rather than executing the empty
// string — the model's explanation is in the error message.
//
// Convenience for one-shot scripts. For interactive use, prefer
// `Ask` + manual review (the model can be wrong; auto-execute hides
// that).
//
// Equivalent to `AskRunContext(context.Background(), db, question, cfg)`.
func AskRun(db *sql.DB, question string, cfg *AskConfig) (*sql.Rows, error) {
	return AskRunContext(context.Background(), db, question, cfg)
}

// AskRunContext is the context-aware form of `AskRun`. The context
// flows through to both the `Ask` call and the subsequent `Query`.
func AskRunContext(ctx context.Context, db *sql.DB, question string, cfg *AskConfig) (*sql.Rows, error) {
	resp, err := AskContext(ctx, db, question, cfg)
	if err != nil {
		return nil, err
	}
	trimmed := strings.TrimSpace(resp.SQL)
	if trimmed == "" {
		expl := resp.Explanation
		if expl == "" {
			expl = "(no explanation)"
		}
		return nil, fmt.Errorf("sqlrite: AskRun: model declined to generate SQL: %s", expl)
	}
	return db.QueryContext(ctx, trimmed)
}

// ---------------------------------------------------------------------------
// Internal: ask on the conn

// ask runs the C FFI `sqlrite_ask` call. Holds the conn mutex for
// the duration of the call (which includes the synchronous HTTP
// round-trip to the LLM, ~hundreds of ms typical, capped at ~90s by
// ureq). Other goroutines using the same connection wait — same
// lock discipline as `exec` / `query`.
func (c *conn) ask(question string, cfg *AskConfig) (*AskResponse, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil, driver.ErrBadConn
	}

	// Marshal the config (or empty string if cfg is nil).
	var configJSON string
	if cfg != nil {
		raw, err := json.Marshal(cfg)
		if err != nil {
			return nil, fmt.Errorf("sqlrite: ask: marshal config: %w", err)
		}
		configJSON = string(raw)
	}

	cQuestion := cString(question)
	defer freeCString(cQuestion)
	var cConfig *C.char
	if configJSON != "" {
		cConfig = cString(configJSON)
		defer freeCString(cConfig)
	}

	var out *C.char
	status := Status(C.sqlrite_ask(c.handle, cQuestion, cConfig, &out))
	if err := wrapErr(status, "ask"); err != nil {
		return nil, err
	}
	if out == nil {
		return nil, errors.New("sqlrite: ask: FFI returned status=ok but null response")
	}
	defer C.sqlrite_free_string(out)

	jsonStr := C.GoString(out)
	var resp AskResponse
	if err := json.Unmarshal([]byte(jsonStr), &resp); err != nil {
		return nil, fmt.Errorf("sqlrite: ask: parse response JSON: %w (raw=%q)", err, jsonStr)
	}
	return &resp, nil
}

// ---------------------------------------------------------------------------
// Compile-time check that `unsafe` is imported (cgo emits unused-import
// warnings otherwise).
var _ = unsafe.Sizeof(C.int(0))
