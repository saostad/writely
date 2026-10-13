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
    /// Char index of the caret (insertion position) into `text`.
    pub caret: usize,
    pub caret_rect: CaretRect,
    /// UTF-16 offset of the start of `text` in the field. `text` is a
    /// bounded window around the caret, not the whole field.
    pub window_start_utf16: usize,
    /// Total length of the field in UTF-16 units.
    pub field_len_utf16: usize,
    pub(crate) handle: PlatformHandle,
}

/// UTF-16 units read on each side of the caret for the snapshot window.
pub(crate) const WINDOW_RADIUS_UTF16: usize = 1000;

/// Whole-field reads above this many UTF-16 units are refused.
pub(crate) const WHOLE_READ_LIMIT_UTF16: usize = 20_000;

/// Number of UTF-16 code units in `text`.
pub(crate) fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Whether the snapshot covers the whole field, in which case a whole-field
/// write fallback is allowed.
pub(crate) fn covers_whole_field(snap: &FieldSnapshot) -> bool {
    snap.window_start_utf16 == 0 && utf16_len(&snap.text) == snap.field_len_utf16
}

/// The `[start, start + len)` UTF-16 window around `caret_utf16`, clamped to
/// a field of `total_utf16` units.
pub(crate) fn window_for_caret(caret_utf16: usize, total_utf16: usize) -> (usize, usize) {
    let start = caret_utf16.saturating_sub(WINDOW_RADIUS_UTF16);
    let end = (caret_utf16 + WINDOW_RADIUS_UTF16).min(total_utf16);
    (start, end.saturating_sub(start))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaretInfo {
    pub pid: u32,
    pub caret_rect: CaretRect,
}

/// Converts a char index into a UTF-16 code-unit offset into `text`.
pub(crate) fn char_to_utf16_offset(text: &str, char_index: usize) -> usize {
    text.chars().take(char_index).map(|ch| ch.len_utf16()).sum()
}

/// Converts a UTF-16 code-unit offset into a char index into `text`.
///
/// An offset that lands inside a surrogate pair or past the end clamps to
/// the next char boundary / the end of the text.
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
        char_to_utf16_offset, replaced_text, utf16_len, utf16_to_char_offset, window_for_caret,
        WINDOW_RADIUS_UTF16,
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
        assert_eq!(len, 10 + WINDOW_RADIUS_UTF16);
    }

    #[test]
    fn window_clamps_at_end() {
        let total = 5000;
        let caret = 4990;
        let (start, len) = window_for_caret(caret, total);
        assert_eq!(start, caret - WINDOW_RADIUS_UTF16);
        assert_eq!(start + len, total);
        assert!(start <= caret && caret <= start + len);
    }

    #[test]
    fn window_centred_on_caret() {
        let caret = 3000;
        let (start, len) = window_for_caret(caret, 9000);
        assert_eq!(start, caret - WINDOW_RADIUS_UTF16);
        assert_eq!(len, 2 * WINDOW_RADIUS_UTF16);
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
        assert_eq!(start, caret_utf16 - WINDOW_RADIUS_UTF16);
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
}
