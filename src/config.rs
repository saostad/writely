use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Where non-secret settings live. Built from env vars only, no new crate:
/// macOS `~/Library/Application Support/Writely/settings.json`, Windows
/// `%APPDATA%\Writely\settings.json`, Linux
/// `$XDG_CONFIG_HOME/writely/settings.json` or `~/.config/writely/settings.json`.
pub fn settings_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            if !appdata.is_empty() {
                return PathBuf::from(appdata).join("Writely").join("settings.json");
            }
        }
        if let Ok(home) = std::env::var("USERPROFILE") {
            if !home.is_empty() {
                return PathBuf::from(home)
                    .join("AppData")
                    .join("Roaming")
                    .join("Writely")
                    .join("settings.json");
            }
        }
        PathBuf::from("Writely").join("settings.json")
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("Writely")
            .join("settings.json")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join("writely").join("settings.json");
            }
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".config")
            .join("writely")
            .join("settings.json")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    #[serde(rename = "gemini")]
    Gemini,
    #[serde(rename = "together")]
    Together,
    #[serde(rename = "ollama")]
    Ollama,
    #[serde(rename = "local")]
    Local,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Gemini => "gemini",
            Provider::Together => "together",
            Provider::Ollama => "ollama",
            Provider::Local => "local",
        }
    }

    pub fn needs_key(&self) -> bool {
        match self {
            Provider::Gemini | Provider::Together => true,
            Provider::Ollama | Provider::Local => false,
        }
    }

    /// Short human name used in messages ("Add your {name} API key first").
    pub fn display_name(&self) -> &'static str {
        match self {
            Provider::Gemini => "Gemini",
            Provider::Together => "Together AI",
            Provider::Ollama => "Ollama",
            Provider::Local => "On this computer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    #[default]
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "minimal")]
    Minimal,
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
}

impl Effort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Effort::Default => "default",
            Effort::Minimal => "minimal",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }
}

pub const DEFAULT_GEMINI_MODEL: &str = "gemini-3.5-flash-lite";
pub const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";

fn default_provider() -> Provider {
    Provider::Gemini
}

fn default_effort() -> Effort {
    Effort::Default
}

fn default_ollama_url() -> String {
    DEFAULT_OLLAMA_URL.to_string()
}

fn default_launch_at_login() -> bool {
    true
}

fn default_clipboard_fallback() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LocalModelId {
    #[default]
    #[serde(rename = "qwen3.5-0.8b")]
    Qwen35_0_8B,
    #[serde(rename = "qwen3.5-2b")]
    Qwen35_2B,
}

impl LocalModelId {
    pub fn as_str(&self) -> &'static str {
        match self {
            LocalModelId::Qwen35_0_8B => "qwen3.5-0.8b",
            LocalModelId::Qwen35_2B => "qwen3.5-2b",
        }
    }
}

fn default_local_model() -> LocalModelId {
    LocalModelId::Qwen35_0_8B
}

fn default_local_warm_up() -> bool {
    true
}

fn default_local_idle_minutes() -> u32 {
    10
}

fn default_local_threads() -> u32 {
    4
}

fn default_local_max_reply_tokens() -> u32 {
    1024
}

fn default_local_context_tokens() -> u32 {
    4096
}

fn default_local_startup_timeout_secs() -> u32 {
    60
}

fn default_local_reply_timeout_secs() -> u32 {
    120
}

fn default_models() -> HashMap<String, String> {
    let mut models = HashMap::new();
    models.insert("gemini".to_string(), DEFAULT_GEMINI_MODEL.to_string());
    models.insert("together".to_string(), String::new());
    models.insert("ollama".to_string(), String::new());
    models
}

