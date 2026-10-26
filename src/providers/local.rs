use std::io::{BufRead, BufReader};
use std::time::Duration;

use crate::config::AppSettings;
use crate::local::{lifecycle, server};
use crate::providers::stream::{read_events, Partial, StreamDelta, StreamFormat};
use crate::providers::{clean_reply, truncate_body};
use crate::settings;

pub fn build_request_body(text: &str, instruction: &str, max_tokens: u32) -> serde_json::Value {
    serde_json::json!({
        "model": "local",
        "messages": [
            { "role": "system", "content": instruction },
            { "role": "user", "content": text }
        ],
        "temperature": 0,
        "max_tokens": max_tokens,
        "stream": true,
        "chat_template_kwargs": { "enable_thinking": false }
    })
}

pub(crate) fn length_cutoff_message(max_tokens: u32) -> String {
    format!(
        "The local model's reply was cut off at the longest-reply limit ({max_tokens} tokens). You can raise it in Settings."
    )
}

pub fn parse_response(status: u16, body: &str, max_tokens: u32) -> Result<String, String> {
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
            "The local model request failed (HTTP {status}): {detail}"
        ));
    }

    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(e) => {
            return Err(format!("Could not parse the local model response: {e}"));
        }
    };

    let first = value
        .get("choices")
        .and_then(|choices| choices.as_array())
        .and_then(|list| list.first());

    let finish_reason = first
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(|reason| reason.as_str())
        .unwrap_or("unknown");

    if finish_reason == "length" {
        return Err(length_cutoff_message(max_tokens));
    }

    let text = first
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
        .unwrap_or_default();

    match clean_reply(text) {
        Some(reply) => Ok(reply),
        None => Err("The local model returned no suggestion.".to_string()),
    }
}

/// Calls `lifecycle::request_finished()` when dropped, so every path out of
/// [`complete`] (success, error, or `?`) ends the request.
struct RequestGuard;

impl Drop for RequestGuard {
    fn drop(&mut self) {
        lifecycle::request_finished();
    }
}

fn parse_stream_event(json: &str) -> Result<StreamDelta, String> {
    if json.trim() == "[DONE]" {
        return Ok(StreamDelta {
            done: true,
            ..StreamDelta::default()
        });
    }
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("Could not parse the local model response: {e}"))?;
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(|message| message.as_str())
            .or_else(|| error.as_str())
            .map(|message| message.to_string())
            .unwrap_or_else(|| truncate_body(json));
        return Err(format!("The local model request failed: {message}"));
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

fn finish(raw: String, finish_reason: Option<String>, max_tokens: u32) -> Result<String, String> {
    if finish_reason.as_deref() == Some("length") {
        return Err(length_cutoff_message(max_tokens));
    }
    match clean_reply(&raw) {
        Some(reply) => Ok(reply),
        None => Err("The local model returned no suggestion.".to_string()),
    }
}

fn run_stream(
    reader: &mut dyn BufRead,
    max_tokens: u32,
    on_partial: &mut dyn FnMut(&str),
) -> Result<String, String> {
    let mut partial = Partial::new(&mut *on_partial);
    let mut finish_reason: Option<String> = None;
    read_events(reader, StreamFormat::Sse, &mut |event| {
        let delta = parse_stream_event(event)?;
        if !delta.text.is_empty() {
            partial.push(&delta.text);
        }
        if delta.finish_reason.is_some() {
            finish_reason = delta.finish_reason.clone();
        }
        Ok(!delta.done)
    })?;
    finish(partial.into_raw(), finish_reason, max_tokens)
}

