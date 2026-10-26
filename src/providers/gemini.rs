use std::io::{BufRead, BufReader};
use std::sync::OnceLock;
use std::time::Duration;

use crate::config::{AppSettings, Effort};
use crate::providers::stream::{read_events, Partial, StreamDelta, StreamFormat};
use crate::providers::{clean_reply, truncate_body};

pub const SYSTEM_INSTRUCTION: &str = "You fix English grammar, spelling and punctuation. Return only the corrected text, with no quotes or explanation. Keep the meaning, tone and wording otherwise unchanged. If the text is already correct, return it unchanged. Keep line breaks and paragraph breaks as they are. If the text is not understandable English (random letters or gibberish), reply with exactly UNCLEAR and nothing else.";

pub fn build_request_body(sentence: &str, effort: Effort, instruction: &str) -> serde_json::Value {
    let mut generation_config = serde_json::json!({ "temperature": 0 });
    if effort != Effort::Default {
        generation_config["thinkingConfig"] = serde_json::json!({
            "thinkingLevel": effort.as_str()
        });
    }
    serde_json::json!({
        "systemInstruction": {
            "parts": [{ "text": instruction }]
        },
        "contents": [
            { "parts": [{ "text": sentence }] }
        ],
        "generationConfig": generation_config
    })
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
            "Gemini request failed (HTTP {status}, model {model}): {detail}"
        ));
    }

    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(e) => {
            return Err(format!("Could not parse Gemini response: {e}"));
        }
    };

    let candidates = value
        .get("candidates")
        .and_then(|candidates| candidates.as_array());

    let first = candidates.and_then(|list| list.first());

    let finish_reason = first
        .and_then(|candidate| candidate.get("finishReason"))
        .and_then(|reason| reason.as_str())
        .map(|reason| reason.to_string())
        .or_else(|| {
            value
                .get("promptFeedback")
                .and_then(|feedback| feedback.get("blockReason"))
                .and_then(|reason| reason.as_str())
                .map(|reason| reason.to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());

    let text = first
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(|parts| parts.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text"))
                .filter_map(|text| text.as_str())
                .collect::<String>()
        })
        .unwrap_or_default();

    match clean_reply(&text) {
        Some(reply) => Ok(reply),
        None => Err(format!(
            "Gemini returned no suggestion (model {model}, finishReason: {finish_reason})."
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
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("Could not parse Gemini response: {e}"))?;
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(|message| message.as_str())
            .map(|message| message.to_string())
            .unwrap_or_else(|| truncate_body(json));
        return Err(format!("Gemini request failed (model {model}): {message}"));
    }
    let first = value
        .get("candidates")
        .and_then(|candidates| candidates.as_array())
        .and_then(|list| list.first());
    let finish_reason = first
        .and_then(|candidate| candidate.get("finishReason"))
        .and_then(|reason| reason.as_str())
        .map(|reason| reason.to_string())
        .or_else(|| {
            value
                .get("promptFeedback")
                .and_then(|feedback| feedback.get("blockReason"))
                .and_then(|reason| reason.as_str())
                .map(|reason| reason.to_string())
        });
    let text = first
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(|parts| parts.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text"))
                .filter_map(|text| text.as_str())
                .collect::<String>()
        })
        .unwrap_or_default();
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
            "Gemini returned no suggestion (model {model}, finishReason: {}).",
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

#[allow(dead_code)] // Non-streaming wrapper; dispatch_stream calls suggest_stream directly.
pub fn suggest(
    settings: &AppSettings,
    key: Option<&str>,
    sentence: &str,
    instruction: &str,
) -> Result<String, String> {
    suggest_stream(settings, key, sentence, instruction, &mut |_| {})
}

pub fn suggest_stream(
    settings: &AppSettings,
    key: Option<&str>,
    sentence: &str,
    instruction: &str,
    on_partial: &mut dyn FnMut(&str),
) -> Result<String, String> {
    let model = settings.current_model();
    if model.is_empty() {
        return Err("No model is set for Gemini. Choose a model first in Settings.".to_string());
    }
    let Some(api_key) = key else {
        return Err(
            "Add your Gemini API key first: click the Writely menu-bar icon › Settings…"
                .to_string(),
        );
    };
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{model}:streamGenerateContent?alt=sse"
    );

    let mut response = agent()
        .post(&url)
        .header("x-goog-api-key", api_key)
        .send_json(build_request_body(sentence, settings.effort, instruction))
        .map_err(|e| format!("Could not reach Gemini: {e}"))?;

    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("Could not reach Gemini: {e}"))?;
        return parse_response(status, &body, &model);
    }
    let mut reader = BufReader::new(response.body_mut().as_reader());
    run_stream(&mut reader, &model, on_partial)
}

#[cfg(test)]
mod tests {
    use super::{
        build_request_body, parse_response, parse_stream_event, run_stream, SYSTEM_INSTRUCTION,
    };
    use crate::config::Effort;
    use std::io::{BufReader, Cursor};

    const MODEL: &str = "gemini-3.5-flash-lite";

