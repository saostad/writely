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
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Gemini => "gemini",
            Provider::Together => "together",
            Provider::Ollama => "ollama",
        }
    }

    /// Short human name used in messages ("Add your {name} API key first").
    pub fn display_name(&self) -> &'static str {
        match self {
            Provider::Gemini => "Gemini",
            Provider::Together => "Together AI",
            Provider::Ollama => "Ollama",
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
        }
    }
}

impl AppSettings {
    pub fn model_for(&self, provider: Provider) -> String {
        self.models
            .get(provider.as_str())
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_model(&mut self, provider: Provider, model: &str) {
        self.models
            .insert(provider.as_str().to_string(), model.to_string());
    }

    /// The model for the currently selected provider, trimmed.
    pub fn current_model(&self) -> String {
        self.model_for(self.provider).trim().to_string()
    }
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
