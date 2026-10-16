pub mod sentence;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::{clear_focused_if, set_focused};
#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos as macos_helpers;
#[cfg(target_os = "windows")]
mod windows;

use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaretRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// True for pixel units (Windows), false for logical points (macOS).
    pub physical: bool,
}

#[cfg(target_os = "macos")]
pub(crate) type PlatformHandle = macos::PlatformHandle;
#[cfg(target_os = "windows")]
pub(crate) type PlatformHandle = windows::PlatformHandle;
#[cfg(target_os = "linux")]
pub(crate) type PlatformHandle = linux::PlatformHandle;
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub(crate) struct PlatformHandle;

pub struct FieldSnapshot {
    pub pid: u32,
    pub text: String,
    /// Char index of the caret (insertion position) into `text`. Meaningful
    /// only when `caret_known` is true; otherwise 0.
    pub caret: usize,
    /// False when the caret offset couldn't be read (e.g. Firefox): `text`
    /// is then the whole field and the click checks it as a whole.
    pub caret_known: bool,
    pub caret_rect: CaretRect,
    /// Char-index range of the current selection into `text`, `Some` only
    /// for a non-empty selection.
    pub selection: Option<Range<usize>>,
    /// UTF-16 offset of the start of `text` in the field. `text` is a
    /// bounded window around the caret, not the whole field.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub window_start_utf16: usize,
    /// Total length of the field in UTF-16 units.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub field_len_utf16: usize,
    pub(crate) handle: PlatformHandle,
}

/// UTF-16 units read on each side of the caret for the snapshot window.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const WINDOW_RADIUS_UTF16: usize = 1000;

/// Longest selection (in characters) that gets checked in one click.
pub const MAX_SELECTION_CHARS: usize = 2000;

/// Shown when the trimmed selection exceeds `MAX_SELECTION_CHARS`.
pub const SELECTION_TOO_LONG_MESSAGE: &str =
    "That selection is too long. Select up to 2,000 characters and try again.";

/// Whole-field reads above this many UTF-16 units are refused.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const WHOLE_READ_LIMIT_UTF16: usize = 20_000;

/// Number of UTF-16 code units in `text`.
pub(crate) fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Whether the snapshot covers the whole field, in which case a whole-field
/// write fallback is allowed.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn covers_whole_field(snap: &FieldSnapshot) -> bool {
    snap.window_start_utf16 == 0 && utf16_len(&snap.text) == snap.field_len_utf16
}

/// The `[start, start + len)` UTF-16 window around `caret_utf16`, clamped to
/// a field of `total_utf16` units.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn window_for_caret(caret_utf16: usize, total_utf16: usize) -> (usize, usize) {
    let start = caret_utf16.saturating_sub(WINDOW_RADIUS_UTF16);
    let end = (caret_utf16 + WINDOW_RADIUS_UTF16).min(total_utf16);
    (start, end.saturating_sub(start))
}

/// The UTF-16 window covering the whole selection plus `WINDOW_RADIUS_UTF16`
/// units on each side, clamped to a field of `total_utf16` units.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn window_for_selection(
    sel_start_utf16: usize,
    sel_end_utf16: usize,
    total_utf16: usize,
) -> (usize, usize) {
    let start = sel_start_utf16.saturating_sub(WINDOW_RADIUS_UTF16);
    let end = sel_end_utf16
        .saturating_add(WINDOW_RADIUS_UTF16)
        .min(total_utf16);
    (start, end.saturating_sub(start))
}

/// What a click should check for the given selection.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckedSelection {
    /// Check (and later replace) this trimmed char range.
    Use(Range<usize>),
    /// The selection is empty or only whitespace: fall back to the sentence
    /// at the caret.
    Fallback,
    /// The trimmed selection exceeds `MAX_SELECTION_CHARS`: show
    /// `SELECTION_TOO_LONG_MESSAGE` and send nothing.
    TooLong,
}

/// Trims whitespace from both ends of `selection` (char indices into `text`)
/// and applies the length limit.
pub fn check_selection(text: &str, selection: Range<usize>) -> CheckedSelection {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut start = selection.start.min(len);
    let mut end = selection.end.min(len);
    if start > end {
        std::mem::swap(&mut start, &mut end);
    }
    while start < end && chars[start].is_whitespace() {
        start += 1;
    }
    while end > start && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    if start >= end {
        return CheckedSelection::Fallback;
    }
    if end - start > MAX_SELECTION_CHARS {
        return CheckedSelection::TooLong;
    }
    CheckedSelection::Use(start..end)
}

