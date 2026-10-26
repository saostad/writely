use std::io::{BufRead, BufReader};
use std::sync::OnceLock;
use std::time::Duration;

use crate::config::{AppSettings, Effort, DEFAULT_OLLAMA_URL};
use crate::providers::stream::{read_events, Partial, StreamDelta, StreamFormat};
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
        "stream": true,
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
        None => Err(format!("Ollama returned no suggestion (model {model}).")),
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

fn parse_stream_event(json: &str, model: &str) -> Result<StreamDelta, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("Could not parse Ollama response: {e}"))?;
    if let Some(error) = value.get("error").and_then(|error| error.as_str()) {
        return Err(format!("Ollama request failed (model {model}): {error}"));
    }
    let text = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
        .unwrap_or_default()
        .to_string();
    let done = value
        .get("done")
        .and_then(|done| done.as_bool())
        .unwrap_or(false);
    let finish_reason = value
        .get("done_reason")
        .and_then(|reason| reason.as_str())
        .map(|reason| reason.to_string());
    Ok(StreamDelta {
        text,
        finish_reason,
        done,
    })
}

fn finish(raw: String, _finish_reason: Option<String>, model: &str) -> Result<String, String> {
    match clean_reply(&raw) {
        Some(reply) => Ok(reply),
        None => Err(format!("Ollama returned no suggestion (model {model}).")),
    }
}

fn run_stream(
    reader: &mut dyn BufRead,
    model: &str,
    on_partial: &mut dyn FnMut(&str),
) -> Result<String, String> {
    let mut partial = Partial::new(&mut *on_partial);
    let mut finish_reason: Option<String> = None;
    read_events(reader, StreamFormat::Ndjson, &mut |event| {
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

#[allow(dead_code)] // Non-streaming wrapper; dispatch_stream calls suggest_stream directly.
pub fn suggest(settings: &AppSettings, text: &str, instruction: &str) -> Result<String, String> {
    suggest_stream(settings, text, instruction, &mut |_| {})
}

pub fn suggest_stream(
    settings: &AppSettings,
    text: &str,
    instruction: &str,
    on_partial: &mut dyn FnMut(&str),
) -> Result<String, String> {
    let model = settings.current_model();
    if model.is_empty() {
        return Err("No model is set for Ollama. Choose a model first in Settings.".to_string());
    }
    let base = base_url(settings);
    let url = format!("{base}/api/chat");

    let mut response = agent()
        .post(&url)
        .send_json(build_request_body(
            &model,
            text,
            settings.effort,
            instruction,
        ))
        .map_err(|e| format!("Could not reach Ollama at {base} — is Ollama running? ({e})"))?;

    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("Could not reach Ollama at {base} — is Ollama running? ({e})"))?;
        return parse_response(status, &body, &model);
    }
    let mut reader = BufReader::new(response.body_mut().as_reader());
    run_stream(&mut reader, &model, on_partial)
}

#[cfg(test)]
mod tests {
    use super::{base_url, build_request_body, parse_response, parse_stream_event, run_stream};
    use crate::config::{AppSettings, Effort, Provider};
    use std::io::{BufReader, Cursor};

    const MODEL: &str = "llama3.2";

    #[test]
    fn request_body_shape_default() {
        let body = build_request_body(MODEL, "She go.", Effort::Default, "Fix it.");
        assert_eq!(body["model"], MODEL);
        assert_eq!(body["messages"][0]["content"], "Fix it.");
        assert_eq!(body["messages"][1]["content"], "She go.");
        assert_eq!(body["stream"], true);
        assert_eq!(body["options"]["temperature"], 0);
        assert!(body.get("think").is_none());
    }

    #[test]
    fn request_body_uses_given_instruction() {
        let body = build_request_body(MODEL, "She go.", Effort::Default, "Make it shorter.");
        assert_eq!(body["messages"][0]["content"], "Make it shorter.");
    }

    #[test]
    fn request_body_effort_levels() {
        let minimal = build_request_body(MODEL, "She go.", Effort::Minimal, "Fix it.");
        assert_eq!(minimal["think"], false);
        for effort in [Effort::Low, Effort::Medium, Effort::High] {
            let body = build_request_body(MODEL, "She go.", effort, "Fix it.");
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

    fn run(bytes: &str) -> (Result<String, String>, Vec<String>) {
        let mut partials: Vec<String> = Vec::new();
        let result = {
            let mut reader = BufReader::with_capacity(2, Cursor::new(bytes.as_bytes().to_vec()));
            let mut on_partial = |text: &str| partials.push(text.to_string());
            run_stream(&mut reader, MODEL, &mut on_partial)
        };
        (result, partials)
    }

    fn event(text: &str, done: bool) -> String {
        let mut value = serde_json::json!({
            "message": { "role": "assistant", "content": text },
            "done": done
        });
        if done {
            value["done_reason"] = serde_json::Value::String("stop".to_string());
        }
        format!("{value}\n")
    }

    #[test]
    fn stream_assembles_text_and_partials_in_order() {
        let bytes = format!(
            "{}{}{}",
            event("She ", false),
            event("goes to ", false),
            event("school.", true)
        );
        let (result, partials) = run(&bytes);
        assert_eq!(result.expect("should succeed"), "She goes to school.");
        assert_eq!(partials, ["She", "She goes to", "She goes to school."]);
    }

    #[test]
    fn stream_hides_think_block_until_closed() {
        let bytes = format!(
            "{}{}",
            event("<think>fix", false),
            event(" verb</think>She goes.", true)
        );
        let (result, partials) = run(&bytes);
        assert_eq!(result.expect("should succeed"), "She goes.");
        assert_eq!(partials, ["She goes."]);
    }

    #[test]
    fn stream_unclear_result_shows_no_partial() {
        let bytes = format!("{}{}", event("UNC", false), event("LEAR", true));
        let (result, partials) = run(&bytes);
        assert_eq!(result.expect("should succeed"), "UNCLEAR");
        assert!(partials.is_empty(), "{partials:?}");
    }

    #[test]
    fn stream_empty_and_error_event() {
        let (result, _) = run(&event("  ", true));
        assert_eq!(
            result.expect_err("should fail"),
            "Ollama returned no suggestion (model llama3.2)."
        );
        let (result, _) = run("{\"error\":\"model 'nope' not found\"}\n");
        assert_eq!(
            result.expect_err("should fail"),
            "Ollama request failed (model llama3.2): model 'nope' not found"
        );
    }

    #[test]
    fn stream_event_done_reason_is_finish() {
        let delta = parse_stream_event(
            r#"{"message":{"content":"x"},"done":true,"done_reason":"stop"}"#,
            MODEL,
        )
        .expect("should parse");
        assert_eq!(delta.text, "x");
        assert_eq!(delta.finish_reason.as_deref(), Some("stop"));
        assert!(delta.done);
    }
}
