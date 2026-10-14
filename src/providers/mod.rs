pub mod gemini;
pub mod ollama;
pub mod together;

pub use crate::config::{AppSettings, Provider};

pub const UNCLEAR_MARKER: &str = "UNCLEAR";

pub const UNCLEAR_MESSAGE: &str =
    "Writely couldn't understand this text, so there's no suggestion.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewriteStyle {
    Friendly,
    Formal,
    Professional,
    Shorter,
    Longer,
}

impl RewriteStyle {
    pub const ALL: [RewriteStyle; 5] = [
        RewriteStyle::Friendly,
        RewriteStyle::Formal,
        RewriteStyle::Professional,
        RewriteStyle::Shorter,
        RewriteStyle::Longer,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RewriteStyle::Friendly => "Friendly",
            RewriteStyle::Formal => "Formal",
            RewriteStyle::Professional => "Professional",
            RewriteStyle::Shorter => "Shorter",
            RewriteStyle::Longer => "Longer",
        }
    }

    pub fn instruction(self) -> String {
        let goal = match self {
            RewriteStyle::Friendly => "to sound warm, friendly and conversational",
            RewriteStyle::Formal => "to sound formal and polite",
            RewriteStyle::Professional => {
                "to sound clear, confident and professional, suitable for work"
            }
            RewriteStyle::Shorter => {
                "to be noticeably shorter and more concise, keeping the key points"
            }
            RewriteStyle::Longer => {
                "to be somewhat longer and more detailed, expanding only on what is already said"
            }
        };
        format!("You rewrite English text. Fix its grammar, spelling and punctuation, and rewrite it {goal}. Keep the original meaning and every fact; never add new facts. Keep line breaks and paragraph breaks as they are. Return only the rewritten text, with no quotes or explanation. If the text is not understandable English (random letters or gibberish), reply with exactly UNCLEAR and nothing else.")
    }
}

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
        Provider::Gemini => gemini::suggest(settings, key, text, gemini::SYSTEM_INSTRUCTION),
        Provider::Together => together::suggest(settings, key, text, gemini::SYSTEM_INSTRUCTION),
        Provider::Ollama => ollama::suggest(settings, text, gemini::SYSTEM_INSTRUCTION),
    }
}

/// Same dispatch as [`suggest`], but with the given rewrite style's system
/// instruction instead of the grammar one. Everything else (model, effort,
/// temperature 0, key header, error parsing) stays the same.
pub fn rewrite(
    settings: &AppSettings,
    key: Option<&str>,
    text: &str,
    style: RewriteStyle,
) -> Result<String, String> {
    let instruction = style.instruction();
    match settings.provider {
        Provider::Gemini => gemini::suggest(settings, key, text, &instruction),
        Provider::Together => together::suggest(settings, key, text, &instruction),
        Provider::Ollama => ollama::suggest(settings, text, &instruction),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_suggestion, clean_reply, strip_think_block, RewriteStyle, SuggestionOutcome,
    };

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

    #[test]
    fn rewrite_style_labels_and_order() {
        assert_eq!(
            RewriteStyle::ALL.map(RewriteStyle::label),
            ["Friendly", "Formal", "Professional", "Shorter", "Longer"]
        );
    }

    #[test]
    fn rewrite_instructions_carry_goal_and_unclear() {
        let goals = [
            (
                RewriteStyle::Friendly,
                "to sound warm, friendly and conversational",
            ),
            (RewriteStyle::Formal, "to sound formal and polite"),
            (
                RewriteStyle::Professional,
                "to sound clear, confident and professional, suitable for work",
            ),
            (
                RewriteStyle::Shorter,
                "to be noticeably shorter and more concise, keeping the key points",
            ),
            (
                RewriteStyle::Longer,
                "to be somewhat longer and more detailed, expanding only on what is already said",
            ),
        ];
        for (style, goal) in goals {
            let instruction = style.instruction();
            assert!(
                instruction.contains(goal),
                "instruction for {style:?} misses its goal"
            );
            assert!(
                instruction.contains("UNCLEAR"),
                "instruction for {style:?} misses UNCLEAR"
            );
            assert!(
                instruction.starts_with("You rewrite English text."),
                "instruction for {style:?} has the wrong shape"
            );
        }
    }
}
