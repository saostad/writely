use std::io::BufRead;

use crate::providers::{strip_think_block, UNCLEAR_MARKER};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StreamFormat {
    Sse,
    Ndjson,
}

/// One provider stream event, decoded by each provider's event parser.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct StreamDelta {
    pub(crate) text: String,
    pub(crate) finish_reason: Option<String>,
    pub(crate) done: bool,
}

/// Reads a streamed reply event by event. SSE events are the joined `data:`
/// lines up to a blank line; NDJSON events are single non-blank lines.
/// `on_event` returns `Ok(false)` to stop reading early.
pub(crate) fn read_events(
    reader: &mut dyn BufRead,
    format: StreamFormat,
    on_event: &mut dyn FnMut(&str) -> Result<bool, String>,
) -> Result<(), String> {
    let mut data_lines: Vec<String> = Vec::new();
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|e| format!("The reply stopped part-way: {e}"))?;
        if read == 0 {
            break;
        }
        if line.ends_with('\n') {
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
        } else if line.ends_with('\r') {
            line.pop();
        }
        match format {
            StreamFormat::Ndjson => {
                if line.trim().is_empty() {
                    continue;
                }
                if !on_event(&line)? {
                    return Ok(());
                }
            }
            StreamFormat::Sse => {
                if line.is_empty() {
                    if !data_lines.is_empty() {
                        let event = data_lines.join("\n");
                        data_lines.clear();
                        if !on_event(&event)? {
                            return Ok(());
                        }
                    }
                } else if let Some(data) = line.strip_prefix("data:") {
                    let data = data.strip_prefix(' ').unwrap_or(data);
                    data_lines.push(data.to_string());
                }
                // `:` comment lines and other fields (event:, id:, …) are ignored.
            }
        }
    }
    if format == StreamFormat::Sse && !data_lines.is_empty() {
        let event = data_lines.join("\n");
        on_event(&event)?;
    }
    Ok(())
}

/// The text the popup may show for the reply received so far, or `None` when
/// nothing safe can be shown yet: an unclosed think block, an empty reply, or
/// a prefix of `UNCLEAR` (which would flash up before becoming the marker).
pub(crate) fn partial_display(raw: &str) -> Option<String> {
    let trimmed_start = raw.trim_start();
    if trimmed_start.starts_with("<think") && !trimmed_start.contains("</think>") {
        return None;
    }
    let text = strip_think_block(raw).trim_start();
    let text = text.strip_prefix('"').unwrap_or(text);
    let text = text.trim_end();
    if text.is_empty() || UNCLEAR_MARKER.starts_with(text) {
        return None;
    }
    Some(text.to_string())
}

/// Accumulates a streamed reply and reports each new displayable partial.
pub(crate) struct Partial<'a> {
    raw: String,
    last_shown: Option<String>,
    on_partial: &'a mut dyn FnMut(&str),
}

impl<'a> Partial<'a> {
    pub(crate) fn new(on_partial: &'a mut dyn FnMut(&str)) -> Self {
        Partial {
            raw: String::new(),
            last_shown: None,
            on_partial,
        }
    }

    pub(crate) fn push(&mut self, delta: &str) {
        self.raw.push_str(delta);
        if let Some(shown) = partial_display(&self.raw) {
            if self.last_shown.as_deref() != Some(shown.as_str()) {
                (self.on_partial)(&shown);
                self.last_shown = Some(shown);
            }
        }
    }

    pub(crate) fn into_raw(self) -> String {
        self.raw
    }
}

#[cfg(test)]
mod tests {
    use super::{partial_display, read_events, Partial, StreamFormat};
    use std::io::{BufReader, Cursor};

    fn collect(bytes: &[u8], format: StreamFormat) -> Vec<String> {
        // A tiny capacity forces reads to split mid-line and mid-event.
        let mut reader = BufReader::with_capacity(2, Cursor::new(bytes.to_vec()));
        let mut events = Vec::new();
        read_events(&mut reader, format, &mut |event| {
            events.push(event.to_string());
            Ok(true)
        })
        .expect("read should succeed");
        events
    }

    #[test]
    fn sse_events_split_across_chunks() {
        let bytes = b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\n";
        assert_eq!(
            collect(bytes, StreamFormat::Sse),
            ["{\"a\":1}", "{\"b\":2}"]
        );
    }

    #[test]
    fn sse_crlf_comments_and_other_fields() {
        let bytes = b": keep-alive\r\nevent: message\r\ndata: hello\r\n\r\n: done\r\n";
        assert_eq!(collect(bytes, StreamFormat::Sse), ["hello"]);
    }