/// What a click should check when the caret offset is unknown but the whole
/// field text is readable (e.g. Firefox).
#[derive(Debug, Clone, PartialEq)]
pub enum NoCaretDecision {
    /// Check (and later replace) this trimmed char range of the whole field.
    CheckWholeField(Range<usize>),
    /// The field is empty or only whitespace: show "Type a sentence first."
    Empty,
    /// The trimmed field exceeds `MAX_SELECTION_CHARS`: the clipboard
    /// fallback (or the too-long message where it isn't available) applies.
    TooLong,
}

/// Trims `text` (the whole field) and applies the length limit.
pub fn check_without_caret(text: &str) -> NoCaretDecision {
    let chars: Vec<char> = text.chars().collect();
    let mut start = 0;
    let mut end = chars.len();
    while start < end && chars[start].is_whitespace() {
        start += 1;
    }
    while end > start && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    if start >= end {
        return NoCaretDecision::Empty;
    }
    if end - start > MAX_SELECTION_CHARS {
        return NoCaretDecision::TooLong;
    }
    NoCaretDecision::CheckWholeField(start..end)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaretInfo {
    pub pid: u32,
    pub caret_rect: CaretRect,
}

/// Converts a char index into a UTF-16 code-unit offset into `text`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn char_to_utf16_offset(text: &str, char_index: usize) -> usize {
    text.chars().take(char_index).map(|ch| ch.len_utf16()).sum()
}

/// Converts a UTF-16 code-unit offset into a char index into `text`.
///
/// An offset that lands inside a surrogate pair or past the end clamps to
/// the next char boundary / the end of the text.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn utf16_to_char_offset(text: &str, utf16_offset: usize) -> usize {
    let mut units = 0usize;
    for (index, ch) in text.chars().enumerate() {
        if units >= utf16_offset {
            return index;
        }
        units += ch.len_utf16();
    }
    text.chars().count()
}

/// Builds the expected full text after replacing `range` (char indices).
pub(crate) fn replaced_text(text: &str, range: &Range<usize>, new_text: &str) -> String {
    let mut out = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index == range.start {
            out.push_str(new_text);
        }
        if index < range.start || index >= range.end {
            out.push(ch);
        }
    }
    if range.start >= text.chars().count() {
        out.push_str(new_text);
    }
    out
}

/// Whether the currently focused element is a secure (password) field.
/// Unknown or unreadable focus counts as secure: the copy-and-paste
/// fallback must never run for a password field.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) fn focused_is_secure() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::focused_is_secure()
    }
    #[cfg(target_os = "windows")]
    {
        windows::focused_is_secure()
    }
}

pub fn check_permission(prompt: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        macos::check_permission(prompt)
    }
    #[cfg(target_os = "windows")]
    {
        windows::check_permission(prompt)
    }
    #[cfg(target_os = "linux")]
    {
        linux::check_permission(prompt)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = prompt;
        Ok(())
    }
}

pub fn caret_info() -> Result<Option<CaretInfo>, String> {
    #[cfg(target_os = "macos")]
    {
        macos::caret_info()
    }
    #[cfg(target_os = "windows")]
    {
        windows::caret_info()
    }
    #[cfg(target_os = "linux")]
    {
        linux::caret_info()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Ok(None)
    }
}

pub fn focused_field() -> Result<Option<FieldSnapshot>, String> {
    #[cfg(target_os = "macos")]
    {
        macos::focused_field()
    }
    #[cfg(target_os = "windows")]
    {
        windows::focused_field()
    }
    #[cfg(target_os = "linux")]
    {
        linux::focused_field()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Ok(None)
    }
}

/// Whether process `pid` is a terminal app, where Replace cannot work.
/// Windows looks the exe up (a failed lookup is logged and treated as
/// not a terminal); other platforms are never terminals.
pub(crate) fn is_terminal_app(pid: u32) -> bool {
    #[cfg(target_os = "windows")]
    {
        windows::is_terminal_app(pid)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = pid;
        false
    }
}

/// Selects `range` in the field so a paste replaces exactly that text.
/// Windows only; other platforms have no caller.
#[cfg(target_os = "windows")]
pub(crate) fn select_range(snap: &FieldSnapshot, range: Range<usize>) -> Result<(), String> {
    windows::select_range(snap, range)
}

/// The selected text in the focused terminal, read through UIA with no
/// keystrokes. Windows only; other platforms have no caller.
#[cfg(target_os = "windows")]
pub(crate) fn terminal_selection_text() -> Result<Option<String>, String> {
    windows::terminal_selection_text()
}

