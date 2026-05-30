//! In-app settings for the `Ask…` panel.
//!
//! Lives at `$APP_DATA/com.sqlrite.desktop/settings.json` — under the
//! app's data directory, on its own so the key never rides along if the
//! user copies or syncs a `.sqlrite` file they opened in the playground.
//!
//! **Why this exists.** The playground used to read its `ask` config
//! exclusively from `SQLRITE_LLM_API_KEY` in the environment. That's
//! hostile for a GUI: launch the app from Finder / the Dock and the env
//! var isn't inherited, so the **Ask…** button just errors. Persisting
//! the key in-app fixes that while keeping the env var as a fallback for
//! existing dev workflows.
//!
//! **Security note (documented in README too).** The settings file is
//! plain JSON on disk. That's the right tradeoff for an example app:
//! better UX than an env var, no OS-keychain plugin dep. A production
//! desktop app shipping the same pattern should reach for
//! `tauri-plugin-keyring` / `keyring-rs` instead.
//!
//! The webview never receives the API key. `get_ask_settings` returns
//! `has_api_key: bool` (so the UI can show "configured" vs "not set")
//! but the raw key value only crosses the IPC boundary in one
//! direction: webview → Rust on `update_ask_settings`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What gets persisted to `settings.json`. Every field is optional so
/// older settings files still parse after we add knobs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AskSettings {
    /// Anthropic API key. None = unset. Empty string in a saved file
    /// is normalised to None on load.
    #[serde(default)]
    pub anthropic_api_key: Option<String>,
    /// Override the engine default model if set.
    #[serde(default)]
    pub model: Option<String>,
    /// Override the engine's default max tokens.
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

/// What we send to the webview. Notice the api key is replaced by a
/// boolean — the value itself stays in the Rust side.
#[derive(Debug, Clone, Serialize)]
pub struct AskSettingsDto {
    pub has_api_key: bool,
    pub model: String,
    pub max_tokens: u32,
    /// Whether `SQLRITE_LLM_API_KEY` is set in the parent shell; the
    /// UI uses this to explain "your settings file is empty but the
    /// env var is providing the key" without having to dig.
    pub env_api_key_present: bool,
}

const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_MAX_TOKENS: u32 = 1024;
const ENV_KEY: &str = "SQLRITE_LLM_API_KEY";

impl AskSettings {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let mut parsed: AskSettings = serde_json::from_str(&s).unwrap_or_default();
                // Normalise: an empty-string api key on disk is
                // equivalent to "not set".
                if parsed.anthropic_api_key.as_deref() == Some("") {
                    parsed.anthropic_api_key = None;
                }
                parsed
            }
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, body)
    }

    pub fn to_dto(&self) -> AskSettingsDto {
        AskSettingsDto {
            has_api_key: self
                .anthropic_api_key
                .as_ref()
                .map(|k| !k.is_empty())
                .unwrap_or(false),
            model: self
                .model
                .clone()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            max_tokens: self
                .max_tokens
                .filter(|t| *t > 0)
                .unwrap_or(DEFAULT_MAX_TOKENS),
            env_api_key_present: std::env::var(ENV_KEY)
                .map(|v| !v.is_empty())
                .unwrap_or(false),
        }
    }
}

/// What `update_ask_settings` accepts.
///
/// Three-valued semantics per field:
///   * `Some(non_empty)` → set
///   * `Some("")` → clear
///   * `None`     → leave untouched
#[derive(Debug, Deserialize)]
pub struct AskSettingsUpdate {
    #[serde(default)]
    pub anthropic_api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

impl AskSettings {
    pub fn apply_update(&mut self, update: AskSettingsUpdate) {
        if let Some(k) = update.anthropic_api_key {
            self.anthropic_api_key = if k.is_empty() { None } else { Some(k) };
        }
        if let Some(m) = update.model {
            self.model = if m.is_empty() { None } else { Some(m) };
        }
        if let Some(t) = update.max_tokens {
            self.max_tokens = if t == 0 { None } else { Some(t) };
        }
    }
}

/// Returns the canonical settings file path inside the resolved
/// app-data directory. The directory may not exist yet — callers
/// using [`AskSettings::save`] handle that.
pub fn settings_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("settings.json")
}

// ----- ask config plumbing --------------------------------------------

