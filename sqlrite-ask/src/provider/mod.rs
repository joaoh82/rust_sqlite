//! LLM provider abstraction.
//!
//! `Provider` is the trait every backend implements. Today there's one
//! production impl ([`anthropic::AnthropicProvider`]) and one test impl
//! ([`MockProvider`]). OpenAI and Ollama follow-ups will plug in here
//! without touching the rest of the crate (per Phase 7 plan Q4 —
//! Anthropic-first, others later).
//!
//! The trait is deliberately narrow: one method, sync, one prompt
//! shape in, one parsed response out. Schema-aware prompt construction
//! lives one layer up in `crate::prompt`, so providers stay generic
//! over what's being asked.

use crate::AskError;
use crate::prompt::{SystemBlock, UserMessage};

pub mod anthropic;

#[cfg(test)]
mod mock;
#[cfg(test)]
pub(crate) use mock::MockProvider;

/// One LLM call's worth of input. Mirrors the Anthropic Messages
/// request shape because it's the most expressive of the three
/// providers we'll support; OpenAI and Ollama adapters convert to
/// their native shapes inside their own `complete` impls.
pub struct Request<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub system: &'a [SystemBlock],
    pub messages: &'a [UserMessage],
}

/// What every provider returns. We keep this minimal — `text` is the
/// raw string the model produced (the caller parses it), `usage`
/// surfaces token counts so callers can verify cache hits.
pub struct Response {
    /// The raw text content of the assistant's reply. Caller is
    /// responsible for JSON-parsing it (per the prompt template, this
    /// will be `{"sql": "...", "explanation": "..."}` on success).
    pub text: String,
    pub usage: Usage,
}

/// Token-usage breakdown. Names match Anthropic's API field names so
/// the mapping stays obvious; OpenAI's `prompt_tokens` /
/// `completion_tokens` will fan into `input_tokens` / `output_tokens`
/// when that adapter lands.
///
/// **Verifying cache hits:** if `cache_read_input_tokens` is zero
/// across repeated `ask()` calls with the same schema, something in
/// the prefix is invalidating the cache (a silent invalidator —
/// `datetime.now()` in a system block, varying tool list, etc.).
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

/// A single one-shot call. Sync because every supported provider has
/// a sync HTTPS entry point and `ask()` itself is sync (matches the
/// engine's surface — `Connection::execute` etc. are all sync).
pub trait Provider {
    fn complete(&self, req: Request<'_>) -> Result<Response, AskError>;
}
