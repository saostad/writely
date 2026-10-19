#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::{copy_suggestion, layered_copy_text, paste_suggestion};
#[cfg(target_os = "windows")]
pub use windows::{copy_suggestion, layered_copy_text, paste_suggestion};

/// True when `text` looks like a whole-line copy editors make when
/// nothing is selected: a single non-blank line plus its line break.
/// Two trailing breaks aren't a single-line copy, so they return false.
pub fn looks_like_line_copy(text: &str) -> bool {
    let Some(line) = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
    else {
        return false;
    };
    !line.trim().is_empty() && !line.contains('\n')
}

#[derive(Debug)]
pub enum CopyResult {
    /// The newly copied text.
    Text(String),
    /// Nothing new was copied (or it was empty): the user should select text.
    NothingNew,
}

#[cfg(test)]
mod tests {
    use super::looks_like_line_copy;

    #[test]
    fn whole_line_copies_are_detected() {
        assert!(looks_like_line_copy("Hello world\n"));
        assert!(looks_like_line_copy("Hello world\r\n"));
    }

    #[test]
    fn non_line_copies_are_not_detected() {
        assert!(!looks_like_line_copy("Hello world"));
        assert!(!looks_like_line_copy("a\nb\n"));
        assert!(!looks_like_line_copy("\n"));
        assert!(!looks_like_line_copy("   \r\n"));
        // Two trailing breaks aren't a single-line copy.
        assert!(!looks_like_line_copy("Hello\n\n"));
    }
}
