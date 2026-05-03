//! ASCII tokenizer for FTS — splits on `[^A-Za-z0-9]+` and lowercases.
//!
//! Resolves Phase 8 plan Q3 (ASCII MVP). Unicode-aware tokenization is
//! deferred to Phase 8.1 behind a `unicode` cargo feature; the limitation
//! here is intentional. Non-ASCII bytes are treated as separators, which
//! means accented Latin (`café`), CJK, and other non-ASCII scripts won't
//! be searchable until that follow-up lands.
//!
//! No stemming and no stop-word removal (Q4 + Q5). BM25's IDF naturally
//! downweights common terms, and modern RAG pipelines rely on exact
//! lexical matches for technical retrieval.

/// Split `text` on runs of non-ASCII-alphanumeric bytes and lowercase
/// each resulting term. Empty input or input made entirely of separators
/// returns an empty `Vec`.
///
/// Tokens are `String` rather than `&str` because the posting-list owns
/// its term strings (see [`super::posting_list::PostingList`]); returning
/// owned strings keeps the call site shape consistent with how the index
/// stores them and avoids a second allocation downstream.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() {
            current.push(b.to_ascii_lowercase() as char);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_empty_vec() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("   ").is_empty());
        assert!(tokenize("!!!---???").is_empty());
    }

    #[test]
    fn splits_on_punctuation_and_whitespace() {
        assert_eq!(
            tokenize("hello, world!"),
            vec!["hello".to_string(), "world".to_string()]
        );
        assert_eq!(
            tokenize("a\tb\nc d"),
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string()
            ]
        );
    }

    #[test]
    fn lowercases_ascii_letters() {
        assert_eq!(
            tokenize("FooBar BAZ"),
            vec!["foobar".to_string(), "baz".to_string()]
        );
    }

    #[test]
    fn alphanumeric_runs_stay_together() {
        // "rust2026" is a single token; digits are alphanumeric.
        assert_eq!(tokenize("rust2026"), vec!["rust2026".to_string()]);
        // "co-op" splits on the hyphen.
        assert_eq!(tokenize("co-op"), vec!["co".to_string(), "op".to_string()]);
    }

    #[test]
    fn non_ascii_bytes_act_as_separators_without_panicking() {
        // ASCII MVP per Q3 — non-ASCII bytes (é = 0xC3 0xA9 in UTF-8) are
        // treated as separators. "café" -> ["caf"]. Documented limitation.
        let toks = tokenize("café");
        assert_eq!(toks, vec!["caf".to_string()]);
        // CJK input: every byte is non-ASCII, so we get an empty result.
        assert!(tokenize("日本語").is_empty());
    }

    #[test]
    fn smoke_module_path_reaches_through_lib() {
        // Confirms `sqlrite::sql::fts::tokenize` is reachable via the
        // public `sql` module path from the crate root. If 8b ever moves
        // the module behind a feature gate, this test will fail loudly.
        assert_eq!(
            crate::sql::fts::tokenize("Hello, world!"),
            vec!["hello".to_string(), "world".to_string()]
        );
    }
}
