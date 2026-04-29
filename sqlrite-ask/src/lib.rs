//! `sqlrite-ask` — natural-language → SQL adapter for SQLRite.
//!
//! Phase 7g.1 (foundational). One sync call:
//!
//! ```no_run
//! use sqlrite::Connection;
//! use sqlrite_ask::{ask, AskConfig};
//!
//! let conn = Connection::open("foo.sqlrite")?;
//! let config = AskConfig::from_env()?;  // reads SQLRITE_LLM_API_KEY etc.
//! let response = ask(&conn, "How many users are over 30?", &config)?;
//! println!("Generated SQL: {}", response.sql);
//! println!("Why: {}", response.explanation);
//! # Ok::<(), sqlrite_ask::AskError>(())
//! ```
//!
//! ## What this crate is
//!
//! - Reflects the schema of an open `Connection` into CREATE TABLE
//!   text the LLM can ground on.
//! - Wraps that schema in a stable system prompt with an
//!   `cache_control: ephemeral` breakpoint so the schema dump is
//!   served from Anthropic's prompt cache after the first call.
//! - Sends one HTTP POST to the LLM provider per `ask()` call.
//! - Parses the response into `AskResponse { sql, explanation }`.
//!
//! ## What this crate is NOT
//!
//! - **Not an executor.** The library deliberately does not run the
//!   generated SQL — the caller decides whether to execute it. SDK
//!   layers (`Python.Connection.ask_run`, `Node.db.askRun`, etc.)
//!   add a one-shot generate-and-execute helper for the common
//!   case, but the default API is "generate, return, let me decide".
//! - **Not multi-turn.** Stateless — every call is a fresh prompt.
//! - **Not multi-provider yet.** Anthropic-first per Phase 7 plan
//!   Q4. OpenAI + Ollama follow-ups slot into [`provider`] without
//!   changing the public surface.
//!
//! ## Configuration
//!
//! [`AskConfig`] resolves in this priority order:
//! 1. Explicit values you set on the struct (`AskConfig { api_key: Some(...), .. }`)
//! 2. Environment variables (`SQLRITE_LLM_*`)
//! 3. Built-in defaults (model = `claude-sonnet-4-6`, max_tokens = 1024,
//!    cache TTL = 5 min)

use std::env;

use sqlrite::Connection;

mod prompt;
mod provider;
pub mod schema;

pub use provider::anthropic::AnthropicProvider;
pub use provider::{Provider, Request, Response, Usage};

use prompt::{CacheControl, UserMessage, build_system};
use provider::Request as ProviderRequest;

/// Default model — Sonnet 4.6 hits the cost-quality sweet spot for
/// NL→SQL. Override via `AskConfig::model` or the `SQLRITE_LLM_MODEL`
/// env var. See `docs/phase-7-plan.md` for the model-choice rationale.
pub const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

/// Default `max_tokens`. SQL generation rarely needs more than ~500
/// output tokens (single-statement queries + a one-sentence
/// explanation). 1024 leaves headroom; under the SDK timeout cap so
/// we don't have to stream.
pub const DEFAULT_MAX_TOKENS: u32 = 1024;

/// Result returned from a successful [`ask`] call.
///
/// `sql` is the generated query text — empty string if the model
/// determined the question can't be answered against the schema.
/// `explanation` is the model's one-sentence rationale; useful in
/// REPL "confirm before run" UIs.
///
/// `usage` surfaces token counts (input/output/cache hit/cache write).
/// Inspect it to verify prompt-caching is actually working — see
/// `docs/phase-7-plan.md` Q3-adjacent for the audit checklist.
#[derive(Debug, Clone)]
pub struct AskResponse {
    pub sql: String,
    pub explanation: String,
    pub usage: Usage,
}

