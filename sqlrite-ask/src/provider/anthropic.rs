//! Anthropic Messages API adapter.
//!
//! One sync POST to `https://api.anthropic.com/v1/messages` per
//! `complete()` call, JSON in / JSON out. Built on `ureq` for the
//! transport and `serde_json` for the bodies — no async runtime,
//! no SDK dependency.
//!
//! ## Auth + headers (current as of 2026-04, per the claude-api skill)
//!
//! ```text
//! x-api-key: <user's key>
//! anthropic-version: 2023-06-01
//! content-type: application/json
//! ```
//!
//! Prompt caching is GA on Claude 4.x — no `anthropic-beta` header
//! needed. The legacy `prompt-caching-2024-07-31` header is a no-op
//! today and we don't send it.
//!
//! ## What we do NOT support yet (and why it's fine)
//!
//! - **Streaming.** `ask()` is one-shot — caller waits for the full
//!   SQL string, then displays it. Streaming would complicate the
//!   sync return type for marginal UX gain on a small payload.
//! - **Tool use.** The model emits free-form text wrapped in JSON;
//!   the caller parses it. Adding tools (so the model could "call
//!   `run_query` directly") is a richer iteration that lives outside
//!   `sqlrite-ask` — it'd belong in `sqlrite-mcp` (Phase 7h).
//! - **Multi-turn.** Stateless. Conversational refinement is its own
//!   UX problem (see `docs/phase-7-plan.md` Q9-adjacent).

use serde::{Deserialize, Serialize};

use super::{Provider, Request, Response, Usage};
use crate::AskError;
use crate::prompt::{SystemBlock, UserMessage};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MESSAGES_PATH: &str = "/v1/messages";

/// Anthropic Messages API client. Stateless — one struct, many
/// `complete()` calls. The `agent` (ureq client) is reused across
/// calls so connection-pool / TLS-session-cache benefits accrue
/// when the same `AnthropicProvider` makes repeat calls.
pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    agent: ureq::Agent,
}

impl AnthropicProvider {
    /// Build a provider with the API key and the production endpoint.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    /// Build a provider pointing at an alternative base URL. The test
    /// suite uses this to point at a localhost mock; users could also
    /// use it for a corporate proxy or a regional Anthropic endpoint
    /// when those become available.
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            // Anthropic responses for short prompts complete well
            // under 30s. Long-form generation (rare for `ask()`,
            // since SQL output is typically <500 tokens) tops out
            // around 60s. 90s leaves headroom without making
            // genuinely-stuck calls hang forever.
            .timeout(std::time::Duration::from_secs(90))
            .build();
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            agent,
        }
    }
}

#[derive(Serialize)]
struct MessagesRequestBody<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a [SystemBlock],
    messages: &'a [UserMessage],
}

#[derive(Deserialize)]
struct MessagesResponseBody {
    content: Vec<ContentBlock>,
    #[serde(default)]
    usage: ResponseUsage,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize, Default)]
struct ResponseUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

#[derive(Deserialize)]
struct ApiErrorBody {
    error: ApiErrorInner,
}

#[derive(Deserialize)]
struct ApiErrorInner {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

impl Provider for AnthropicProvider {
    fn complete(&self, req: Request<'_>) -> Result<Response, AskError> {
        let body = MessagesRequestBody {
            model: req.model,
            max_tokens: req.max_tokens,
            system: req.system,
            messages: req.messages,
        };

        let url = format!("{}{}", self.base_url, MESSAGES_PATH);

        // ureq returns Err for 4xx/5xx; trap it so we can surface the
        // structured `error.message` from Anthropic's body rather than
        // the bare HTTP status. On a transport error (no response
        // body), fall back to the status text.
        let result = self
            .agent
            .post(&url)
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", ANTHROPIC_VERSION)
            .set("content-type", "application/json")
            .send_json(serde_json::to_value(&body).map_err(AskError::Json)?);

        let resp = match result {
            Ok(r) => r,
            Err(ureq::Error::Status(code, response)) => {
                let body_text = response
                    .into_string()
                    .unwrap_or_else(|_| "<unreadable response body>".to_string());
                let detail = serde_json::from_str::<ApiErrorBody>(&body_text)
                    .map(|e| format!("{}: {}", e.error.kind, e.error.message))
                    .unwrap_or_else(|_| body_text);
                return Err(AskError::ApiStatus {
                    status: code,
                    detail,
                });
            }
            Err(ureq::Error::Transport(t)) => {
                return Err(AskError::Http(t.to_string()));
            }
        };

        let parsed: MessagesResponseBody = resp
            .into_json()
            .map_err(|e| AskError::Http(e.to_string()))?;

        // Concatenate every text block in the response. With a
        // non-thinking, non-tool-use request like ours, there's
        // exactly one — but iterating future-proofs against
        // model upgrades that interleave thinking / refusal /
        // text blocks (see the claude-api skill notes on
        // `block.type == "text"` filtering).
        let text = parsed
            .content
            .iter()
            .filter(|b| b.kind == "text")
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("");

        if text.is_empty() {
            return Err(AskError::EmptyResponse);
        }

        Ok(Response {
            text,
            usage: Usage {
                input_tokens: parsed.usage.input_tokens,
                output_tokens: parsed.usage.output_tokens,
                cache_creation_input_tokens: parsed.usage.cache_creation_input_tokens,
                cache_read_input_tokens: parsed.usage.cache_read_input_tokens,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::{CacheControl, UserMessage, build_system};

    #[test]
    fn request_body_serializes_to_expected_shape() {
        // Catches any future field rename / casing slip — the
        // Anthropic API rejects unknown fields, so a typo here would
        // 400 every call.
        let system = build_system(
            "CREATE TABLE users (id INTEGER PRIMARY KEY);\n",
            Some(CacheControl::ephemeral()),
        );
        let messages = vec![UserMessage::new("count users")];
        let body = MessagesRequestBody {
            model: "claude-sonnet-4-6",
            max_tokens: 1024,
            system: &system,
            messages: &messages,
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["model"], "claude-sonnet-4-6");
        assert_eq!(json["max_tokens"], 1024);
        assert_eq!(json["system"][0]["type"], "text");
        assert_eq!(json["system"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "count users");
    }
}