pub fn replace_range(
    snap: &FieldSnapshot,
    range: Range<usize>,
    new_text: &str,
) -> Result<(), String> {
    let char_count = snap.text.chars().count();
    if range.start > range.end || range.end > char_count {
        return Err(
            "The text changed before Writely could replace it. Click the Writely icon again."
                .to_string(),
        );
    }
    let current = read_current_text(snap)?;
    if current != snap.text {
        return Err(
            "The text changed before Writely could replace it. Click the Writely icon again."
                .to_string(),
        );
    }
    #[cfg(target_os = "macos")]
    {
        macos::replace_range(snap, range, new_text)
    }
    #[cfg(target_os = "windows")]
    {
        windows::replace_range(snap, range, new_text)
    }
    #[cfg(target_os = "linux")]
    {
        linux::replace_range(snap, range, new_text)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = (snap, range, new_text);
        Err("Writely cannot replace text on this platform.".to_string())
    }
}

fn read_current_text(snap: &FieldSnapshot) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        macos::read_at(&snap.handle, snap, utf16_len(&snap.text))
    }
    #[cfg(target_os = "windows")]
    {
        windows::read_text(&snap.handle)
    }
    #[cfg(target_os = "linux")]
    {
        linux::read_text(&snap.handle)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = snap;
        Ok(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        char_to_utf16_offset, check_selection, replaced_text, utf16_len, utf16_to_char_offset,
        window_for_caret, CheckedSelection, MAX_SELECTION_CHARS,
    };

    #[test]
    fn utf16_round_trip_with_emoji_and_accents() {
        let text = "Café 😀 done";
        // "Café " is 5 chars / 5 UTF-16 units; the emoji is 1 char / 2 units.
        assert_eq!(char_to_utf16_offset(text, 5), 5);
        assert_eq!(char_to_utf16_offset(text, 6), 7);
        assert_eq!(utf16_to_char_offset(text, 5), 5);
        assert_eq!(utf16_to_char_offset(text, 7), 6);
        assert_eq!(utf16_to_char_offset(text, 999), text.chars().count());
    }

    #[test]
    fn utf16_offset_inside_surrogate_pair_clamps_to_next_char() {
        let text = "a😀b";
        assert_eq!(utf16_to_char_offset(text, 2), 2);
    }

    #[test]
    fn replaced_text_splices_by_char_index() {
        assert_eq!(replaced_text("Café bad.", &(5..8), "good"), "Café good.");
        assert_eq!(replaced_text("hello", &(5..5), "!"), "hello!");
    }

    #[test]
    fn window_clamps_at_start() {
        let (start, len) = window_for_caret(10, 5000);
        assert_eq!(start, 0);
        assert_eq!(len, 10 + super::WINDOW_RADIUS_UTF16);
    }

    #[test]
    fn window_clamps_at_end() {
        let total = 5000;
        let caret = 4990;
        let (start, len) = window_for_caret(caret, total);
        assert_eq!(start, caret - super::WINDOW_RADIUS_UTF16);
        assert_eq!(start + len, total);
        assert!(start <= caret && caret <= start + len);
    }

    #[test]
    fn window_centred_on_caret() {
        let caret = 3000;
        let (start, len) = window_for_caret(caret, 9000);
        assert_eq!(start, caret - super::WINDOW_RADIUS_UTF16);
        assert_eq!(len, 2 * super::WINDOW_RADIUS_UTF16);
        assert!(start <= caret && caret <= start + len);
    }

    #[test]
    fn caret_maps_inside_window_with_emoji_at_edge() {
        // An emoji (1 char, 2 UTF-16 units) sits right at the window edge.
        let text = format!("{}ab", "😀".repeat(600));
        let total = utf16_len(&text);
        assert_eq!(total, 600 * 2 + 2);
        // Caret just after the emoji run, in UTF-16 units.
        let caret_utf16 = 600 * 2;
        let (start, len) = window_for_caret(caret_utf16, total);
        assert_eq!(start, caret_utf16 - super::WINDOW_RADIUS_UTF16);
        assert!(start <= caret_utf16 && caret_utf16 <= start + len);
        // The window slice decoded from UTF-16 maps the caret to the same
        // char index as the full text does.
        let units: Vec<u16> = text.encode_utf16().collect();
        let window_text =
            String::from_utf16(&units[start..start + len]).expect("edge is char-aligned here");
        assert_eq!(
            utf16_to_char_offset(&window_text, caret_utf16 - start),
            utf16_to_char_offset(&text, caret_utf16) - utf16_to_char_offset(&text, start)
        );
        assert_eq!(utf16_to_char_offset(&text, caret_utf16), 600);
        assert_eq!(char_to_utf16_offset(&text, 600), caret_utf16);
        // A UTF-16 offset landing inside a surrogate pair clamps forward to
        // the next char boundary instead of splitting the emoji.
        assert_eq!(utf16_to_char_offset(&text, 1), 1);
    }

    #[test]
    fn selection_uses_trimmed_multi_sentence_range() {
        let text = "No notification needed for this repo. Are you ready? Today are a good day.";
        // Select all three sentences with extra surrounding whitespace.
        let start = text.find("No notification").expect("fixture contains text");
        let start_chars = text[..start].chars().count();
        let selection = start_chars.saturating_sub(2)..text.chars().count();
        let checked = check_selection(text, selection);
        let expected_start: usize = text
            .chars()
            .take_while(|ch| ch.is_whitespace())
            .count()
            .min(start_chars);
        match checked {
            CheckedSelection::Use(range) => {
                let got: String = text
                    .chars()
                    .skip(range.start)
                    .take(range.end - range.start)
                    .collect();
                assert_eq!(
                    got,
                    "No notification needed for this repo. Are you ready? Today are a good day."
                );
                assert_eq!(range.start, expected_start);
            }
            other => panic!("expected Use, got {other:?}"),
        }
    }

    #[test]
    fn selection_trims_surrounding_whitespace() {
        let text = "  She go to school.  ";
        let checked = check_selection(text, 0..text.chars().count());
        match checked {
            CheckedSelection::Use(range) => {
                let got: String = text
                    .chars()
                    .skip(range.start)
                    .take(range.end - range.start)
                    .collect();
                assert_eq!(got, "She go to school.");
            }
            other => panic!("expected Use, got {other:?}"),
        }
    }

    #[test]
    fn whitespace_only_selection_falls_back_to_sentence() {
        let text = "First sentence.   Second sentence here.";
        // A selection covering only the whitespace between the sentences.
        let first_end = text.find('.').expect("fixture has a period") + 1;
        let first_end_chars = text[..first_end].chars().count();
        let checked = check_selection(text, first_end_chars..first_end_chars + 3);
        assert_eq!(checked, CheckedSelection::Fallback);
        assert_eq!(check_selection(text, 0..0), CheckedSelection::Fallback);
    }

    #[test]
    fn too_long_selection_is_rejected() {
        let text = "a".repeat(MAX_SELECTION_CHARS + 500);
        assert_eq!(
            check_selection(&text, 0..text.chars().count()),
            CheckedSelection::TooLong
        );
        let ok_text = "b".repeat(MAX_SELECTION_CHARS);
        match check_selection(&ok_text, 0..ok_text.chars().count()) {
            CheckedSelection::Use(range) => {
                assert_eq!(range.end - range.start, MAX_SELECTION_CHARS)
            }
            other => panic!("expected Use at the limit, got {other:?}"),
        }
    }

    #[test]
    fn whole_field_check_without_caret() {
        use super::{check_without_caret, NoCaretDecision};
        // A short field is checked as a whole, trimmed.
        match check_without_caret("  Today are a good day.  ") {
            NoCaretDecision::CheckWholeField(range) => {
                let text = "  Today are a good day.  ";
                let got: String = text
                    .chars()
                    .skip(range.start)
                    .take(range.end - range.start)
                    .collect();
                assert_eq!(got, "Today are a good day.");
            }
            other => panic!("expected CheckWholeField, got {other:?}"),
        }
        // Empty or whitespace-only fields have nothing to check.
        assert_eq!(check_without_caret(""), NoCaretDecision::Empty);
        assert_eq!(check_without_caret("   \n  "), NoCaretDecision::Empty);
        // An over-long field goes to the clipboard fallback.
        let long = "a".repeat(MAX_SELECTION_CHARS + 1);
        assert_eq!(check_without_caret(&long), NoCaretDecision::TooLong);
        // Exactly at the limit is still checked.
        let at_limit = "b".repeat(MAX_SELECTION_CHARS);
        match check_without_caret(&at_limit) {
            NoCaretDecision::CheckWholeField(range) => {
                assert_eq!(range.end - range.start, MAX_SELECTION_CHARS)
            }
            other => panic!("expected CheckWholeField at the limit, got {other:?}"),
        }
    }
}