/// Cache-TTL knob exposed on [`AskConfig`].
///
/// Anthropic's `ephemeral` cache supports two TTLs:
/// - **5 minutes** (default) — break-even at 2 calls per cached
///   prefix; right for interactive REPL use where users ask a few
///   questions in a session.
/// - **1 hour** — costs 2× write premium instead of 1.25×; needs
///   3+ calls per prefix to break even. Worth it for long-running
///   editor / desktop sessions where the same DB is queried
///   sporadically over an hour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheTtl {
    FiveMinutes,
    OneHour,
    /// Disables caching — schema block is sent without a
    /// `cache_control` marker. Useful when the schema is below the
    /// model's minimum cacheable prefix size (~2K tokens for Sonnet,
    /// ~4K for Haiku/Opus); marking it would be a no-op.
    Off,
}

impl CacheTtl {
    fn into_marker(self) -> Option<CacheControl> {
        match self {
            CacheTtl::FiveMinutes => Some(CacheControl::ephemeral()),
            CacheTtl::OneHour => Some(CacheControl::ephemeral_1h()),
            CacheTtl::Off => None,
        }
    }
}

/// Which LLM provider [`ask`] talks to. Anthropic-only in 7g.1; the
/// enum is here so adding OpenAI/Ollama later doesn't break the
/// `AskConfig` shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
}

impl ProviderKind {
    fn parse(s: &str) -> Result<Self, AskError> {
        match s.to_ascii_lowercase().as_str() {
            "anthropic" => Ok(ProviderKind::Anthropic),
            other => Err(AskError::UnknownProvider(other.to_string())),
        }
    }
}

/// Knobs for an `ask()` call. Construct directly, or via
/// [`AskConfig::from_env`] to pull defaults from the environment.
#[derive(Debug, Clone)]
pub struct AskConfig {
    pub provider: ProviderKind,
    pub api_key: Option<String>,
    pub model: String,
    pub max_tokens: u32,
    pub cache_ttl: CacheTtl,
    /// Override the API base URL. Production callers leave this
    /// `None`; tests point it at a localhost mock.
    pub base_url: Option<String>,
}

impl Default for AskConfig {
    fn default() -> Self {
        Self {
            provider: ProviderKind::Anthropic,
            api_key: None,
            model: DEFAULT_MODEL.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            cache_ttl: CacheTtl::FiveMinutes,
            base_url: None,
        }
    }
}

impl AskConfig {
    /// Build a config from environment variables, with built-in
    /// defaults for anything not set.
    ///
    /// Recognized vars:
    /// - `SQLRITE_LLM_PROVIDER` — `anthropic` (only currently supported)
    /// - `SQLRITE_LLM_API_KEY` — required at call time, but a missing
    ///   var is not an error here (lets you build a config to inspect
    ///   without the secret loaded)
    /// - `SQLRITE_LLM_MODEL` — overrides [`DEFAULT_MODEL`]
    /// - `SQLRITE_LLM_MAX_TOKENS` — overrides [`DEFAULT_MAX_TOKENS`]
    /// - `SQLRITE_LLM_CACHE_TTL` — `5m` (default) | `1h` | `off`
    pub fn from_env() -> Result<Self, AskError> {
        let mut cfg = AskConfig::default();
        if let Ok(p) = env::var("SQLRITE_LLM_PROVIDER") {
            cfg.provider = ProviderKind::parse(&p)?;
        }
        if let Ok(k) = env::var("SQLRITE_LLM_API_KEY") {
            if !k.is_empty() {
                cfg.api_key = Some(k);
            }
        }
        if let Ok(m) = env::var("SQLRITE_LLM_MODEL") {
            if !m.is_empty() {
                cfg.model = m;
            }
        }
        if let Ok(t) = env::var("SQLRITE_LLM_MAX_TOKENS") {
            cfg.max_tokens = t
                .parse()
                .map_err(|_| AskError::Config(format!("SQLRITE_LLM_MAX_TOKENS not a u32: {t}")))?;
        }
        if let Ok(c) = env::var("SQLRITE_LLM_CACHE_TTL") {
            cfg.cache_ttl = match c.to_ascii_lowercase().as_str() {
                "5m" | "5min" | "5minutes" => CacheTtl::FiveMinutes,
                "1h" | "1hr" | "1hour" => CacheTtl::OneHour,
                "off" | "none" | "disabled" => CacheTtl::Off,
                other => {
                    return Err(AskError::Config(format!(
                        "SQLRITE_LLM_CACHE_TTL: unknown value '{other}'"
                    )));
                }
            };
        }
        Ok(cfg)
    }
}