    #[test]
    fn request_body_shape() {
        let body = build_request_body("She go to school.", Effort::Default, SYSTEM_INSTRUCTION);
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            SYSTEM_INSTRUCTION
        );
        assert_eq!(body["contents"][0]["parts"][0]["text"], "She go to school.");
        assert_eq!(body["generationConfig"]["temperature"], 0);
        assert!(body["generationConfig"].get("thinkingConfig").is_none());
    }

    #[test]
    fn request_body_uses_given_instruction() {
        let body = build_request_body("She go.", Effort::Default, "Be friendly.");
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "Be friendly."
        );
    }

    #[test]
    fn request_body_effort_levels() {
        for effort in [Effort::Minimal, Effort::Low, Effort::Medium, Effort::High] {
            let body = build_request_body("She go.", effort, SYSTEM_INSTRUCTION);
            assert_eq!(
                body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
                effort.as_str(),
                "effort {effort:?}"
            );
        }
    }

    #[test]
    fn parses_200_with_text() {
        let body = r#"{"candidates":[{"content":{"parts":[{"text":"She goes to school."}],"role":"model"},"finishReason":"STOP"}]}"#;
        assert_eq!(
            parse_response(200, body, MODEL).expect("should parse suggestion"),
            "She goes to school."
        );
    }

    #[test]
    fn trims_and_strips_one_pair_of_wrapping_quotes() {
        let body = r#"{"candidates":[{"content":{"parts":[{"text":"\"She goes to school.\""}],"role":"model"},"finishReason":"STOP"}]}"#;
        assert_eq!(
            parse_response(200, body, MODEL).expect("should parse suggestion"),
            "She goes to school."
        );
    }

    #[test]
    fn strips_leading_think_block() {
        let body = r#"{"candidates":[{"content":{"parts":[{"text":"<think>fix verb</think>She goes to school."}],"role":"model"},"finishReason":"STOP"}]}"#;
        assert_eq!(
            parse_response(200, body, MODEL).expect("should parse suggestion"),
            "She goes to school."
        );
    }

    #[test]
    fn parses_400_with_error_message() {
        let body = r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT"}}"#;
        assert_eq!(
            parse_response(400, body, MODEL).expect_err("should fail"),
            "Gemini request failed (HTTP 400, model gemini-3.5-flash-lite): API key not valid. Please pass a valid API key."
        );
    }

    #[test]
    fn parses_500_with_non_json_body() {
        assert_eq!(
            parse_response(500, "Internal Server Error: something broke", MODEL)
                .expect_err("should fail"),
            "Gemini request failed (HTTP 500, model gemini-3.5-flash-lite): Internal Server Error: something broke"
        );
    }

    #[test]
    fn truncates_long_non_json_error_body_to_300_chars() {
        let body = "x".repeat(400);
        let err = parse_response(500, &body, MODEL).expect_err("should fail");
        let prefix = "Gemini request failed (HTTP 500, model gemini-3.5-flash-lite): ";
        assert!(err.starts_with(prefix));
        assert_eq!(err.len(), prefix.len() + 300);
    }

    #[test]
    fn parses_200_with_no_candidates_safety() {
        let body = r#"{"candidates":[{"finishReason":"SAFETY","content":{"parts":[]}}]}"#;
        assert_eq!(
            parse_response(200, body, MODEL).expect_err("should fail"),
            "Gemini returned no suggestion (model gemini-3.5-flash-lite, finishReason: SAFETY)."
        );
    }

    #[test]
    fn parses_200_with_empty_candidates_as_unknown() {
        let body = r#"{"candidates":[]}"#;
        assert_eq!(
            parse_response(200, body, MODEL).expect_err("should fail"),
            "Gemini returned no suggestion (model gemini-3.5-flash-lite, finishReason: unknown)."
        );
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
        let mut candidate = serde_json::json!({ "content": { "parts": [{ "text": text }] } });
        if let Some(reason) = finish {
            candidate["finishReason"] = serde_json::Value::String(reason.to_string());
        }
        format!(
            "data: {}\n\n",
            serde_json::json!({ "candidates": [candidate] })
        )
    }

    #[test]
    fn stream_assembles_text_and_partials_in_order() {
        let bytes = format!(
            "{}{}{}",
            event("She ", None),
            event("goes to ", None),
            event("school.", Some("STOP"))
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
            event(" verb</think>She goes.", Some("STOP"))
        );
        let (result, partials) = run(&bytes);
        assert_eq!(result.expect("should succeed"), "She goes.");
        assert_eq!(partials, ["She goes."]);
    }

    #[test]
    fn stream_unclear_result_shows_no_partial() {
        let bytes = format!("{}{}", event("UNC", None), event("LEAR", Some("STOP")));
        let (result, partials) = run(&bytes);
        assert_eq!(result.expect("should succeed"), "UNCLEAR");
        assert!(partials.is_empty(), "{partials:?}");
    }

    #[test]
    fn stream_empty_and_error_event() {
        let (result, _) = run(&event("", Some("SAFETY")));
        assert_eq!(
            result.expect_err("should fail"),
            "Gemini returned no suggestion (model gemini-3.5-flash-lite, finishReason: SAFETY)."
        );
        let (result, _) = run("data: {\"error\":{\"message\":\"quota gone\"}}\n\n");
        assert_eq!(
            result.expect_err("should fail"),
            "Gemini request failed (model gemini-3.5-flash-lite): quota gone"
        );
    }

    #[test]
    fn stream_event_uses_block_reason_as_finish() {
        let delta = parse_stream_event(r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#, MODEL)
            .expect("should parse");
        assert_eq!(delta.finish_reason.as_deref(), Some("SAFETY"));
        assert!(delta.done);
        assert!(delta.text.is_empty());
    }
}
