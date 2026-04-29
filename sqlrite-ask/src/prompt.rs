//! Prompt construction — turn a schema dump + a natural-language
//! question into the request shape Anthropic expects.
//!
//! ## Structure (matters for prompt caching)
//!
//! `system` is a list of two text blocks:
//!
//! 1. **Rules** — frozen instructions about what dialect of SQL to
//!    emit, what JSON shape to wrap the answer in, and what to do
//!    when the schema doesn't support the question. Byte-stable
//!    across every call, regardless of which DB is connected.
//! 2. **Schema dump** — the output of [`crate::schema::dump_schema`].
//!    Stable for a given DB, changes when the DB's schema changes.
//!    This is the block we put `cache_control: ephemeral` on.
//!
//! The user's question goes in `messages[0]` (always volatile, never
//! cached).
//!
//! Render order is `tools → system → messages`, so a `cache_control`
//! marker on the last system block caches the rules + the schema
//! together. We don't currently send any tools.
//!
//! ## What we ask the model to produce
//!
//! Strict JSON: `{"sql": "...", "explanation": "..."}`. Asking for
//! JSON in the prompt (rather than via the API's structured-output
//! parameter) keeps this crate compatible with non-Anthropic
//! providers we'll add later — Ollama models without structured-
//! output support still need to work.

use serde::Serialize;

/// The system prompt's first block — load-bearing instructions.
///
/// **Edits invalidate every cache.** Only change this when you have
/// a reason; not "I don't like the wording today". The cost is a
/// one-time miss across all callers.
pub const SYSTEM_RULES: &str = "\
You translate natural-language questions into SQL queries against a SQLRite database.

SQLRite is a small SQLite-compatible database. The dialect supported here is a strict subset of SQLite:

- SELECT with WHERE, ORDER BY (single sort key, can be an expression), LIMIT.
- INSERT, UPDATE, DELETE.
- CREATE TABLE, CREATE [UNIQUE] INDEX [IF NOT EXISTS] <name> ON <table> (<col>).
- BEGIN / COMMIT / ROLLBACK.
- Operators: = <> < <= > >= AND OR NOT + - * / % ||.
- Functions: vec_distance_l2(a, b), vec_distance_cosine(a, b), vec_distance_dot(a, b),
  json_extract(json, path), json_type(json[, path]), json_array_length(json[, path]),
  json_object_keys(json[, path]).
- Vector literals are bracket arrays: [0.1, 0.2, 0.3]. Vector columns are VECTOR(N).
- JSON columns store text; query with the json_* functions and a JSONPath subset
  ($, .key, [N], chained).
- Composite-column ORDER BY, JOIN, GROUP BY, aggregates, subqueries, CTEs, LIKE,
  IN, IS NULL, BETWEEN, OFFSET, column aliases (AS), and DISTINCT are NOT supported
  yet. If the user's question requires any of those, return SQL that's as close as
  possible and explain the limitation in the explanation field.

You will see the database schema as a list of CREATE TABLE statements. Use only
those tables and columns; never invent columns that aren't in the schema.

Respond with a single JSON object on one line, no surrounding prose, no Markdown
code fences:

  {\"sql\": \"<the SQL query, single statement, no trailing semicolon required>\", \
\"explanation\": \"<one short sentence on what the query does or why it can't be answered>\"}

If the question can't be answered with the available schema, set sql to an empty
string and explain in the explanation field.\n";

/// One block of an Anthropic `system` array.
///
/// We only ever send `type: "text"`. The `cache_control` field is
/// conditionally serialized — when `None`, it's omitted from the wire
/// JSON (`skip_serializing_if`), so a non-cached request and a cached
/// request produce different bytes — that difference *is* the cache
/// key.
#[derive(Serialize, Debug)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// `cache_control` payload — currently always `ephemeral`. The Anthropic
/// API also accepts `{"type": "ephemeral", "ttl": "1h"}`; we expose
/// that via [`CacheTtl`] in `crate::config`.
#[derive(Serialize, Debug)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<&'static str>,
}

impl CacheControl {
    pub fn ephemeral() -> Self {
        Self {
            kind: "ephemeral",
            ttl: None,
        }
    }

    pub fn ephemeral_1h() -> Self {
        Self {
            kind: "ephemeral",
            ttl: Some("1h"),
        }
    }
}

/// One element of an Anthropic `messages` array. We only ever send
/// `role: "user"` — `ask()` is stateless / one-shot.
#[derive(Serialize, Debug)]
pub struct UserMessage {
    pub role: &'static str,
    pub content: String,
}

impl UserMessage {
    pub fn new(question: &str) -> Self {
        Self {
            role: "user",
            content: question.to_string(),
        }
    }
}

/// Build the `system` array for an Anthropic request.
///
/// `cache_schema` controls whether the schema block carries a
/// `cache_control` breakpoint. Pass `None` to skip caching (e.g.,
/// schemas under the model's ~2K-token minimum cacheable prefix —
/// they silently won't cache anyway, so the cache_control marker is
/// noise without it).
pub fn build_system(schema_dump: &str, cache_schema: Option<CacheControl>) -> Vec<SystemBlock> {
    vec![
        SystemBlock {
            kind: "text",
            text: SYSTEM_RULES.to_string(),
            cache_control: None,
        },
        SystemBlock {
            kind: "text",
            text: format!("<schema>\n{schema_dump}</schema>\n"),
            cache_control: cache_schema,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_control_omitted_when_none() {
        let block = SystemBlock {
            kind: "text",
            text: "hi".to_string(),
            cache_control: None,
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(!json.contains("cache_control"), "got: {json}");
    }

    #[test]
    fn cache_control_emits_ephemeral_when_set() {
        let block = SystemBlock {
            kind: "text",
            text: "hi".to_string(),
            cache_control: Some(CacheControl::ephemeral()),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"cache_control\""), "got: {json}");
        assert!(json.contains("\"ephemeral\""));
        // 5-min TTL is the default — the `ttl` field should be absent.
        assert!(!json.contains("\"ttl\""), "got: {json}");
    }

    #[test]
    fn cache_control_1h_emits_ttl() {
        let block = SystemBlock {
            kind: "text",
            text: "hi".to_string(),
            cache_control: Some(CacheControl::ephemeral_1h()),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"ttl\":\"1h\""), "got: {json}");
    }

    #[test]
    fn build_system_places_cache_marker_only_on_schema_block() {
        let blocks = build_system(
            "CREATE TABLE x (id INTEGER);\n",
            Some(CacheControl::ephemeral()),
        );
        assert_eq!(blocks.len(), 2);
        assert!(
            blocks[0].cache_control.is_none(),
            "rules block must not be marked"
        );
        assert!(
            blocks[1].cache_control.is_some(),
            "schema block must be marked"
        );
    }

    #[test]
    fn schema_block_wraps_dump_in_xml_tags() {
        // The <schema>...</schema> wrapping helps the model spot the
        // boundary between rules and reflection. It's not load-
        // bearing for cache hits (those are byte-level) but it
        // stabilizes the prompt structure across schemas of wildly
        // different size.
        let blocks = build_system("CREATE TABLE foo (id INT);\n", None);
        let text = &blocks[1].text;
        assert!(text.starts_with("<schema>\n"), "got: {text}");
        assert!(text.ends_with("</schema>\n"), "got: {text}");
    }

    #[test]
    fn user_message_roles_are_always_user() {
        let m = UserMessage::new("how many users are over 30?");
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("how many users are over 30?"));
    }
}