/// Errors `ask()` can return. Includes every failure mode along the
/// path: config / network / API / parsing.
#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("missing API key (set SQLRITE_LLM_API_KEY or AskConfig.api_key)")]
    MissingApiKey,

    #[error("config error: {0}")]
    Config(String),

    #[error("unknown provider: {0} (supported: anthropic)")]
    UnknownProvider(String),

    #[error("HTTP transport error: {0}")]
    Http(String),

    #[error("API returned status {status}: {detail}")]
    ApiStatus { status: u16, detail: String },

    #[error("API returned no text content")]
    EmptyResponse,

    #[error("model output not valid JSON: {0}")]
    OutputNotJson(String),

    #[error("model output JSON missing required field '{0}'")]
    OutputMissingField(&'static str),

    #[error("schema introspection failed: {0}")]
    Schema(String),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Engine(#[from] sqlrite::SQLRiteError),
}

/// Extension trait that adds [`ConnectionAskExt::ask`] to
/// [`sqlrite::Connection`]. Lives here (not on the engine) to keep the
/// engine free of HTTP / TLS / serde deps. Bring it into scope with
/// `use sqlrite_ask::ConnectionAskExt;`.
pub trait ConnectionAskExt {
    /// Generate SQL from a natural-language question. Equivalent to
    /// the free-function [`ask`] but reads as a method:
    ///
    /// ```no_run
    /// use sqlrite::Connection;
    /// use sqlrite_ask::{AskConfig, ConnectionAskExt};
    ///
    /// let conn = Connection::open("foo.sqlrite")?;
    /// let cfg = AskConfig::from_env()?;
    /// let resp = conn.ask("how many users are over 30?", &cfg)?;
    /// # Ok::<(), sqlrite_ask::AskError>(())
    /// ```
    fn ask(&self, question: &str, config: &AskConfig) -> Result<AskResponse, AskError>;
}

impl ConnectionAskExt for Connection {
    fn ask(&self, question: &str, config: &AskConfig) -> Result<AskResponse, AskError> {
        ask(self, question, config)
    }
}

/// One-shot natural-language → SQL.
///
/// Walks `conn`'s schema, builds a cache-friendly prompt, calls the
/// configured LLM, parses the JSON-shaped reply into [`AskResponse`].
///
/// The library does **not** execute the returned SQL — that's the
/// caller's call. See module docs for rationale.
pub fn ask(conn: &Connection, question: &str, config: &AskConfig) -> Result<AskResponse, AskError> {
    let api_key = config.api_key.clone().ok_or(AskError::MissingApiKey)?;

    let provider = match config.provider {
        ProviderKind::Anthropic => match &config.base_url {
            Some(url) => AnthropicProvider::with_base_url(api_key, url.clone()),
            None => AnthropicProvider::new(api_key),
        },
    };

    ask_with_provider(conn, question, config, &provider)
}

/// Lower-level entry point — same flow, but you supply the provider.
///
/// Used by the test suite (which passes a `MockProvider`) and by
/// advanced callers who want to drive a custom backend (an internal
/// LLM gateway, a recorded-replay test harness, etc.).
pub fn ask_with_provider<P: Provider>(
    conn: &Connection,
    question: &str,
    config: &AskConfig,
    provider: &P,
) -> Result<AskResponse, AskError> {
    let schema_dump = schema::dump_schema(conn);
    let system = build_system(&schema_dump, config.cache_ttl.into_marker());
    let messages = [UserMessage::new(question)];

    let req = ProviderRequest {
        model: &config.model,
        max_tokens: config.max_tokens,
        system: &system,
        messages: &messages,
    };

    let resp = provider.complete(req)?;
    parse_response(&resp.text, resp.usage)
}

/// Pull `sql` and `explanation` out of the model's reply.
///
/// We accept three shapes — strict JSON object, JSON wrapped in a
/// fenced code block, or "almost JSON" with leading/trailing prose —
/// because real LLM output drifts even with strict instructions. The
/// fence/prose tolerance matches what real callers do (better-sqlite3,
/// rusqlite, etc.) when interfacing with model output.
fn parse_response(raw: &str, usage: Usage) -> Result<AskResponse, AskError> {
    // 1. Strip markdown fences if the model wrapped its JSON.
    let trimmed = raw.trim();
    let body = strip_markdown_fence(trimmed).unwrap_or(trimmed);

    // 2. Try strict JSON first.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        return extract_fields(&value, usage);
    }

    // 3. Fallback: extract the first {...} block. Some models tack
    // prose like "Here is the SQL:" before the JSON despite the
    // prompt instruction. Find the first balanced object and try
    // parsing that.
    if let Some(json_block) = extract_first_json_object(body) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&json_block) {
            return extract_fields(&value, usage);
        }
    }

    Err(AskError::OutputNotJson(raw.to_string()))
}

