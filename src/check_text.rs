//! Shared logic for the "Check text" window: input validation and mapping a
//! provider reply to what the window shows. Pure and platform-neutral.

use crate::{providers, text_access};

/// What the window shows for a provider reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// Show the suggestion (with its changes highlighted).
    Suggestion(String),
    /// The text is already fine.
    Unchanged,
    /// The provider could not understand the text.
    Unclear,
}

/// Trims the pasted text and applies the same length limit as a click
/// selection. Empty and over-long input are user-facing errors.
pub fn validate_input(text: &str) -> Result<String, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("Paste or type some text first.".to_string());
    }
    if trimmed.chars().count() > text_access::MAX_SELECTION_CHARS {
        return Err(text_access::SELECTION_TOO_LONG_MESSAGE.to_string());
    }
    Ok(trimmed.to_string())
}

/// Maps a provider reply to what the window shows, using the same
/// classification as the click flow.
pub fn outcome_for_suggestion(suggestion: String, original: &str) -> CheckOutcome {
    match providers::classify_suggestion(&suggestion, original) {
        providers::SuggestionOutcome::Unclear => CheckOutcome::Unclear,
        providers::SuggestionOutcome::Unchanged => CheckOutcome::Unchanged,
        providers::SuggestionOutcome::Suggestion => CheckOutcome::Suggestion(suggestion),
    }
}

/// The user-facing status text for the non-suggestion outcomes; `None` for an
/// actual suggestion (the window shows the suggestion itself).
pub fn outcome_message(outcome: &CheckOutcome) -> Option<&'static str> {
    match outcome {
        CheckOutcome::Suggestion(_) => None,
        CheckOutcome::Unchanged => Some("Looks good. Writely has no changes to suggest."),
        CheckOutcome::Unclear => Some(providers::UNCLEAR_MESSAGE),
    }
}

#[cfg(test)]
mod tests {
    use super::{outcome_for_suggestion, outcome_message, validate_input, CheckOutcome};
    use crate::{providers, text_access};

    #[test]
    fn rejects_empty_input() {
        assert_eq!(
            validate_input("").expect_err("empty input should fail"),
            "Paste or type some text first."
        );
    }

    #[test]
    fn rejects_whitespace_only_input() {
        assert_eq!(
            validate_input("   \t\n ").expect_err("blank input should fail"),
            "Paste or type some text first."
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            validate_input("  hello  ").expect("input should be valid"),
            "hello"
        );
    }

    #[test]
    fn accepts_exactly_the_limit() {
        let text = "a".repeat(text_access::MAX_SELECTION_CHARS);
        assert_eq!(
            validate_input(&text).expect("limit input should be valid"),
            text
        );
    }

    #[test]
    fn rejects_one_over_the_limit() {
        let text = "a".repeat(text_access::MAX_SELECTION_CHARS + 1);
        assert_eq!(
            validate_input(&text).expect_err("over-long input should fail"),
            text_access::SELECTION_TOO_LONG_MESSAGE
        );
    }

    #[test]
    fn limit_applies_after_trimming() {
        let text = format!("  {}  ", "a".repeat(text_access::MAX_SELECTION_CHARS));
        assert_eq!(
            validate_input(&text)
                .expect("trimmed limit input should be valid")
                .chars()
                .count(),
            text_access::MAX_SELECTION_CHARS
        );
    }

    #[test]
    fn counts_multi_byte_chars_as_chars() {
        let at_limit = "é".repeat(text_access::MAX_SELECTION_CHARS);
        assert!(validate_input(&at_limit).is_ok());
        let over_limit = "é".repeat(text_access::MAX_SELECTION_CHARS + 1);
        assert_eq!(
            validate_input(&over_limit).expect_err("over-long input should fail"),
            text_access::SELECTION_TOO_LONG_MESSAGE
        );
    }

    #[test]
    fn classifies_the_three_outcomes() {
        assert_eq!(
            outcome_for_suggestion("UNCLEAR".to_string(), "some pasted text"),
            CheckOutcome::Unclear
        );
        assert_eq!(
            outcome_for_suggestion(
                "She goes to school.".to_string(),
                "She goes to school."
            ),
            CheckOutcome::Unchanged
        );
        assert_eq!(
            outcome_for_suggestion("She goes to school.".to_string(), "She go to school."),
            CheckOutcome::Suggestion("She goes to school.".to_string())
        );
    }

    #[test]
    fn outcome_messages_for_unchanged_and_unclear() {
        assert_eq!(
            outcome_message(&CheckOutcome::Unchanged),
            Some("Looks good. Writely has no changes to suggest.")
        );
        assert_eq!(
            outcome_message(&CheckOutcome::Unclear),
            Some(providers::UNCLEAR_MESSAGE)
        );
        assert_eq!(
            outcome_message(&CheckOutcome::Suggestion("She goes.".to_string())),
            None
        );
    }
}