/// Builds an [`AskConfig`] for one `ask_sql` call. Prefers the saved
/// settings; falls back to `SQLRITE_LLM_API_KEY` from the environment
/// when nothing is saved, so existing dev workflows keep working.
///
/// The desktop crate always builds the engine with its `ask` feature
/// on, so this is unconditional (no `cfg(feature = "ask")` gate).
pub fn build_ask_config(settings: &AskSettings) -> Result<sqlrite::ask::AskConfig, String> {
    use sqlrite::ask::AskConfig;
    let api_key = settings
        .anthropic_api_key
        .clone()
        .filter(|k| !k.is_empty())
        .or_else(|| std::env::var(ENV_KEY).ok().filter(|k| !k.is_empty()));
    let cfg = AskConfig {
        api_key,
        model: settings
            .model
            .clone()
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| AskConfig::default().model),
        max_tokens: settings
            .max_tokens
            .filter(|t| *t > 0)
            .unwrap_or(DEFAULT_MAX_TOKENS),
        ..AskConfig::default()
    };
    if cfg.api_key.is_none() {
        return Err(format!(
            "No Anthropic API key configured. Open Settings (gear icon) and \
             paste a key from console.anthropic.com — or set {ENV_KEY} in \
             the shell that launched the app."
        ));
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_key_normalises_to_none() {
        // Round-trip an empty-string saved key through load() and
        // confirm it lands as None — that's the contract callers rely
        // on so the "configured" indicator stays accurate.
        let dir = tempdir();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"anthropic_api_key": ""}"#).unwrap();
        let s = AskSettings::load(&path);
        assert!(s.anthropic_api_key.is_none());
    }

    #[test]
    fn update_three_valued_semantics() {
        let mut s = AskSettings {
            anthropic_api_key: Some("old".into()),
            model: Some("custom-model".into()),
            max_tokens: Some(512),
        };
        // None → unchanged
        s.apply_update(AskSettingsUpdate {
            anthropic_api_key: None,
            model: None,
            max_tokens: None,
        });
        assert_eq!(s.anthropic_api_key.as_deref(), Some("old"));
        assert_eq!(s.model.as_deref(), Some("custom-model"));
        // Some("") → clear
        s.apply_update(AskSettingsUpdate {
            anthropic_api_key: Some(String::new()),
            model: Some(String::new()),
            max_tokens: Some(0),
        });
        assert!(s.anthropic_api_key.is_none());
        assert!(s.model.is_none());
        assert!(s.max_tokens.is_none());
        // Some(value) → set
        s.apply_update(AskSettingsUpdate {
            anthropic_api_key: Some("new".into()),
            model: Some("haiku".into()),
            max_tokens: Some(2048),
        });
        assert_eq!(s.anthropic_api_key.as_deref(), Some("new"));
        assert_eq!(s.model.as_deref(), Some("haiku"));
        assert_eq!(s.max_tokens, Some(2048));
    }

    #[test]
    fn dto_hides_api_key_value() {
        let s = AskSettings {
            anthropic_api_key: Some("sk-ant-secret".into()),
            model: None,
            max_tokens: None,
        };
        let dto = s.to_dto();
        assert!(dto.has_api_key);
        // The DTO doesn't have a field that would carry the secret —
        // this assertion is a compile-time guarantee that future
        // refactors keep that contract.
        let serialized = serde_json::to_string(&dto).unwrap();
        assert!(!serialized.contains("sk-ant-secret"));
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir();
        let path = dir.join("settings.json");
        let s = AskSettings {
            anthropic_api_key: Some("sk-ant-xyz".into()),
            model: Some("opus".into()),
            max_tokens: Some(2048),
        };
        s.save(&path).unwrap();
        let loaded = AskSettings::load(&path);
        assert_eq!(loaded.anthropic_api_key.as_deref(), Some("sk-ant-xyz"));
        assert_eq!(loaded.model.as_deref(), Some("opus"));
        assert_eq!(loaded.max_tokens, Some(2048));
    }

    #[test]
    fn build_ask_config_prefers_settings_over_env() {
        // SAFETY: this test mutates global process env, which can
        // race with parallel tests reading the same var. The Rust
        // test harness runs unit tests in parallel within a crate, so
        // we use a known-unique value and don't assert the env var's
        // post-state.
        use std::env;
        unsafe {
            env::set_var(ENV_KEY, "env-value");
        }
        let s = AskSettings {
            anthropic_api_key: Some("settings-value".into()),
            ..Default::default()
        };
        let cfg = build_ask_config(&s).unwrap();
        assert_eq!(cfg.api_key.as_deref(), Some("settings-value"));
        unsafe {
            env::remove_var(ENV_KEY);
        }
    }

    fn tempdir() -> std::path::PathBuf {
        // A process-wide atomic counter guarantees a unique directory
        // per call even when the test harness runs these in parallel —
        // a timestamp alone can collide on its sub-second fraction and
        // let two tests stomp on the same settings.json.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        p.push(format!("sqlrite-desktop-test-{pid}-{n}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
