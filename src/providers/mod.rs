pub mod gemini;
pub mod local;
pub mod ollama;
pub(crate) mod stream;
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

    pub fn progress_text(self) -> &'static str {
        match self {
            RewriteStyle::Friendly => "Making it friendlier…",
            RewriteStyle::Formal => "Making it more formal…",
            RewriteStyle::Professional => "Making it more professional…",
            RewriteStyle::Shorter => "Making it shorter…",
            RewriteStyle::Longer => "Making it longer…",
        }
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

/// The provider dispatch shared by every request: same model, effort,
/// temperature 0, key header and error parsing; only the text and the system
/// instruction differ. The key is `None` for providers that need no key;
/// for Gemini and Together a missing key or model is an error for the popup.
fn dispatch_stream(
    settings: &AppSettings,
    key: Option<&str>,
    text: &str,
    instruction: &str,
    on_partial: &mut dyn FnMut(&str),
) -> Result<String, String> {
    match settings.provider {
        Provider::Gemini => gemini::suggest_stream(settings, key, text, instruction, on_partial),
        Provider::Together => {
            together::suggest_stream(settings, key, text, instruction, on_partial)
        }
        Provider::Ollama => ollama::suggest_stream(settings, text, instruction, on_partial),
        Provider::Local => local::suggest_stream(settings, text, instruction, on_partial),
    }
}

/// One entry point for every provider: the grammar instruction, temperature 0
/// and UNCLEAR handling.
#[allow(dead_code)] // Non-streaming wrapper; the workers call suggest_stream directly.
pub fn suggest(settings: &AppSettings, key: Option<&str>, text: &str) -> Result<String, String> {
    suggest_stream(settings, key, text, &mut |_| {})
}

pub fn suggest_stream(
    settings: &AppSettings,
    key: Option<&str>,
    text: &str,
    on_partial: &mut dyn FnMut(&str),
) -> Result<String, String> {
    dispatch_stream(settings, key, text, gemini::SYSTEM_INSTRUCTION, on_partial)
}

/// Same dispatch as [`suggest`], but with the given rewrite style's system
/// instruction instead of the grammar one.
#[allow(dead_code)] // Non-streaming wrapper; the workers call rewrite_stream directly.
pub fn rewrite(
    settings: &AppSettings,
    key: Option<&str>,
    text: &str,
    style: RewriteStyle,
) -> Result<String, String> {
    rewrite_stream(settings, key, text, style, &mut |_| {})
}

pub fn rewrite_stream(
    settings: &AppSettings,
    key: Option<&str>,
    text: &str,
    style: RewriteStyle,
    on_partial: &mut dyn FnMut(&str),
) -> Result<String, String> {
    let instruction = style.instruction();
    dispatch_stream(settings, key, text, &instruction, on_partial)
}

#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub const COMPOSE_INSTRUCTION: &str = "You write English text for the user. The user describes what they want to write; write that text, ready to paste. Use correct grammar, spelling and punctuation. Keep it as short as the request allows. Never invent names, dates, numbers or other facts the user didn't give; use a placeholder in square brackets instead, like [name]. Return only the text, with no title, quotes or explanation. If the request is not understandable (random letters or gibberish), reply with exactly UNCLEAR and nothing else.";

/// Writes new text from the user's description of what they want to write.
#[allow(dead_code)] // Non-streaming wrapper; the workers call compose_stream directly.
pub fn compose(settings: &AppSettings, key: Option<&str>, request: &str) -> Result<String, String> {
    compose_stream(settings, key, request, &mut |_| {})
}

#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub fn compose_stream(
    settings: &AppSettings,
    key: Option<&str>,
    request: &str,
    on_partial: &mut dyn FnMut(&str),
) -> Result<String, String> {
    dispatch_stream(settings, key, request, COMPOSE_INSTRUCTION, on_partial)
}

#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub const CHANGE_INSTRUCTION: &str = "You rewrite English text the way the user asks. The message has the user's request after \"Request:\" and the text after \"Text:\". Apply the request, and fix grammar, spelling and punctuation. Keep the meaning and every fact unless the request asks to change them; never add facts the user didn't give. Keep line breaks and paragraph breaks unless the request asks otherwise. Return only the rewritten text, with no quotes or explanation. If the request or the text is not understandable, reply with exactly UNCLEAR and nothing else.";

