use std::ops::Range;

fn is_closing(ch: char) -> bool {
    matches!(
        ch,
        '"' | '\'' | ')' | ']' | '}' | '\u{201D}' | '\u{2019}' | '\u{00BB}'
    )
}

fn ends_with_terminator(chars: &[char], range: &Range<usize>) -> bool {
    if range.is_empty() {
        return false;
    }
    let mut end = range.end;
    while end > range.start && is_closing(chars[end - 1]) {
        end -= 1;
    }
    end > range.start && matches!(chars[end - 1], '.' | '!' | '?')
}

fn trimmed(chars: &[char], range: Range<usize>) -> Range<usize> {
    let mut start = range.start;
    let mut end = range.end;
    while start < end && chars[start].is_whitespace() {
        start += 1;
    }
    while end > start && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    start..end
}

/// Returns the char-index range of the sentence containing `caret`.
///
/// `caret` is a char index (insertion position) and is clamped to the text
/// length. If the caret sits right after a sentence's end punctuation and
/// following whitespace (including at the very end of the text), the sentence
/// just before the caret is returned.
pub fn sentence_range(text: &str, caret: usize) -> Option<Range<usize>> {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    if len == 0 {
        return None;
    }
    let caret = caret.min(len);

    // Split into raw sentences: a sentence ends after `.`/`!`/`?` plus any
    // closing quotes/brackets when followed by whitespace or end of text,
    // or at a newline.
    let mut raw: Vec<Range<usize>> = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < len {
        if chars[i] == '\n' {
            raw.push(start..i);
            start = i + 1;
            i += 1;
            continue;
        }
        if matches!(chars[i], '.' | '!' | '?') {
            let mut j = i + 1;
            while j < len && is_closing(chars[j]) {
                j += 1;
            }
            if j == len {
                raw.push(start..j);
                start = j;
                i = j;
                continue;
            }
            if chars[j].is_whitespace() {
                raw.push(start..j);
                start = j;
                i = j;
                continue;
            }
            i += 1;
            continue;
        }
        i += 1;
    }
    if start < len {
        raw.push(start..len);
    }
    if raw.is_empty() {
        return None;
    }

    let sentences: Vec<Range<usize>> = raw
        .into_iter()
        .map(|range| trimmed(&chars, range))
        .filter(|range| !range.is_empty())
        .collect();
    if sentences.is_empty() {
        return None;
    }

    // Caret at the start of a sentence that follows a terminated sentence:
    // prefer the sentence just before it.
    for idx in 1..sentences.len() {
        if caret == sentences[idx].start && ends_with_terminator(&chars, &sentences[idx - 1]) {
            let prev = sentences[idx - 1].clone();
            return finish(&chars, prev);
        }
    }

    // Sentence containing the caret (caret may sit on its end position).
    for range in &sentences {
        if caret >= range.start && caret <= range.end {
            return finish(&chars, range.clone());
        }
    }

    // Caret in whitespace between sentences, or after the last sentence.
    let mut before: Option<Range<usize>> = None;
    for range in &sentences {
        if range.end < caret {
            before = Some(range.clone());
        }
    }
    if let Some(range) = before {
        return finish(&chars, range);
    }

    // Caret before the first sentence (leading whitespace).
    finish(&chars, sentences[0].clone())
}

fn finish(chars: &[char], range: Range<usize>) -> Option<Range<usize>> {
    if range.is_empty() {
        return None;
    }
    if !chars[range.clone()].iter().any(|ch| ch.is_alphabetic()) {
        return None;
    }
    Some(range)
}

#[cfg(test)]
mod tests {
    use super::sentence_range;

    fn text_of(text: &str, range: std::ops::Range<usize>) -> String {
        text.chars()
            .skip(range.start)
            .take(range.end - range.start)
            .collect()
    }

    fn caret_after(text: &str, prefix: &str) -> usize {
        assert!(text.starts_with(prefix), "prefix must start the text");
        prefix.chars().count()
    }