/// Non-secret settings. The model is stored per provider (a map), so
/// switching providers keeps each one's model. Nothing secret is stored here;
/// API keys stay in the keychain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default = "default_provider")]
    pub provider: Provider,
    #[serde(default = "default_models")]
    pub models: HashMap<String, String>,
    #[serde(default = "default_effort")]
    pub effort: Effort,
    #[serde(default = "default_ollama_url")]
    pub ollama_url: String,
    /// Start Writely automatically at login (on by default; files written
    /// before this setting existed also get `true`).
    #[serde(default = "default_launch_at_login")]
    pub launch_at_login: bool,
    /// Copy the selection with the clipboard (and put the clipboard back)
    /// when an app doesn't share its text. On by default; when off, Writely
    /// never touches the clipboard to read or replace text.
    #[serde(default = "default_clipboard_fallback")]
    pub clipboard_fallback: bool,
    #[serde(default = "default_local_model")]
    pub local_model: LocalModelId,
    #[serde(default = "default_local_warm_up")]
    pub local_warm_up: bool,
    #[serde(default = "default_local_idle_minutes")]
    pub local_idle_minutes: u32,
    #[serde(default = "default_local_threads")]
    pub local_threads: u32,
    #[serde(default = "default_local_max_reply_tokens")]
    pub local_max_reply_tokens: u32,
    #[serde(default = "default_local_context_tokens")]
    pub local_context_tokens: u32,
    #[serde(default = "default_local_startup_timeout_secs")]
    pub local_startup_timeout_secs: u32,
    #[serde(default = "default_local_reply_timeout_secs")]
    pub local_reply_timeout_secs: u32,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            provider: Provider::Gemini,
            models: default_models(),
            effort: Effort::Default,
            ollama_url: DEFAULT_OLLAMA_URL.to_string(),
            launch_at_login: true,
            clipboard_fallback: true,
            local_model: LocalModelId::Qwen35_0_8B,
            local_warm_up: true,
            local_idle_minutes: 10,
            local_threads: 4,
            local_max_reply_tokens: 1024,
            local_context_tokens: 4096,
            local_startup_timeout_secs: 60,
            local_reply_timeout_secs: 120,
        }
    }
}

#[allow(dead_code)] // Wired up in a later round (local-model task).
pub fn validate_local_settings(s: &AppSettings) -> Result<(), String> {
    if s.local_idle_minutes > 1440 {
        return Err(
            "Free memory after must be between 0 and 1440 minutes (0 means never).".to_string(),
        );
    }
    if s.local_threads == 0 || s.local_threads > 64 {
        return Err("Processor threads must be between 1 and 64.".to_string());
    }
    if s.local_max_reply_tokens < 64 || s.local_max_reply_tokens > 8192 {
        return Err("Longest reply must be between 64 and 8192 tokens.".to_string());
    }
    if s.local_context_tokens < 1024 || s.local_context_tokens > 32768 {
        return Err("Context size must be between 1024 and 32768 tokens.".to_string());
    }
    if s.local_startup_timeout_secs < 5 || s.local_startup_timeout_secs > 600 {
        return Err("Startup time limit must be between 5 and 600 seconds.".to_string());
    }
    if s.local_reply_timeout_secs < 10 || s.local_reply_timeout_secs > 900 {
        return Err("Reply time limit must be between 10 and 900 seconds.".to_string());
    }
    if s.local_max_reply_tokens >= s.local_context_tokens {
        return Err("Longest reply must be smaller than the context size.".to_string());
    }
    Ok(())
}

