package store

import (
	"bytes"
	"encoding/json"
	"fmt"
	"strings"
)

// The SQLRite Go driver does not support parameter binding — it rejects
// any non-empty argument slice and tells you to inline values into the
// SQL string (see sdk/go/sqlrite.go `rejectParamsForNow`). That makes
// these two helpers load-bearing: every value the collector sends to
// the engine flows through one of them, so this file is the single
// chokepoint that keeps inlined SQL safe against quote-injection and
// malformed JSON.

// quoteText renders a Go string as a SQL single-quoted text literal,
// doubling embedded single quotes per SQLRite's escaping rule
// (docs/supported-sql.md → "Value literals accepted": `'it”s'`).
func quoteText(s string) string {
	return "'" + strings.ReplaceAll(s, "'", "''") + "'"
}

// maxPayloadBytes bounds the compacted JSON payload of a single event.
//
// This isn't arbitrary: under MVCC every committed row is encoded into a
// WAL log-record frame whose body is capped at 4 KiB
// (docs/concurrent-writes.md → Durability; verified: a commit whose
// encoded batch exceeds the cap fails with "encoded batch exceeds
// 4096-byte frame body cap"). A single event row's record carries its
// whole image — payload + the other columns + per-record framing — so an
// unbounded payload would make even the hot-path single-row INSERT fail
// at COMMIT. Capping the payload well under 4 KiB guarantees any one row
// commits, which in turn lets the checkpoint mark rows one-at-a-time as
// its safe floor (see Store.writeAdaptive). The headroom below 4096
// covers the device_id / kind columns and the framing overhead.
const maxPayloadBytes = 3072

// quoteJSON validates that payload is well-formed JSON and returns it
// as a SQL text literal for a JSON column, or the bareword NULL when
// the payload is empty. SQLRite validates JSON columns at INSERT time
// (serde_json) and rejects malformed input with a typed error; we
// pre-validate here so a bad client payload becomes a clean 400 rather
// than a 500 bubbling up from the engine. The payload is compacted to
// canonical form first, which both shortens the inlined literal and
// strips any stray control characters that survived JSON parsing.
//
// Oversized payloads are rejected here (see maxPayloadBytes) so they
// surface as a clean client error rather than an opaque MVCC frame-cap
// failure at COMMIT.
func quoteJSON(payload json.RawMessage) (string, error) {
	if len(bytes.TrimSpace(payload)) == 0 {
		return "NULL", nil
	}
	if !json.Valid(payload) {
		return "", fmt.Errorf("payload is not valid JSON")
	}
	var buf bytes.Buffer
	if err := json.Compact(&buf, payload); err != nil {
		return "", fmt.Errorf("compact payload: %w", err)
	}
	if buf.Len() > maxPayloadBytes {
		return "", fmt.Errorf("payload too large: %d bytes (max %d under the MVCC 4 KiB commit-record cap)",
			buf.Len(), maxPayloadBytes)
	}
	return quoteText(buf.String()), nil
}
