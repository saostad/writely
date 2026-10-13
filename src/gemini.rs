use std::time::Duration;

#[allow(dead_code)]
pub const GEMINI_MODEL: &str = "gemini-3.6-flash";

const SYSTEM_INSTRUCTION: &str = "You fix English grammar, spelling and punctuation. Return only the corrected text, with no quotes or explanation. Keep the meaning, tone and wording otherwise unchanged. If the text is already correct, return it unchanged.";

#[allow(dead_code)]
pub fn build_request_body(sentence: &str) -> serde_json::Value {
    serde_json::json!({
        "systemInstruction": {
            "parts": [{ "text": SYSTEM_INSTRUCTION }]
        },
        "contents": [
            { "parts": [{ "text": sentence }] }
        ],
        "generationConfig": { "temperature": 0 }
    })
}

#[allow(dead_code)]
pub fn parse_response(status: u16, body: &str) -> Result<String, String> {
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
            .unwrap_or_else(|| body.chars().take(300).collect());
        return Err(format!("Gemini request failed (HTTP {status}): {detail}"));
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

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "Gemini returned no suggestion (finishReason: {finish_reason})."
        ));
    }

    let unquoted = if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].trim()
    } else {
        trimmed
    };

    if unquoted.is_empty() {
        return Err(format!(
            "Gemini returned no suggestion (finishReason: {finish_reason})."
        ));
    }

    Ok(unquoted.to_string())
}

#[allow(dead_code)]
pub async fn suggest(api_key: &str, sentence: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("Could not reach Gemini: {e}"))?;

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{GEMINI_MODEL}:generateContent"
    );

    let response = client
        .post(&url)
        .header("x-goog-api-key", api_key)
        .json(&build_request_body(sentence))
        .send()
        .await
        .map_err(|e| format!("Could not reach Gemini: {e}"))?;

    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Could not reach Gemini: {e}"))?;

    parse_response(status, &body)
}

#[cfg(test)]
mod tests {
    use super::{build_request_body, parse_response, GEMINI_MODEL};

    #[test]
    fn model_name_is_the_required_one() {
        assert_eq!(GEMINI_MODEL, "gemini-3.6-flash");
    }

    #[test]
    fn request_body_shape() {
        let body = build_request_body("She go to school.");
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "You fix English grammar, spelling and punctuation. Return only the corrected text, with no quotes or explanation. Keep the meaning, tone and wording otherwise unchanged. If the text is already correct, return it unchanged."
        );
        assert_eq!(body["contents"][0]["parts"][0]["text"], "She go to school.");
        assert_eq!(body["generationConfig"]["temperature"], 0);
    }

    #[test]
    fn parses_200_with_text() {
        let body = r#"{"candidates":[{"content":{"parts":[{"text":"She goes to school."}],"role":"model"},"finishReason":"STOP"}]}"#;
        assert_eq!(
            parse_response(200, body).expect("should parse suggestion"),
            "She goes to school."
        );
    }

    #[test]
    fn trims_and_strips_one_pair_of_wrapping_quotes() {
        let body = r#"{"candidates":[{"content":{"parts":[{"text":"\"She goes to school.\""}],"role":"model"},"finishReason":"STOP"}]}"#;
        assert_eq!(
            parse_response(200, body).expect("should parse suggestion"),
            "She goes to school."
        );
    }

    #[test]
    fn trims_returned_text() {
        let body = r#"{"candidates":[{"content":{"parts":[{"text":"  She goes to school.  "}],"role":"model"},"finishReason":"STOP"}]}"#;
        assert_eq!(
            parse_response(200, body).expect("should parse suggestion"),
            "She goes to school."
        );
    }

    #[test]
    fn parses_400_with_error_message() {
        let body = r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT"}}"#;
        assert_eq!(
            parse_response(400, body).expect_err("should fail"),
            "Gemini request failed (HTTP 400): API key not valid. Please pass a valid API key."
        );
    }

    #[test]
    fn parses_500_with_non_json_body() {
        assert_eq!(
            parse_response(500, "Internal Server Error: something broke").expect_err("should fail"),
            "Gemini request failed (HTTP 500): Internal Server Error: something broke"
        );
    }

    #[test]
    fn truncates_long_non_json_error_body_to_300_chars() {
        let body = "x".repeat(400);
        let err = parse_response(500, &body).expect_err("should fail");
        let prefix = "Gemini request failed (HTTP 500): ";
        assert!(err.starts_with(prefix));
        assert_eq!(err.len(), prefix.len() + 300);
    }

    #[test]
    fn parses_200_with_no_candidates_safety() {
        let body = r#"{"candidates":[{"finishReason":"SAFETY","content":{"parts":[]}}]}"#;
        assert_eq!(
            parse_response(200, body).expect_err("should fail"),
            "Gemini returned no suggestion (finishReason: SAFETY)."
        );
    }

    #[test]
    fn parses_200_with_empty_candidates_as_unknown() {
        let body = r#"{"candidates":[]}"#;
        assert_eq!(
            parse_response(200, body).expect_err("should fail"),
            "Gemini returned no suggestion (finishReason: unknown)."
        );
    }
}
