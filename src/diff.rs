/// Word-level diff between the checked text and the suggestion, for the
/// popup's "what changed" view.
///
/// Tokens are words plus whitespace/punctuation runs; the diff is a plain
/// LCS (dynamic programming) over tokens. Inputs are at most
/// `MAX_SELECTION_CHARS`, so O(n·m) on tokens is fine.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum DiffOp {
    /// Text in both, shown in `labelColor`.
    Equal(String),
    /// Removed text, shown in red with a strikethrough.
    Delete(String),
    /// Inserted text, shown in green, semibold.
    Insert(String),
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn token_kind(ch: char) -> u8 {
    if ch.is_whitespace() {
        0
    } else if ch.is_alphanumeric() || ch == '\'' || ch == '_' {
        1
    } else {
        2
    }
}

/// Splits `text` into word / whitespace / punctuation runs. Pure so the popup
/// and the tests share it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut current_kind: Option<u8> = None;
    for ch in text.chars() {
        let kind = token_kind(ch);
        if current_kind == Some(kind) {
            current.push(ch);
        } else {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            current.push(ch);
            current_kind = Some(kind);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Diffs `old` against `new` at the token level. Adjacent ops of the same
/// kind are merged into single runs.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn diff_words(old: &str, new: &str) -> Vec<DiffOp> {
    let a = tokenize(old);
    let b = tokenize(new);
    let n = a.len();
    let m = b.len();
    // LCS length table.
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if a[i] == b[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    // Backtrack into per-token ops.
    let mut token_ops: Vec<(u8, String)> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            token_ops.push((0, a[i].clone()));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            token_ops.push((1, a[i].clone()));
            i += 1;
        } else {
            token_ops.push((2, b[j].clone()));
            j += 1;
        }
    }
    while i < n {
        token_ops.push((1, a[i].clone()));
        i += 1;
    }
    while j < m {
        token_ops.push((2, b[j].clone()));
        j += 1;
    }
    // Merge adjacent ops of the same kind.
    let mut ops: Vec<DiffOp> = Vec::new();
    for (kind, text) in token_ops {
        let same = matches!(
            (ops.last(), kind),
            (Some(DiffOp::Equal(_)), 0)
                | (Some(DiffOp::Delete(_)), 1)
                | (Some(DiffOp::Insert(_)), 2)
        );
        if same {
            match ops.last_mut() {
                Some(DiffOp::Equal(s)) | Some(DiffOp::Delete(s)) | Some(DiffOp::Insert(s)) => {
                    s.push_str(&text);
                }
                None => {}
            }
        } else {
            ops.push(match kind {
                0 => DiffOp::Equal(text),
                1 => DiffOp::Delete(text),
                _ => DiffOp::Insert(text),
            });
        }
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::{diff_words, tokenize, DiffOp};

    #[test]
    fn identical_text_is_all_equal() {
        let ops = diff_words("Today is a good day.", "Today is a good day.");
        assert_eq!(ops, vec![DiffOp::Equal("Today is a good day.".to_string())]);
    }

    #[test]
    fn one_word_change_is_delete_plus_insert() {
        let ops = diff_words("Today are a good day.", "Today is a good day.");
        assert_eq!(
            ops,
            vec![
                DiffOp::Equal("Today ".to_string()),
                DiffOp::Delete("are".to_string()),
                DiffOp::Insert("is".to_string()),
                DiffOp::Equal(" a good day.".to_string()),
            ]
        );
    }

    #[test]
    fn insertion_is_found() {
        let ops = diff_words("She goes home.", "She always goes home.");
        assert_eq!(
            ops,
            vec![
                DiffOp::Equal("She ".to_string()),
                DiffOp::Insert("always ".to_string()),
                DiffOp::Equal("goes home.".to_string()),
            ]
        );
    }

    #[test]
    fn deletion_is_found() {
        let ops = diff_words("She always goes home.", "She goes home.");
        assert_eq!(
            ops,
            vec![
                DiffOp::Equal("She ".to_string()),
                DiffOp::Delete("always ".to_string()),
                DiffOp::Equal("goes home.".to_string()),
            ]
        );
    }

    #[test]
    fn punctuation_splits_but_whitespace_preserved() {
        assert_eq!(
            tokenize("Hi,  you!"),
            vec![
                "Hi".to_string(),
                ",".to_string(),
                "  ".to_string(),
                "you".to_string(),
                "!".to_string()
            ]
        );
        let ops = diff_words("Hi, you!", "Hi. you?");
        assert_eq!(
            ops,
            vec![
                DiffOp::Equal("Hi".to_string()),
                DiffOp::Delete(",".to_string()),
                DiffOp::Insert(".".to_string()),
                DiffOp::Equal(" you".to_string()),
                DiffOp::Delete("!".to_string()),
                DiffOp::Insert("?".to_string()),
            ]
        );
    }

    #[test]
    fn multiline_text_keeps_newlines() {
        let old = "First line.\nSecond line here.";
        let new = "First line.\nSecond line there.";
        let ops = diff_words(old, new);
        assert_eq!(
            ops,
            vec![
                DiffOp::Equal("First line.\nSecond line ".to_string()),
                DiffOp::Delete("here".to_string()),
                DiffOp::Insert("there".to_string()),
                DiffOp::Equal(".".to_string()),
            ]
        );
        // The rendered ops joined back give the suggestion.
        let rebuilt: String = ops
            .iter()
            .map(|op| match op {
                DiffOp::Equal(s) | DiffOp::Insert(s) => s.as_str(),
                DiffOp::Delete(_) => "",
            })
            .collect();
        assert_eq!(rebuilt, new);
    }
}