fn extract_fields(value: &serde_json::Value, usage: Usage) -> Result<AskResponse, AskError> {
    let sql = value
        .get("sql")
        .and_then(|v| v.as_str())
        .ok_or(AskError::OutputMissingField("sql"))?
        .trim()
        .trim_end_matches(';')
        .to_string();
    let explanation = value
        .get("explanation")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(AskResponse {
        sql,
        explanation,
        usage,
    })
}

fn strip_markdown_fence(s: &str) -> Option<&str> {
    let s = s.trim();
    let opening_variants = ["```json\n", "```JSON\n", "```\n"];
    for opener in opening_variants {
        if let Some(rest) = s.strip_prefix(opener) {
            // Strip trailing ``` (with or without a final newline).
            let body = rest.trim_end();
            let body = body.strip_suffix("```").unwrap_or(body);
            return Some(body.trim());
        }
    }
    None
}

fn extract_first_json_object(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let start = s.find('{')?;
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if escape {
            escape = false;
            continue;
        }
        match b {
            b'\\' if in_string => escape = true,
            b'"' => in_string = !in_string,
            b'{' if !in_string => depth += 1,
            b'}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::MockProvider;
    use sqlrite::Connection;

    fn open() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    fn cfg() -> AskConfig {
        AskConfig {
            api_key: Some("test-key".to_string()),
            ..AskConfig::default()
        }
    }

    #[test]
    fn ask_with_mock_provider_returns_parsed_sql() {
        let mut conn = open();
        conn.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();

        let provider = MockProvider::new(
            r#"{"sql": "SELECT COUNT(*) FROM users", "explanation": "counts users"}"#,
        );

        let resp = ask_with_provider(&conn, "how many users?", &cfg(), &provider).unwrap();
        assert_eq!(resp.sql, "SELECT COUNT(*) FROM users");
        assert_eq!(resp.explanation, "counts users");
    }

    #[test]
    fn schema_dump_appears_in_system_block() {
        let mut conn = open();
        conn.execute("CREATE TABLE widgets (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        let provider = MockProvider::new(r#"{"sql": "", "explanation": ""}"#);
        let _ = ask_with_provider(&conn, "anything", &cfg(), &provider).unwrap();

        let captured = provider.last_request.borrow().clone().unwrap();
        let schema_block = &captured.system_blocks[1];
        assert!(
            schema_block.contains("CREATE TABLE widgets"),
            "got: {schema_block}"
        );
        assert!(schema_block.contains("name TEXT"), "got: {schema_block}");
    }

    #[test]
    fn cache_ttl_off_omits_cache_control() {
        let conn = open();
        let provider = MockProvider::new(r#"{"sql": "", "explanation": ""}"#);
        let mut config = cfg();
        config.cache_ttl = CacheTtl::Off;
        let _ = ask_with_provider(&conn, "test", &config, &provider).unwrap();
        let captured = provider.last_request.borrow().clone().unwrap();
        assert!(!captured.schema_block_has_cache_control);
    }

    #[test]
    fn cache_ttl_5m_sets_cache_control() {
        let conn = open();
        let provider = MockProvider::new(r#"{"sql": "", "explanation": ""}"#);
        let _ = ask_with_provider(&conn, "test", &cfg(), &provider).unwrap();
        let captured = provider.last_request.borrow().clone().unwrap();
        assert!(captured.schema_block_has_cache_control);
    }

    #[test]
    fn user_question_arrives_in_messages_unchanged() {
        let conn = open();
        let provider = MockProvider::new(r#"{"sql": "", "explanation": ""}"#);
        let q = "Find users with email containing '@example.com'";
        let _ = ask_with_provider(&conn, q, &cfg(), &provider).unwrap();
        assert_eq!(
            provider
                .last_request
                .borrow()
                .as_ref()
                .unwrap()
                .user_message,
            q
        );
    }

    #[test]
    fn missing_api_key_errors_clearly() {
        let conn = open();
        // Default has api_key: None already; just be explicit for the
        // reader.
        let config = AskConfig {
            api_key: None,
            ..AskConfig::default()
        };
        let err = ask(&conn, "test", &config).unwrap_err();
        match err {
            AskError::MissingApiKey => {}
            other => panic!("expected MissingApiKey, got {other:?}"),
        }
    }

    #[test]
    fn parse_response_strips_trailing_semicolon() {
        let resp = parse_response(
            r#"{"sql": "SELECT 1;", "explanation": "demo"}"#,
            Usage::default(),
        )
        .unwrap();
        assert_eq!(resp.sql, "SELECT 1");
    }

    #[test]
    fn parse_response_handles_markdown_fence() {
        let raw = "```json\n{\"sql\": \"SELECT 1\", \"explanation\": \"x\"}\n```";
        let resp = parse_response(raw, Usage::default()).unwrap();
        assert_eq!(resp.sql, "SELECT 1");
    }

    #[test]
    fn parse_response_handles_leading_prose() {
        let raw =
            "Here is the query you asked for:\n{\"sql\": \"SELECT 1\", \"explanation\": \"x\"}";
        let resp = parse_response(raw, Usage::default()).unwrap();
        assert_eq!(resp.sql, "SELECT 1");
    }

    #[test]
    fn parse_response_rejects_non_json() {
        let err = parse_response("just some prose, no JSON here", Usage::default()).unwrap_err();
        assert!(matches!(err, AskError::OutputNotJson(_)));
    }

    #[test]
    fn parse_response_rejects_missing_sql_field() {
        let err = parse_response(r#"{"explanation": "no sql key"}"#, Usage::default()).unwrap_err();
        assert!(matches!(err, AskError::OutputMissingField("sql")));
    }

    #[test]
    fn parse_response_allows_missing_explanation() {
        let resp = parse_response(r#"{"sql": "SELECT 1"}"#, Usage::default()).unwrap();
        assert_eq!(resp.sql, "SELECT 1");
        assert_eq!(resp.explanation, "");
    }

    #[test]
    fn parse_response_passes_usage_through() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 20,
            cache_creation_input_tokens: 80,
            cache_read_input_tokens: 0,
        };
        let resp =
            parse_response(r#"{"sql": "SELECT 1", "explanation": ""}"#, usage.clone()).unwrap();
        assert_eq!(resp.usage.input_tokens, 100);
        assert_eq!(resp.usage.cache_creation_input_tokens, 80);
    }
}
