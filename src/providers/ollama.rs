use std::sync::OnceLock;
use std::time::Duration;

use crate::config::{AppSettings, Effort, DEFAULT_OLLAMA_URL};
use crate::providers::gemini::SYSTEM_INSTRUCTION;
use crate::providers::{clean_reply, truncate_body};

pub fn base_url(settings: &AppSettings) -> String {
    let raw = settings.ollama_url.trim();
    let base = if raw.is_empty() {
        DEFAULT_OLLAMA_URL
    } else {
        raw
    };
    base.trim_end_matches('/').to_string()
}

pub fn build_request_body(model: &str, text: &str, effort: Effort) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": SYSTEM_INSTRUCTION },
            { "role": "user", "content": text }
        ],
        "stream": false,
        "options": { "temperature": 0 }
    });
    match effort {
        Effort::Default => {}
        Effort::Minimal => {
            body["think"] = serde_json::Value::Bool(false);
        }
        Effort::Low | Effort::Medium | Effort::High => {
            body["think"] = serde_json::Value::String(effort.as_str().to_string());
        }
    }
    body
}

pub fn parse_response(status: u16, body: &str, model: &str) -> Result<String, String> {
    if !(200..300).contains(&status) {
        let detail = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|error| error.as_str())
                    .map(|error| error.to_string())
            })
            .unwrap_or_else(|| truncate_body(body));
        return Err(format!(
            "Ollama request failed (HTTP {status}, model {model}): {detail}"
        ));
    }

    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(e) => {
            return Err(format!("Could not parse Ollama response: {e}"));
        }
    };

    let text = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
        .unwrap_or_default();

    match clean_reply(text) {
        Some(reply) => Ok(reply),
        None => Err(format!(
            "Ollama returned no suggestion (model {model})."
        )),
    }
}

static AGENT: OnceLock<ureq::Agent> = OnceLock::new();

fn agent() -> &'static ureq::Agent {
    AGENT.get_or_init(|| {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .http_status_as_error(false)
            .build();
        ureq::Agent::new_with_config(config)
    })
}

pub fn suggest(settings: &AppSettings, text: &str) -> Result<String, String> {
    let model = settings.current_model();
    if model.is_empty() {
        return Err("No model is set for Ollama. Choose a model first in Settings.".to_string());
    }
    let base = base_url(settings);
    let url = format!("{base}/api/chat");

    let mut response = agent()
        .post(&url)
        .send_json(build_request_body(&model, text, settings.effort))
        .map_err(|e| format!("Could not reach Ollama at {base} — is Ollama running? ({e})"))?;

    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Could not reach Ollama at {base} — is Ollama running? ({e})"))?;

    parse_response(status, &body, &model)
}

#[cfg(test)]
mod tests {
    use super::{base_url, build_request_body, parse_response};
    use crate::config::{AppSettings, Effort, Provider};

    const MODEL: &str = "llama3.2";

    #[test]
    fn request_body_shape_default() {
        let body = build_request_body(MODEL, "She go.", Effort::Default);
        assert_eq!(body["model"], MODEL);
        assert_eq!(body["messages"][1]["content"], "She go.");
        assert_eq!(body["stream"], false);
        assert_eq!(body["options"]["temperature"], 0);
        assert!(body.get("think").is_none());
    }

    #[test]
    fn request_body_effort_levels() {
        let minimal = build_request_body(MODEL, "She go.", Effort::Minimal);
        assert_eq!(minimal["think"], false);
        for effort in [Effort::Low, Effort::Medium, Effort::High] {
            let body = build_request_body(MODEL, "She go.", effort);
            assert_eq!(body["think"], effort.as_str(), "effort {effort:?}");
        }
    }

    #[test]
    fn parses_200_with_text() {
        let body = r#"{"message":{"role":"assistant","content":"She goes to school."}}"#;
        assert_eq!(
            parse_response(200, body, MODEL).expect("should parse"),
            "She goes to school."
        );
    }

    #[test]
    fn strips_leading_think_block() {
        let body = r#"{"message":{"role":"assistant","content":"<think>verb</think>She goes.","thinking":"verb"}}"#;
        assert_eq!(
            parse_response(200, body, MODEL).expect("should parse"),
            "She goes."
        );
    }

    #[test]
    fn parses_error_body() {
        let body = r#"{"error":"model 'nope' not found"}"#;
        assert_eq!(
            parse_response(404, body, MODEL).expect_err("should fail"),
            "Ollama request failed (HTTP 404, model llama3.2): model 'nope' not found"
        );
    }

    #[test]
    fn base_url_trims_trailing_slash() {
        let settings = AppSettings {
            provider: Provider::Ollama,
            ollama_url: "http://localhost:11434/".to_string(),
            ..AppSettings::default()
        };
        assert_eq!(base_url(&settings), "http://localhost:11434");
    }
}