/// The user message for a describe-a-change request: the request, then the
/// text to change.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub fn change_message(request: &str, text: &str) -> String {
    format!("Request: {request}\n\nText:\n{text}")
}

/// Rewrites `text` the way `request` asks.
#[allow(dead_code)] // Non-streaming wrapper; the workers call change_stream directly.
pub fn change(
    settings: &AppSettings,
    key: Option<&str>,
    text: &str,
    request: &str,
) -> Result<String, String> {
    change_stream(settings, key, text, request, &mut |_| {})
}

#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub fn change_stream(
    settings: &AppSettings,
    key: Option<&str>,
    text: &str,
    request: &str,
    on_partial: &mut dyn FnMut(&str),
) -> Result<String, String> {
    dispatch_stream(
        settings,
        key,
        &change_message(request, text),
        CHANGE_INSTRUCTION,
        on_partial,
    )
}

/// Longest accepted compose/describe-a-change request, in chars.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub const MAX_REQUEST_CHARS: usize = 1000;

#[cfg(test)]
mod tests {
    use super::{
        change, change_message, change_stream, classify_suggestion, clean_reply, compose,
        compose_stream, rewrite, rewrite_stream, strip_think_block, suggest, suggest_stream,
        RewriteStyle, SuggestionOutcome, CHANGE_INSTRUCTION, COMPOSE_INSTRUCTION,
    };
    use crate::config::AppSettings;

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

    #[test]
    fn change_message_puts_request_before_text() {
        assert_eq!(
            change_message("sound confident", "We might go."),
            "Request: sound confident\n\nText:\nWe might go."
        );
    }

    #[test]
    fn compose_and_change_instructions_end_in_unclear() {
        for instruction in [COMPOSE_INSTRUCTION, CHANGE_INSTRUCTION] {
            assert!(
                instruction.contains("UNCLEAR"),
                "instruction misses UNCLEAR: {instruction}"
            );
        }
    }

    #[test]
    fn stream_twins_share_dispatch_with_non_stream_wrappers() {
        // Default settings are Gemini with a model set, so a missing key
        // fails in dispatch before any network use, whatever instruction
        // each entry point passes. The instructions themselves are shared
        // by construction: each non-stream wrapper calls its `_stream`
        // twin, which is the only place the instruction is chosen.
        let settings = AppSettings::default();
        let mut partials: Vec<String> = Vec::new();
        let streamed = [
            suggest_stream(&settings, None, "She go.", &mut |text: &str| {
                partials.push(text.to_string())
            }),
            rewrite_stream(
                &settings,
                None,
                "She go.",
                RewriteStyle::Friendly,
                &mut |_| {},
            ),
            compose_stream(&settings, None, "a thank-you note", &mut |_| {}),
            change_stream(&settings, None, "She go.", "sound confident", &mut |_| {}),
        ];
        let plain = [
            suggest(&settings, None, "She go."),
            rewrite(&settings, None, "She go.", RewriteStyle::Friendly),
            compose(&settings, None, "a thank-you note"),
            change(&settings, None, "She go.", "sound confident"),
        ];
        assert_eq!(streamed, plain);
        for result in &streamed {
            assert_eq!(
                result.as_ref().expect_err("missing key should fail"),
                "Add your Gemini API key first: click the Writely menu-bar icon › Settings…"
            );
        }
        assert!(partials.is_empty(), "{partials:?}");
    }

    #[test]
    fn rewrite_progress_texts() {
        let texts = [
            (RewriteStyle::Friendly, "Making it friendlier…"),
            (RewriteStyle::Formal, "Making it more formal…"),
            (RewriteStyle::Professional, "Making it more professional…"),
            (RewriteStyle::Shorter, "Making it shorter…"),
            (RewriteStyle::Longer, "Making it longer…"),
        ];
        for (style, expected) in texts {
            assert_eq!(style.progress_text(), expected);
        }
    }
}
