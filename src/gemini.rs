//! Compatibility shim: the providers live in `crate::providers` now.
//! `main.rs` still uses this module until the click flow switches over.

use crate::config::{AppSettings, Effort, Provider, DEFAULT_GEMINI_MODEL};

pub const GEMINI_MODEL: &str = DEFAULT_GEMINI_MODEL;

#[allow(dead_code)]
pub const UNCLEAR_MARKER: &str = crate::providers::UNCLEAR_MARKER;

pub const UNCLEAR_MESSAGE: &str = crate::providers::UNCLEAR_MESSAGE;

pub use crate::providers::{classify_suggestion, SuggestionOutcome};

#[allow(dead_code)]
pub fn build_request_body(sentence: &str) -> serde_json::Value {
    crate::providers::gemini::build_request_body(sentence, Effort::Default)
}

#[allow(dead_code)]
pub fn parse_response(status: u16, body: &str) -> Result<String, String> {
    crate::providers::gemini::parse_response(status, body, GEMINI_MODEL)
}

pub fn suggest(api_key: &str, sentence: &str) -> Result<String, String> {
    let settings = AppSettings {
        provider: Provider::Gemini,
        models: {
            let mut models = std::collections::HashMap::new();
            models.insert("gemini".to_string(), GEMINI_MODEL.to_string());
            models
        },
        effort: Effort::Default,
        ollama_url: crate::config::DEFAULT_OLLAMA_URL.to_string(),
    };
    crate::providers::gemini::suggest(&settings, Some(api_key), sentence)
}
