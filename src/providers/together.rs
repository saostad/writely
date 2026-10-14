use std::sync::OnceLock;
use std::time::Duration;

use crate::config::{AppSettings, Effort};
use crate::providers::gemini::SYSTEM_INSTRUCTION;
use crate::providers::{clean_reply, truncate_body};

const ENDPOINT: &str = "https://api.together.ai/v1/chat/completions";

pub fn build_request_body(model: &str, text: &str, effort: Effort) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": SYSTEM_INSTRUCTION },
            { "role": "user", "content": text }
        ],
        "temperature": 0
    });
    match effort {
        Effort::Default => {}
        Effort::Minimal => {
            body["reasoning"] = serde_json::json!({ "enabled": false });
        }
        Effort::Low | Effort::Medium | Effort::High => {
            body["reasoning_effort"] = serde_json::Value::String(effort.as_str().to_string());
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
                    .and_then(|error| error.get("message"))
                    .and_then(|message| message.as_str())
                    .map(|message| message.to_string())
            })
            .unwrap_or_else(|| truncate_body(body));
        return Err(format!(
            "Together AI request failed (HTTP {status}, model {model}): {detail}"
        ));
    }

    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(e) => {
            return Err(format!("Could not parse Together AI response: {e}"));
        }
    };

    let first = value
        .get("choices")
        .and_then(|choices| choices.as_array())
        .and_then(|list| list.first());

    let finish_reason = first
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(|reason| reason.as_str())
        .map(|reason| reason.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let text = first
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
        .unwrap_or_default();

    match clean_reply(text) {
        Some(reply) => Ok(reply),
        None => Err(format!(
            "Together AI returned no suggestion (model {model}, finishReason: {finish_reason})."
        )),
    }
}

static AGENT: OnceLock<ureq::Agent> = OnceLock::new();

fn agent() -> &'static ureq::Agent {
    AGENT.get_or_init(|| {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build();
        ureq::Agent::new_with_config(config)
    })
}

pub fn suggest(settings: &AppSettings, key: Option<&str>, text: &str) -> Result<String, String> {
    let model = settings.current_model();
    if model.is_empty() {
        return Err(
            "No model is set for Together AI. Choose a model first in Settings.".to_string(),
        );
    }
    let Some(api_key) = key else {
        return Err(
            "Add your Together AI API key first: click the Writely menu-bar icon › Settings…"
                .to_string(),
        );
    };

    let mut response = agent()
        .post(ENDPOINT)
        .header("Authorization", format!("Bearer {api_key}"))
        .send_json(build_request_body(&model, text, settings.effort))
        .map_err(|e| format!("Could not reach Together AI: {e}"))?;

    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Could not reach Together AI: {e}"))?;

    parse_response(status, &body, &model)
}

#[cfg(test)]
mod tests {
    use super::{build_request_body, parse_response};
    use crate::config::Effort;

    const MODEL: &str = "deepseek-ai/DeepSeek-V4.1-Flash";

    #[test]
    fn request_body_shape_default() {
        let body = build_request_body(MODEL, "She go.", Effort::Default);
        assert_eq!(body["model"], MODEL);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "She go.");
        assert_eq!(body["temperature"], 0);
        assert!(body.get("reasoning").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn request_body_effort_levels() {
        let minimal = build_request_body(MODEL, "She go.", Effort::Minimal);
        assert_eq!(minimal["reasoning"]["enabled"], false);
        assert!(minimal.get("reasoning_effort").is_none());
        for effort in [Effort::Low, Effort::Medium, Effort::High] {
            let body = build_request_body(MODEL, "She go.", effort);
            assert_eq!(
                body["reasoning_effort"],
                effort.as_str(),
                "effort {effort:?}"
            );
            assert!(body.get("reasoning").is_none());
        }
    }

    #[test]
    fn parses_200_with_text() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"She goes to school."},"finish_reason":"stop"}]}"#;
        assert_eq!(
            parse_response(200, body, MODEL).expect("should parse"),
            "She goes to school."
        );
    }

    #[test]
    fn strips_leading_think_block() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"<think>verb fix</think>She goes."},"finish_reason":"stop"}]}"#;
        assert_eq!(
            parse_response(200, body, MODEL).expect("should parse"),
            "She goes."
        );
    }

    #[test]
    fn parses_error_body() {
        let body = r#"{"error":{"message":"model not found"}}"#;
        assert_eq!(
            parse_response(404, body, MODEL).expect_err("should fail"),
            "Together AI request failed (HTTP 404, model deepseek-ai/DeepSeek-V4.1-Flash): model not found"
        );
    }

    #[test]
    fn parses_non_json_error_body() {
        assert_eq!(
            parse_response(500, "boom", MODEL).expect_err("should fail"),
            "Together AI request failed (HTTP 500, model deepseek-ai/DeepSeek-V4.1-Flash): boom"
        );
    }

    #[test]
    fn empty_content_is_no_suggestion() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"  "},"finish_reason":"stop"}]}"#;
        assert!(parse_response(200, body, MODEL).is_err());
    }
}