    #[test]
    fn middle_of_second_of_three_sentences() {
        let text = "First sentence here. Second one is bad. Third sentence follows.";
        let caret = caret_after(text, "First sentence here. Second one");
        let range = sentence_range(text, caret).expect("should find a sentence");
        assert_eq!(text_of(text, range), "Second one is bad.");
    }

    #[test]
    fn caret_at_end_of_text() {
        let text = "First sentence. Second sentence bad";
        let caret = text.chars().count();
        let range = sentence_range(text, caret).expect("should find a sentence");
        assert_eq!(text_of(text, range), "Second sentence bad");
    }

    #[test]
    fn caret_right_after_period_and_space() {
        let text = "First sentence. Second sentence.";
        let caret = caret_after(text, "First sentence. ");
        let range = sentence_range(text, caret).expect("should find a sentence");
        assert_eq!(text_of(text, range), "First sentence.");
    }

    #[test]
    fn caret_right_after_period_and_space_at_end_of_text() {
        let text = "Only sentence here. ";
        let caret = text.chars().count();
        let range = sentence_range(text, caret).expect("should find a sentence");
        assert_eq!(text_of(text, range), "Only sentence here.");
    }

    #[test]
    fn newline_separated_lines() {
        let text = "First line is fine\nSecond line is bad\nThird line";
        let caret = caret_after(text, "First line is fine\nSecond line");
        let range = sentence_range(text, caret).expect("should find a sentence");
        assert_eq!(text_of(text, range), "Second line is bad");
    }

    #[test]
    fn abbreviations_are_split_naively() {
        // "e.g." is acceptable to split; do not special-case abbreviations.
        let text = "Use e.g. this one here. Next sentence.";
        let caret = caret_after(text, "Use e.g. this one");
        let range = sentence_range(text, caret).expect("should find a sentence");
        assert_eq!(text_of(text, range), "this one here.");
    }

    #[test]
    fn non_ascii_text() {
        let text = "Café is good. ça va mal ici";
        let caret = caret_after(text, "Café is good. ça va");
        let range = sentence_range(text, caret).expect("should find a sentence");
        assert_eq!(text_of(text, range), "ça va mal ici");
    }

    #[test]
    fn empty_text_returns_none() {
        assert_eq!(sentence_range("", 0), None);
    }

    #[test]
    fn whitespace_only_returns_none() {
        assert_eq!(sentence_range("   \n  ", 2), None);
    }

    #[test]
    fn text_without_letters_returns_none() {
        assert_eq!(sentence_range("123 456.", 4), None);
    }

    #[test]
    fn caret_past_end_clamps() {
        let text = "Short one.";
        let range = sentence_range(text, 999).expect("should clamp and find a sentence");
        assert_eq!(text_of(text, range), "Short one.");
    }

    #[test]
    fn closing_quote_after_punctuation_stays_with_sentence() {
        let text = "She said \"this is bad.\" Next sentence here.";
        let caret = caret_after(text, "She said \"this is");
        let range = sentence_range(text, caret).expect("should find a sentence");
        assert_eq!(text_of(text, range), "She said \"this is bad.\"");
    }

    #[test]
    fn question_and_exclamation_end_sentences() {
        let text = "Is this bad? Yes it is! Fix it now.";
        let caret = caret_after(text, "Is this bad? Yes");
        let range = sentence_range(text, caret).expect("should find a sentence");
        assert_eq!(text_of(text, range), "Yes it is!");
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let text = "  Padded sentence here.  ";
        let caret = caret_after(text, "  Padded sentence");
        let range = sentence_range(text, caret).expect("should find a sentence");
        assert_eq!(text_of(text, range), "Padded sentence here.");
    }

    #[test]
    fn caret_at_start_returns_first_sentence() {
        let text = "First sentence. Second sentence.";
        let range = sentence_range(text, 0).expect("should find a sentence");
        assert_eq!(text_of(text, range), "First sentence.");
    }
}