/// One request to the local helper: starts it if needed, posts the chat
/// completion, and parses the reply. `suggest` uses the longest-reply
/// setting; the warm-up in `lifecycle` passes a smaller `max_tokens`.
pub(crate) fn complete(
    settings: &AppSettings,
    text: &str,
    instruction: &str,
    max_tokens: u32,
    on_partial: &mut dyn FnMut(&str),
) -> Result<String, String> {
    lifecycle::request_started();
    let _guard = RequestGuard;

    let endpoint = server::ensure_running(settings)?;
    if let Some(notice) = server::take_crash_notice() {
        settings::set_provider_message(Some(notice));
    }

    // A fresh agent per request so a changed reply time limit applies at once.
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(
            settings.local_reply_timeout_secs as u64,
        )))
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let url = format!("{}/v1/chat/completions", endpoint.base_url);
    let mut response = agent
        .post(&url)
        .header("Authorization", format!("Bearer {}", endpoint.api_key))
        .send_json(build_request_body(text, instruction, max_tokens))
        .map_err(|e| format!("Could not reach the local model: {e}"))?;

    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("Could not reach the local model: {e}"))?;
        return parse_response(status, &body, max_tokens);
    }
    let mut reader = BufReader::new(response.body_mut().as_reader());
    run_stream(&mut reader, max_tokens, on_partial)
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
    complete(
        settings,
        text,
        instruction,
        settings.local_max_reply_tokens,
        on_partial,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        build_request_body, length_cutoff_message, parse_response, parse_stream_event, run_stream,
    };
    use std::io::{BufReader, Cursor};

    #[test]
    fn request_body_shape() {
        let body = build_request_body("She go.", "Fix it.", 1024);
        assert_eq!(body["model"], "local");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "Fix it.");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "She go.");
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["stream"], true);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        // No key anywhere in the body.
        assert!(body.get("api_key").is_none());
        let serialized = body.to_string();
        assert!(!serialized.contains("Bearer"));
        assert!(!serialized.contains("api_key"));
        assert!(!serialized.contains("Authorization"));
    }

    #[test]
    fn parses_200_with_text() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"She goes to school."},"finish_reason":"stop"}]}"#;
        assert_eq!(
            parse_response(200, body, 1024).expect("should parse"),
            "She goes to school."
        );
    }

    #[test]
    fn parses_error_body() {
        let body = r#"{"error":{"message":"model not loaded"}}"#;
        assert_eq!(
            parse_response(503, body, 1024).expect_err("should fail"),
            "The local model request failed (HTTP 503): model not loaded"
        );
        assert_eq!(
            parse_response(500, "boom", 1024).expect_err("should fail"),
            "The local model request failed (HTTP 500): boom"
        );
    }

    #[test]
    fn length_cutoff_message_is_exact() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"She goes to sch"},"finish_reason":"length"}]}"#;
        assert_eq!(
            parse_response(200, body, 256).expect_err("should fail"),
            "The local model's reply was cut off at the longest-reply limit (256 tokens). You can raise it in Settings."
        );
        assert_eq!(
            length_cutoff_message(1),
            "The local model's reply was cut off at the longest-reply limit (1 tokens). You can raise it in Settings."
        );
    }

    #[test]
    fn empty_reply_is_no_suggestion() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"  "},"finish_reason":"stop"}]}"#;
        assert_eq!(
            parse_response(200, body, 1024).expect_err("should fail"),
            "The local model returned no suggestion."
        );
    }

    fn run(bytes: &str, max_tokens: u32) -> (Result<String, String>, Vec<String>) {
        let mut partials: Vec<String> = Vec::new();
        let result = {
            let mut reader = BufReader::with_capacity(2, Cursor::new(bytes.as_bytes().to_vec()));
            let mut on_partial = |text: &str| partials.push(text.to_string());
            run_stream(&mut reader, max_tokens, &mut on_partial)
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
        let (result, partials) = run(&bytes, 1024);
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
        let (result, partials) = run(&bytes, 1024);
        assert_eq!(result.expect("should succeed"), "She goes.");
        assert_eq!(partials, ["She goes."]);
    }

    #[test]
    fn stream_unclear_result_shows_no_partial() {
        let bytes = format!("{}{}", event("UNC", None), event("LEAR", Some("stop")));
        let (result, partials) = run(&bytes, 1024);
        assert_eq!(result.expect("should succeed"), "UNCLEAR");
        assert!(partials.is_empty(), "{partials:?}");
    }

    #[test]
    fn stream_empty_error_and_length() {
        let (result, _) = run(&event("  ", Some("stop")), 1024);
        assert_eq!(
            result.expect_err("should fail"),
            "The local model returned no suggestion."
        );
        let (result, _) = run(
            "data: {\"error\":{\"message\":\"model not loaded\"}}\n\n",
            1024,
        );
        assert_eq!(
            result.expect_err("should fail"),
            "The local model request failed: model not loaded"
        );
        let (result, _) = run(&event("She goes to sch", Some("length")), 256);
        assert_eq!(result.expect_err("should fail"), length_cutoff_message(256));
    }

    #[test]
    fn stream_event_done_marker() {
        let delta = parse_stream_event("[DONE]").expect("should parse");
        assert!(delta.done);
        assert!(delta.text.is_empty());
        assert_eq!(delta.finish_reason, None);
    }
}