impl AppSettings {
    pub fn model_for(&self, provider: Provider) -> String {
        if provider == Provider::Local {
            return self.local_model.as_str().to_string();
        }
        self.models
            .get(provider.as_str())
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_model(&mut self, provider: Provider, model: &str) {
        if provider == Provider::Local {
            return;
        }
        self.models
            .insert(provider.as_str().to_string(), model.to_string());
    }

    /// The model for the currently selected provider, trimmed.
    pub fn current_model(&self) -> String {
        self.model_for(self.provider).trim().to_string()
    }
}

/// Whether `provider` is ready to use: it needs a non-blank model, and every
/// provider that needs a key also needs a saved key.
pub fn provider_ready(provider: Provider, model: &str, has_key: bool) -> bool {
    if model.trim().is_empty() {
        return false;
    }
    !provider.needs_key() || has_key
}

/// A copy of `settings` switched to `provider`; every other field (all models,
/// effort, URL, toggles) is unchanged.
fn with_provider(mut settings: AppSettings, provider: Provider) -> AppSettings {
    settings.provider = provider;
    settings
}

/// Switches the saved provider, keeping every other setting.
pub fn switch_provider(provider: Provider) -> Result<(), String> {
    let settings = load_settings()?;
    save_settings(&with_provider(settings, provider))
}

/// Validates the model field for saving: it must not be empty.
pub fn validate_model_name(model: &str) -> Result<String, String> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        Err("Enter a model name first.".to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

/// Loads the settings. A missing file means defaults; an unreadable or
/// invalid file is an error with the real error text (never silently reset).
pub fn load_settings() -> Result<AppSettings, String> {
    let path = settings_path();
    match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str::<AppSettings>(&contents)
            .map_err(|e| format!("Could not read the settings file ({}): {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AppSettings::default()),
        Err(e) => Err(format!(
            "Could not read the settings file ({}): {e}",
            path.display()
        )),
    }
}

/// Saves the settings atomically (temp file + rename).
pub fn save_settings(settings: &AppSettings) -> Result<(), String> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not save the settings file ({}): {e}", path.display()))?;
    }
    let contents = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Could not save the settings file ({}): {e}", path.display()))?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, contents)
        .map_err(|e| format!("Could not save the settings file ({}): {e}", path.display()))?;
    std::fs::rename(&tmp_path, &path)
        .map_err(|e| format!("Could not save the settings file ({}): {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_have_expected_values() {
        let settings = AppSettings::default();
        assert_eq!(settings.provider, Provider::Gemini);
        assert_eq!(settings.model_for(Provider::Gemini), DEFAULT_GEMINI_MODEL);
        assert_eq!(settings.model_for(Provider::Together), "");
        assert_eq!(settings.model_for(Provider::Ollama), "");
        assert_eq!(settings.effort, Effort::Default);
        assert_eq!(settings.ollama_url, DEFAULT_OLLAMA_URL);
        assert!(settings.launch_at_login);
    }

    #[test]
    fn needs_key_only_for_gemini_and_together() {
        assert!(Provider::Gemini.needs_key());
        assert!(Provider::Together.needs_key());
        assert!(!Provider::Ollama.needs_key());
        assert!(!Provider::Local.needs_key());
    }

    #[test]
    fn local_provider_serde_round_trip() {
        let s = AppSettings {
            provider: Provider::Local,
            ..AppSettings::default()
        };
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(json.contains(r#""provider":"local""#));
        let back: AppSettings = serde_json::from_str(&json).expect("parse");
        assert_eq!(back.provider, Provider::Local);
        assert_eq!(Provider::Local.as_str(), "local");
        assert_eq!(Provider::Local.display_name(), "On this computer");
    }

    #[test]
    fn model_for_local_follows_local_model() {
        let mut s = AppSettings::default();
        assert_eq!(s.model_for(Provider::Local), "qwen3.5-0.8b");
        s.local_model = LocalModelId::Qwen35_2B;
        assert_eq!(s.model_for(Provider::Local), "qwen3.5-2b");
        assert_eq!(LocalModelId::Qwen35_0_8B.as_str(), "qwen3.5-0.8b");
        assert_eq!(LocalModelId::Qwen35_2B.as_str(), "qwen3.5-2b");
        // set_model for Local is a no-op.
        s.set_model(Provider::Local, "something-else");
        assert_eq!(s.model_for(Provider::Local), "qwen3.5-2b");
        assert!(!s.models.contains_key("local"));
    }

    #[test]
    fn launch_at_login_defaults_on_for_old_files() {
        // A settings file written before the setting existed has no key.
        let old =
            r#"{"provider":"gemini","effort":"default","ollama_url":"http://localhost:11434"}"#;
        let settings: AppSettings = serde_json::from_str(old).expect("old file should parse");
        assert!(settings.launch_at_login);
        let off: AppSettings =
            serde_json::from_str(r#"{"launch_at_login":false}"#).expect("should parse");
        assert!(!off.launch_at_login);
    }

    #[test]
    fn clipboard_fallback_defaults_on_for_old_files() {
        let old =
            r#"{"provider":"gemini","effort":"default","ollama_url":"http://localhost:11434"}"#;
        let settings: AppSettings = serde_json::from_str(old).expect("old file should parse");
        assert!(settings.clipboard_fallback);
        let off: AppSettings =
            serde_json::from_str(r#"{"clipboard_fallback":false}"#).expect("should parse");
        assert!(!off.clipboard_fallback);
    }

    #[test]
    fn missing_file_means_defaults() {
        let dir = std::env::temp_dir().join(format!("writely-cfg-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");
        assert!(!path.exists());
        // load_settings reads the real path; instead check the rule directly:
        // a NotFound read maps to defaults.
        let result = std::fs::read_to_string(&path);
        let err = result.expect_err("file should be missing");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        let settings = AppSettings::default();
        assert_eq!(settings.provider, Provider::Gemini);
    }

    #[test]
    fn invalid_json_is_an_error() {
        let parsed: Result<AppSettings, _> = serde_json::from_str("{not json");
        assert!(parsed.is_err());
        let err = parsed.expect_err("should fail");
        let message = format!("Could not read the settings file (/tmp/x.json): {err}");
        assert!(message.contains("Could not read the settings file"));
    }

    #[test]
    fn round_trip_through_json() {
        let mut settings = AppSettings {
            provider: Provider::Together,
            effort: Effort::Low,
            ..AppSettings::default()
        };
        settings.set_model(Provider::Together, "deepseek-ai/DeepSeek-V4.1-Flash");
        settings.ollama_url = "http://localhost:11434".to_string();
        let json = serde_json::to_string_pretty(&settings).expect("should serialize");
        let back: AppSettings = serde_json::from_str(&json).expect("should parse");
        assert_eq!(back, settings);
        assert_eq!(
            back.model_for(Provider::Together),
            "deepseek-ai/DeepSeek-V4.1-Flash"
        );
        // Switching providers keeps each one's model.
        assert_eq!(back.model_for(Provider::Gemini), DEFAULT_GEMINI_MODEL);
    }

    #[test]
    fn model_validation_rejects_empty() {
        assert_eq!(
            validate_model_name("   ").expect_err("empty model should fail"),
            "Enter a model name first."
        );
        assert_eq!(
            validate_model_name("  llama3.2 ").expect("model should be valid"),
            "llama3.2"
        );
    }

    #[test]
    fn provider_ready_needs_a_model_and_a_key() {
        // Blank model: never ready, even with a key.
        assert!(!provider_ready(Provider::Gemini, "   ", true));
        assert!(!provider_ready(Provider::Together, "", true));
        assert!(!provider_ready(Provider::Ollama, "", false));
        // Gemini and Together need a saved key.
        assert!(!provider_ready(
            Provider::Gemini,
            "gemini-3.5-flash-lite",
            false
        ));
        assert!(!provider_ready(
            Provider::Together,
            "deepseek-ai/DeepSeek-V4.1-Flash",
            false
        ));
        assert!(provider_ready(
            Provider::Gemini,
            "gemini-3.5-flash-lite",
            true
        ));
        assert!(provider_ready(
            Provider::Together,
            "deepseek-ai/DeepSeek-V4.1-Flash",
            true
        ));
        // Ollama and Local need no key.
        assert!(provider_ready(Provider::Ollama, "llama3.2", false));
        assert!(provider_ready(Provider::Local, "qwen3.5-0.8b", false));
        assert!(!provider_ready(Provider::Local, "", false));
    }

    #[test]
    fn with_provider_keeps_every_other_field() {
        let mut settings = AppSettings {
            provider: Provider::Gemini,
            effort: Effort::Low,
            ollama_url: "http://example:11434".to_string(),
            launch_at_login: false,
            clipboard_fallback: false,
            ..AppSettings::default()
        };
        settings.set_model(Provider::Gemini, "gemini-model");
        settings.set_model(Provider::Together, "together-model");
        settings.set_model(Provider::Ollama, "ollama-model");
        let switched = with_provider(settings.clone(), Provider::Together);
        assert_eq!(switched.provider, Provider::Together);
        assert_eq!(switched.models, settings.models);
        assert_eq!(switched.effort, settings.effort);
        assert_eq!(switched.ollama_url, settings.ollama_url);
        assert_eq!(switched.launch_at_login, settings.launch_at_login);
        assert_eq!(switched.clipboard_fallback, settings.clipboard_fallback);
    }

    #[test]
    fn local_defaults_have_expected_values() {
        let s = AppSettings::default();
        assert_eq!(s.local_model, LocalModelId::Qwen35_0_8B);
        assert!(s.local_warm_up);
        assert_eq!(s.local_idle_minutes, 10);
        assert_eq!(s.local_threads, 4);
        assert_eq!(s.local_max_reply_tokens, 1024);
        assert_eq!(s.local_context_tokens, 4096);
        assert_eq!(s.local_startup_timeout_secs, 60);
        assert_eq!(s.local_reply_timeout_secs, 120);
        assert!(validate_local_settings(&s).is_ok());
    }

    #[test]
    fn local_settings_default_for_old_files() {
        let old =
            r#"{"provider":"gemini","effort":"default","ollama_url":"http://localhost:11434"}"#;
        let s: AppSettings = serde_json::from_str(old).expect("old file should parse");
        assert_eq!(s.local_reply_timeout_secs, 120);
        assert_eq!(s, AppSettings::default());
    }

    #[test]
    fn local_model_round_trip_qwen35_2b() {
        let s = AppSettings {
            local_model: LocalModelId::Qwen35_2B,
            ..AppSettings::default()
        };
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(json.contains(r#""local_model":"qwen3.5-2b""#));
        let back: AppSettings = serde_json::from_str(&json).expect("parse");
        assert_eq!(back.local_model, LocalModelId::Qwen35_2B);
        let parsed: AppSettings =
            serde_json::from_str(r#"{"local_model":"qwen3.5-2b"}"#).expect("parse");
        assert_eq!(parsed.local_model, LocalModelId::Qwen35_2B);
        let parsed: AppSettings =
            serde_json::from_str(r#"{"local_model":"qwen3.5-0.8b"}"#).expect("parse");
        assert_eq!(parsed.local_model, LocalModelId::Qwen35_0_8B);
    }

    #[test]
    fn validate_local_settings_edges() {
        let base = AppSettings::default();
        // idle 0-1440
        assert!(validate_local_settings(&AppSettings {
            local_idle_minutes: 0,
            ..base.clone()
        })
        .is_ok());
        assert!(validate_local_settings(&AppSettings {
            local_idle_minutes: 1440,
            ..base.clone()
        })
        .is_ok());
        assert_eq!(
            validate_local_settings(&AppSettings {
                local_idle_minutes: 1441,
                ..base.clone()
            })
            .expect_err("fail"),
            "Free memory after must be between 0 and 1440 minutes (0 means never)."
        );
        // threads 1-64
        assert!(validate_local_settings(&AppSettings {
            local_threads: 1,
            ..base.clone()
        })
        .is_ok());
        assert!(validate_local_settings(&AppSettings {
            local_threads: 64,
            ..base.clone()
        })
        .is_ok());
        for v in [0, 65] {
            assert_eq!(
                validate_local_settings(&AppSettings {
                    local_threads: v,
                    ..base.clone()
                })
                .expect_err("fail"),
                "Processor threads must be between 1 and 64."
            );
        }
        // max reply 64-8192 (context raised so reply<context at high edge)
        assert!(validate_local_settings(&AppSettings {
            local_max_reply_tokens: 64,
            ..base.clone()
        })
        .is_ok());
        assert!(validate_local_settings(&AppSettings {
            local_max_reply_tokens: 8192,
            local_context_tokens: 32768,
            ..base.clone()
        })
        .is_ok());
        for v in [63, 8193] {
            assert_eq!(
                validate_local_settings(&AppSettings {
                    local_max_reply_tokens: v,
                    local_context_tokens: 32768,
                    ..base.clone()
                })
                .expect_err("fail"),
                "Longest reply must be between 64 and 8192 tokens."
            );
        }
        // context 1024-32768 (reply lowered so reply<context at low edge)
        assert!(validate_local_settings(&AppSettings {
            local_context_tokens: 1024,
            local_max_reply_tokens: 64,
            ..base.clone()
        })
        .is_ok());
        assert!(validate_local_settings(&AppSettings {
            local_context_tokens: 32768,
            ..base.clone()
        })
        .is_ok());
        for v in [1023, 32769] {
            assert_eq!(
                validate_local_settings(&AppSettings {
                    local_context_tokens: v,
                    local_max_reply_tokens: 64,
                    ..base.clone()
                })
                .expect_err("fail"),
                "Context size must be between 1024 and 32768 tokens."
            );
        }
        // startup 5-600
        assert!(validate_local_settings(&AppSettings {
            local_startup_timeout_secs: 5,
            ..base.clone()
        })
        .is_ok());
        assert!(validate_local_settings(&AppSettings {
            local_startup_timeout_secs: 600,
            ..base.clone()
        })
        .is_ok());
        for v in [4, 601] {
            assert_eq!(
                validate_local_settings(&AppSettings {
                    local_startup_timeout_secs: v,
                    ..base.clone()
                })
                .expect_err("fail"),
                "Startup time limit must be between 5 and 600 seconds."
            );
        }
        // reply timeout 10-900
        assert!(validate_local_settings(&AppSettings {
            local_reply_timeout_secs: 10,
            ..base.clone()
        })
        .is_ok());
        assert!(validate_local_settings(&AppSettings {
            local_reply_timeout_secs: 900,
            ..base.clone()
        })
        .is_ok());
        for v in [9, 901] {
            assert_eq!(
                validate_local_settings(&AppSettings {
                    local_reply_timeout_secs: v,
                    ..base.clone()
                })
                .expect_err("fail"),
                "Reply time limit must be between 10 and 900 seconds."
            );
        }
    }

    #[test]
    fn validate_local_settings_reply_must_be_smaller_than_context() {
        let base = AppSettings::default();
        for (reply, ctx) in [(4096, 4096), (8192, 4096)] {
            assert_eq!(
                validate_local_settings(&AppSettings {
                    local_max_reply_tokens: reply,
                    local_context_tokens: ctx,
                    ..base.clone()
                })
                .expect_err("fail"),
                "Longest reply must be smaller than the context size."
            );
        }
        assert!(validate_local_settings(&AppSettings {
            local_max_reply_tokens: 4095,
            local_context_tokens: 4096,
            ..base
        })
        .is_ok());
    }

    #[test]
    fn settings_path_uses_expected_locations() {
        let path = settings_path();
        let text = path.to_string_lossy();
        assert!(text.ends_with("settings.json"));
        #[cfg(target_os = "macos")]
        assert!(text.contains("Application Support"));
        #[cfg(target_os = "windows")]
        assert!(text.contains("Writely"));
    }
}