    #[test]
    fn sse_multi_line_data_joined_with_newline() {
        let bytes = b"data: first\ndata:second\ndata:  third\n\n";
        // Only one optional space after the colon is stripped.
        assert_eq!(collect(bytes, StreamFormat::Sse), ["first\nsecond\n third"]);
    }

    #[test]
    fn sse_trailing_event_without_blank_line() {
        assert_eq!(collect(b"data: last", StreamFormat::Sse), ["last"]);
        assert_eq!(collect(b"data: last\n", StreamFormat::Sse), ["last"]);
    }

    #[test]
    fn ndjson_skips_blank_lines() {
        let bytes = b"{\"a\":1}\n\n  \n{\"b\":2}\r\n";
        assert_eq!(
            collect(bytes, StreamFormat::Ndjson),
            ["{\"a\":1}", "{\"b\":2}"]
        );
    }

    #[test]
    fn early_stop_ends_reading() {
        let mut reader =
            BufReader::with_capacity(2, Cursor::new(b"data: one\n\ndata: two\n\n".to_vec()));
        let mut events = Vec::new();
        read_events(&mut reader, StreamFormat::Sse, &mut |event| {
            events.push(event.to_string());
            Ok(false)
        })
        .expect("read should succeed");
        assert_eq!(events, ["one"]);
    }

    #[test]
    fn read_error_is_reported() {
        struct Failing;
        impl std::io::Read for Failing {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gone"))
            }
        }
        impl std::io::BufRead for Failing {
            fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
                Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gone"))
            }
            fn consume(&mut self, _amt: usize) {}
        }
        let mut reader = Failing;
        let err = read_events(&mut reader, StreamFormat::Ndjson, &mut |_| Ok(true))
            .expect_err("should fail");
        assert!(err.starts_with("The reply stopped part-way: "), "{err}");
        assert!(err.contains("gone"), "{err}");
    }

    #[test]
    fn partial_display_rules() {
        assert_eq!(partial_display(""), None);
        assert_eq!(partial_display("   "), None);
        assert_eq!(partial_display("She go").as_deref(), Some("She go"));
        // Leading whitespace and trailing whitespace are trimmed.
        assert_eq!(partial_display("  She go  ").as_deref(), Some("She go"));
        // One leading quote is dropped; a lone quote shows nothing.
        assert_eq!(partial_display("\"She go").as_deref(), Some("She go"));
        assert_eq!(partial_display("\""), None);
        // Unclosed think block: nothing; closed: the rest.
        assert_eq!(partial_display("<think>rea"), None);
        assert_eq!(partial_display("  <think>rea"), None);
        assert_eq!(partial_display("<think>r</think>"), None);
        assert_eq!(
            partial_display("<think>r</think> She go").as_deref(),
            Some("She go")
        );
        // UNCLEAR prefixes and the marker itself are never shown.
        for prefix in ["U", "UN", "UNC", "UNCLE", "UNCLEAR", " UNCLEAR "] {
            assert_eq!(partial_display(prefix), None, "{prefix:?}");
        }
        assert_eq!(partial_display("\"UNC"), None);
        // Text that merely starts with the marker letters is shown.
        assert_eq!(
            partial_display("UNCLEARLY so").as_deref(),
            Some("UNCLEARLY so")
        );
    }

    #[test]
    fn partial_calls_back_only_on_change() {
        let mut shown: Vec<String> = Vec::new();
        let raw = {
            let mut on_partial = |text: &str| shown.push(text.to_string());
            let mut partial = Partial::new(&mut on_partial);
            partial.push("<think>think");
            partial.push("ing</think>She ");
            partial.push(" "); // only trailing whitespace changes: no callback
            partial.push("goes.");
            partial.into_raw()
        };
        assert_eq!(shown, ["She", "She  goes."]);
        assert_eq!(raw, "<think>thinking</think>She  goes.");
    }

    #[test]
    fn partial_never_shows_unclear() {
        let mut shown: Vec<String> = Vec::new();
        let raw = {
            let mut on_partial = |text: &str| shown.push(text.to_string());
            let mut partial = Partial::new(&mut on_partial);
            partial.push("UNC");
            partial.push("LEAR");
            partial.into_raw()
        };
        assert!(shown.is_empty(), "{shown:?}");
        assert_eq!(raw, "UNCLEAR");
    }
}
