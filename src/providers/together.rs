use std::io::{BufRead, BufReader};
use std::sync::OnceLock;
use std::time::Duration;

use crate::config::{AppSettings, Effort};
use crate::providers::stream::{read_events, Partial, StreamDelta, StreamFormat};
use crate::providers::{clean_reply, truncate_body};

const ENDPOINT: &str = "https://api.together.ai/v1/chat/completions";

pub fn build_request_body(
    model: &str,
    text: &str,
    effort: Effort,
    instruction: &str,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": instruction },
            { "role": "user", "content": text }
        ],
        "temperature": 0,
        "stream": true
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

fn parse_stream_event(json: &str, model: &str) -> Result<StreamDelta, String> {
    if json.trim() == "[DONE]" {
        return Ok(StreamDelta {
            done: true,
            ..StreamDelta::default()
        });
    }
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("Could not parse Together AI response: {e}"))?;
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(|message| message.as_str())
            .or_else(|| error.as_str())
            .map(|message| message.to_string())
            .unwrap_or_else(|| truncate_body(json));
        return Err(format!(
            "Together AI request failed (model {model}): {message}"
        ));
    }
    let first = value
        .get("choices")
        .and_then(|choices| choices.as_array())
        .and_then(|list| list.first());
    let finish_reason = first
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(|reason| reason.as_str())
        .map(|reason| reason.to_string());
    let text = first
        .and_then(|choice| choice.get("delta"))
        .and_then(|delta| delta.get("content"))
        .and_then(|content| content.as_str())
        .unwrap_or_default()
        .to_string();
    Ok(StreamDelta {
        text,
        done: finish_reason.is_some(),
        finish_reason,
    })
}

fn finish(raw: String, finish_reason: Option<String>, model: &str) -> Result<String, String> {
    match clean_reply(&raw) {
        Some(reply) => Ok(reply),
        None => Err(format!(
            "Together AI returned no suggestion (model {model}, finishReason: {}).",
            finish_reason.as_deref().unwrap_or("unknown")
        )),
    }
}

fn run_stream(
    reader: &mut dyn BufRead,
    model: &str,
    on_partial: &mut dyn FnMut(&str),
) -> Result<String, String> {
    let mut partial = Partial::new(&mut *on_partial);
    let mut finish_reason: Option<String> = None;
    read_events(reader, StreamFormat::Sse, &mut |event| {
        let delta = parse_stream_event(event, model)?;
        if !delta.text.is_empty() {
            partial.push(&delta.text);
        }
        if delta.finish_reason.is_some() {
            finish_reason = delta.finish_reason.clone();
        }
        Ok(!delta.done)
    })?;
    finish(partial.into_raw(), finish_reason, model)
}

pub fn suggest(
    settings: &AppSettings,
    key: Option<&str>,
    text: &str,
    instruction: &str,
) -> Result<String, String> {
    suggest_stream(settings, key, text, instruction, &mut |_| {})
}

pub fn suggest_stream(
    settings: &AppSettings,
    key: Option<&str>,
    text: &str,
    instruction: &str,
    on_partial: &mut dyn FnMut(&str),
) -> Result<String, String> {
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
        .send_json(build_request_body(
            &model,
            text,
            settings.effort,
            instruction,
        ))
        .map_err(|e| format!("Could not reach Together AI: {e}"))?;

    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("Could not reach Together AI: {e}"))?;
        return parse_response(status, &body, &model);
    }
    let mut reader = BufReader::new(response.body_mut().as_reader());
    run_stream(&mut reader, &model, on_partial)
}

#[cfg(test)]
mod tests {
    use super::{build_request_body, parse_response, parse_stream_event, run_stream};
    use crate::config::Effort;
    use std::io::{BufReader, Cursor};

    const MODEL: &str = "deepseek-ai/DeepSeek-V4.1-Flash";

    #[test]
    fn request_body_shape_default() {
        let body = build_request_body(MODEL, "She go.", Effort::Default, "Fix it.");
        assert_eq!(body["model"], MODEL);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "Fix it.");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "She go.");
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["stream"], true);
        assert!(body.get("reasoning").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn request_body_uses_given_instruction() {
        let body = build_request_body(MODEL, "She go.", Effort::Default, "Be formal.");
        assert_eq!(body["messages"][0]["content"], "Be formal.");
    }

    #[test]
    fn request_body_effort_levels() {
        let minimal = build_request_body(MODEL, "She go.", Effort::Minimal, "Fix it.");
        assert_eq!(minimal["reasoning"]["enabled"], false);
        assert!(minimal.get("reasoning_effort").is_none());
        for effort in [Effort::Low, Effort::Medium, Effort::High] {
            let body = build_request_body(MODEL, "She go.", effort, "Fix it.");
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

    fn run(bytes: &str) -> (Result<String, String>, Vec<String>) {
        let mut partials: Vec<String> = Vec::new();
        let result = {
            let mut reader = BufReader::with_capacity(2, Cursor::new(bytes.as_bytes().to_vec()));
            let mut on_partial = |text: &str| partials.push(text.to_string());
            run_stream(&mut reader, MODEL, &mut on_partial)
        };
        (result, partials)
    }

    fn event(text: &str, finish: Option<&str>) -> String {
        let mut choice = serde_json::json!({
            "delta": { "role": "assistant", "content": text },
            "finish_reason": null
        });
        if let Some(reason) = finish {
            choice["finish_reason"] = serde_json::Value::String(reason.to_string());
        }
        format!("data: {}\n\n", serde_json::json!({ "choices": [choice] }))
    }

    #[test]
    fn stream_assembles_text_and_partials_in_order() {
        let bytes = format!(
            "{}{}{}data: [DONE]\n\n",
            event("She ", None),
            event("goes to ", None),
            event("school.", Some("stop"))
        );
        let (result, partials) = run(&bytes);
        assert_eq!(result.expect("should succeed"), "She goes to school.");
        assert_eq!(partials, ["She", "She goes to", "She goes to school."]);
    }

    #[test]
    fn stream_hides_think_block_until_closed() {
        let bytes = format!(
            "{}{}",
            event("<think>fix", None),
            event(" verb</think>She goes.", Some("stop"))
        );
        let (result, partials) = run(&bytes);
        assert_eq!(result.expect("should succeed"), "She goes.");
        assert_eq!(partials, ["She goes."]);
    }

    #[test]
    fn stream_unclear_result_shows_no_partial() {
        let bytes = format!("{}{}", event("UNC", None), event("LEAR", Some("stop")));
        let (result, partials) = run(&bytes);
        assert_eq!(result.expect("should succeed"), "UNCLEAR");
        assert!(partials.is_empty(), "{partials:?}");
    }

    #[test]
    fn stream_empty_and_error_event() {
        let (result, _) = run(&event("  ", Some("stop")));
        assert_eq!(
            result.expect_err("should fail"),
            "Together AI returned no suggestion (model deepseek-ai/DeepSeek-V4.1-Flash, finishReason: stop)."
        );
        let (result, _) = run("data: {\"error\":{\"message\":\"model not found\"}}\n\n");
        assert_eq!(
            result.expect_err("should fail"),
            "Together AI request failed (model deepseek-ai/DeepSeek-V4.1-Flash): model not found"
        );
    }

    #[test]
    fn stream_event_done_marker() {
        let delta = parse_stream_event("[DONE]", MODEL).expect("should parse");
        assert!(delta.done);
        assert!(delta.text.is_empty());
        assert_eq!(delta.finish_reason, None);
    }
}
