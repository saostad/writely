pub mod gemini;
pub mod ollama;
pub mod together;

pub use crate::config::{AppSettings, Provider};

pub const UNCLEAR_MARKER: &str = "UNCLEAR";

pub const UNCLEAR_MESSAGE: &str =
    "Writely couldn't understand this text, so there's no suggestion.";

#[derive(Debug, PartialEq, Eq)]
pub enum SuggestionOutcome {
    Unclear,
    Unchanged,
    Suggestion,
}

pub fn classify_suggestion(suggestion: &str, sentence: &str) -> SuggestionOutcome {
    if suggestion.trim() == UNCLEAR_MARKER {
        SuggestionOutcome::Unclear
    } else if suggestion.trim() == sentence.trim() {
        SuggestionOutcome::Unchanged
    } else {
        SuggestionOutcome::Suggestion
    }
}

/// Strips a leading `<think>…</think>` reasoning block from a reply: some
/// models put their reasoning in the content. Only a leading block is
/// removed; anything after it is kept.
pub(crate) fn strip_think_block(text: &str) -> &str {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("<think") {
        return text;
    }
    let Some(close) = trimmed.find("</think>") else {
        return text;
    };
    &trimmed[close + "</think>".len()..]
}

/// Trims the reply, strips a leading think block, and removes one pair of
/// wrapping quotes. Returns `None` when nothing readable remains.
pub(crate) fn clean_reply(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_think = strip_think_block(trimmed).trim();
    if without_think.is_empty() {
        return None;
    }
    let unquoted = if without_think.len() >= 2
        && without_think.starts_with('"')
        && without_think.ends_with('"')
    {
        without_think[1..without_think.len() - 1].trim()
    } else {
        without_think
    };
    if unquoted.is_empty() {
        None
    } else {
        Some(unquoted.to_string())
    }
}

/// Truncates a raw error body to 300 chars for error messages.
pub(crate) fn truncate_body(body: &str) -> String {
    body.chars().take(300).collect()
}

/// One entry point for every provider: same system instruction, temperature 0
/// and UNCLEAR handling. The key is `None` for Ollama (no key needed); for
/// Gemini and Together a missing key or model is an error for the popup.
pub fn suggest(settings: &AppSettings, key: Option<&str>, text: &str) -> Result<String, String> {
    match settings.provider {
        Provider::Gemini => gemini::suggest(settings, key, text),
        Provider::Together => together::suggest(settings, key, text),
        Provider::Ollama => ollama::suggest(settings, text),
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_suggestion, clean_reply, strip_think_block, SuggestionOutcome};

    #[test]
    fn classify_suggestion_distinguishes_three_outcomes() {
        assert_eq!(
            classify_suggestion("UNCLEAR", "Sig we we ef wet wf wet"),
            SuggestionOutcome::Unclear
        );
        // Unclear wins over unchanged, even with surrounding whitespace.
        assert_eq!(
            classify_suggestion("  UNCLEAR  ", "UNCLEAR"),
            SuggestionOutcome::Unclear
        );
        assert_eq!(
            classify_suggestion("She goes to school.", "She goes to school."),
            SuggestionOutcome::Unchanged
        );
        assert_eq!(
            classify_suggestion("She goes to school.", "She go to school."),
            SuggestionOutcome::Suggestion
        );
    }

    #[test]
    fn think_block_stripping() {
        assert_eq!(
            strip_think_block("<think>reasoning here</think>She goes to school."),
            "She goes to school."
        );
        assert_eq!(
            strip_think_block("  <think>hm</think>  She goes."),
            "  She goes."
        );
        // No block: untouched.
        assert_eq!(strip_think_block("She goes."), "She goes.");
        // Unclosed block: untouched.
        assert_eq!(
            strip_think_block("<think>hm She goes."),
            "<think>hm She goes."
        );
        // Think block elsewhere is not stripped.
        assert_eq!(
            strip_think_block("She <think>hm</think> goes."),
            "She <think>hm</think> goes."
        );
    }

    #[test]
    fn clean_reply_trims_strips_think_and_quotes() {
        assert_eq!(
            clean_reply("  <think>r</think>\"She goes.\"  ").as_deref(),
            Some("She goes.")
        );
        assert_eq!(clean_reply("   ").as_deref(), None);
        assert_eq!(
            clean_reply("<think>only thinking</think>   ").as_deref(),
            None
        );
        assert_eq!(clean_reply("\"\"").as_deref(), None);
    }
}
