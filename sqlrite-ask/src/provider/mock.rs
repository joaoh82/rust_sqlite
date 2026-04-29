//! In-process mock provider used by `lib.rs` tests.
//!
//! Lets us exercise the full `ask()` flow — schema introspection,
//! prompt construction, response parsing, error mapping — without
//! standing up an HTTP server. The `AnthropicProvider` itself has
//! its own integration test in `tests/anthropic_http.rs` against a
//! `tiny_http` localhost mock; this is the cheaper unit-test path.

use std::cell::RefCell;

use super::{Provider, Request, Response, Usage};
use crate::AskError;

/// Records the last request and replays a canned response. `RefCell`
/// gives interior mutability for `&self` `complete()`; we never share
/// these across threads.
pub(crate) struct MockProvider {
    pub last_request: RefCell<Option<CapturedRequest>>,
    pub canned: String,
    pub canned_usage: Usage,
}

#[derive(Clone)]
pub(crate) struct CapturedRequest {
    // model + max_tokens are recorded but not currently asserted on
    // by lib.rs unit tests — the integration test in
    // `tests/anthropic_http.rs` exercises those via the real wire
    // body. Kept on the struct so future tests can assert on them
    // without changing the mock surface.
    #[allow(dead_code)]
    pub model: String,
    #[allow(dead_code)]
    pub max_tokens: u32,
    pub system_blocks: Vec<String>,
    pub user_message: String,
    pub schema_block_has_cache_control: bool,
}

impl MockProvider {
    pub(crate) fn new(canned: impl Into<String>) -> Self {
        Self {
            last_request: RefCell::new(None),
            canned: canned.into(),
            canned_usage: Usage::default(),
        }
    }
}

impl Provider for MockProvider {
    fn complete(&self, req: Request<'_>) -> Result<Response, AskError> {
        let captured = CapturedRequest {
            model: req.model.to_string(),
            max_tokens: req.max_tokens,
            system_blocks: req.system.iter().map(|b| b.text.clone()).collect(),
            user_message: req
                .messages
                .first()
                .map(|m| m.content.clone())
                .unwrap_or_default(),
            schema_block_has_cache_control: req.system.iter().any(|b| b.cache_control.is_some()),
        };
        *self.last_request.borrow_mut() = Some(captured);
        Ok(Response {
            text: self.canned.clone(),
            usage: self.canned_usage.clone(),
        })
    }
}
