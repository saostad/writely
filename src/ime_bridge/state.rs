//! Pure state for the Linux input method bridge protocol (v1).
//!
//! [`BridgeState`] mirrors the D-Bus contract in `docs/linux-ime-bridge.md`
//! without any D-Bus types in it, so it can be unit-tested directly. The
//! D-Bus glue lives in `service.rs`.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// Current protocol version, returned by `Hello`.
const PROTOCOL_VERSION: u32 = 1;

/// One bridge context, keyed by (D-Bus unique sender name, context string).
/// Two bridges may reuse the same context string without colliding.
type ContextKey = (String, String);

/// A validated `Clicked` action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickAction {
    Check,
    Apply,
    Dismiss,
}

impl ClickAction {
    fn parse(action: &str) -> Option<Self> {
        match action {
            "check" => Some(Self::Check),
            "apply" => Some(Self::Apply),
            "dismiss" => Some(Self::Dismiss),
            _ => None,
        }
    }
}

/// The focused field's text, for round 2 wiring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusedText {
    /// Latest surrounding text.
    pub text: String,
    /// Caret offset in chars (= cursor).
    pub caret: usize,
    /// Ordered, non-empty selection; `None` when `anchor == cursor`.
    pub selection: Option<Range<usize>>,
}

struct ContextData {
    program: String,
    password: bool,
    text: Option<String>,
    cursor: u32,
    anchor: u32,
}

/// All input-method bridge contexts. See `docs/linux-ime-bridge.md`.
pub struct BridgeState {
    contexts: HashMap<ContextKey, ContextData>,
    focused: Option<ContextKey>,
    // Used from round 2 wiring.
    #[allow(dead_code)]
    next_replace_id: u32,
    /// Senders that already got their `Hello` log line.
    hello_logged: HashSet<String>,
}

impl BridgeState {
    pub fn new() -> Self {
        Self {
            contexts: HashMap::new(),
            focused: None,
            next_replace_id: 1,
            hello_logged: HashSet::new(),
        }
    }

    /// The `Hello` handshake. Returns the protocol version and, the first
    /// time per sender, the log line body for
    /// `Writely: connected to the {bridge} input method bridge {bridge_version}.`
    pub fn hello(
        &mut self,
        sender: &str,
        bridge: &str,
        bridge_version: &str,
    ) -> (u32, Option<String>) {
        let message = if self.hello_logged.insert(sender.to_string()) {
            Some(format!(
                "connected to the {bridge} input method bridge {bridge_version}."
            ))
        } else {
            None
        };
        (PROTOCOL_VERSION, message)
    }

    /// This context now has keyboard focus, replacing any previous focus
    /// from any bridge. Re-focusing a known context keeps its text.
    pub fn focus_in(&mut self, sender: &str, context: &str, program: &str, password: bool) {
        let key = (sender.to_string(), context.to_string());
        match self.contexts.get_mut(&key) {
            Some(data) => {
                data.program = program.to_string();
                data.password = password;
            }
            None => {
                self.contexts.insert(
                    key.clone(),
                    ContextData {
                        program: program.to_string(),
                        password,
                        text: None,
                        cursor: 0,
                        anchor: 0,
                    },
                );
            }
        }
        self.focused = Some(key);
    }

    /// Clears focus, but only if this (sender, context) is the focused one.
    pub fn focus_out(&mut self, sender: &str, context: &str) {
        let key = (sender.to_string(), context.to_string());
        if self.focused.as_ref() == Some(&key) {
            self.focused = None;
        }
    }

    /// Stores the text around the cursor. Offsets are chars, not bytes.
    pub fn surrounding_text(
        &mut self,
        sender: &str,
        context: &str,
        text: &str,
        cursor: u32,
        anchor: u32,
    ) -> Result<(), String> {
        let key = (sender.to_string(), context.to_string());
        let Some(data) = self.contexts.get_mut(&key) else {
            return Err(format!(
                "SurroundingText: unknown context \"{context}\"; call FocusIn first."
            ));
        };
        let len = text.chars().count();
        if cursor as usize > len || anchor as usize > len {
            return Err(format!(
                "SurroundingText: cursor {cursor} / anchor {anchor} is past the end of the {len}-character text."
            ));
        }
        data.text = Some(text.to_string());
        data.cursor = cursor;
        data.anchor = anchor;
        Ok(())
    }

    /// Validates a `Clicked` action. The service stores the result.
    pub fn clicked(
        &self,
        sender: &str,
        context: &str,
        action: &str,
    ) -> Result<ClickAction, String> {
        let key = (sender.to_string(), context.to_string());
        if !self.contexts.contains_key(&key) {
            return Err(format!(
                "Clicked: unknown context \"{context}\"; call FocusIn first."
            ));
        }
        ClickAction::parse(action).ok_or_else(|| {
            format!("Clicked: unknown action \"{action}\" (expected check, apply or dismiss).")
        })
    }

    /// Validates a `ReplaceResult`. The service stores the result.
    pub fn replace_result(
        &self,
        sender: &str,
        context: &str,
        _request: u32,
        _ok: bool,
        _error: &str,
    ) -> Result<(), String> {
        let key = (sender.to_string(), context.to_string());
        if !self.contexts.contains_key(&key) {
            return Err(format!(
                "ReplaceResult: unknown context \"{context}\"; call FocusIn first."
            ));
        }
        Ok(())
    }

    /// Drops one context. Unknown contexts are a no-op, because bridges
    /// may destroy contexts that never reached `FocusIn`.
    pub fn destroy(&mut self, sender: &str, context: &str) {
        let key = (sender.to_string(), context.to_string());
        self.contexts.remove(&key);
        if self.focused.as_ref() == Some(&key) {
            self.focused = None;
        }
    }

    /// Drops every context owned by a bridge that left the bus, and clears
    /// focus if it owned the focused one.
    pub fn sender_vanished(&mut self, sender: &str) {
        self.contexts.retain(|key, _| key.0 != sender);
        if self.focused.as_ref().is_some_and(|key| key.0 == sender) {
            self.focused = None;
        }
        self.hello_logged.remove(sender);
    }

    /// Next `Replace` request id: 1, 2, 3, …
    // Used from round 2 wiring.
    #[allow(dead_code)]
    pub fn next_request(&mut self) -> u32 {
        let request = self.next_replace_id;
        self.next_replace_id = self.next_replace_id.wrapping_add(1).max(1);
        request
    }

    /// The focused field's text, or `None` when nothing is focused, the
    /// focused field is a password, or no surrounding text arrived yet.
    pub fn focused_text(&self) -> Option<FocusedText> {
        let key = self.focused.as_ref()?;
        let data = self.contexts.get(key)?;
        if data.password {
            return None;
        }
        let text = data.text.clone()?;
        let (cursor, anchor) = (data.cursor as usize, data.anchor as usize);
        let selection = if cursor == anchor {
            None
        } else {
            Some(cursor.min(anchor)..cursor.max(anchor))
        };
        Some(FocusedText {
            text,
            caret: cursor,
            selection,
        })
    }

    /// The focused (sender, context) key, for change detection.
    pub fn focused_key(&self) -> Option<(&str, &str)> {
        self.focused
            .as_ref()
            .map(|key| (key.0.as_str(), key.1.as_str()))
    }
}

impl Default for BridgeState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = ":1.1";
    const B: &str = ":1.2";

    fn focused_with_text() -> BridgeState {
        let mut state = BridgeState::new();
        state.focus_in(A, "ctx", "gedit", false);
        state.surrounding_text(A, "ctx", "hello", 5, 5).unwrap();
        state
    }

    #[test]
    fn focus_switches_between_senders_with_same_context_string() {
        let mut state = BridgeState::new();
        state.focus_in(A, "ctx", "gedit", false);
        state.surrounding_text(A, "ctx", "aaa", 3, 3).unwrap();
        state.focus_in(B, "ctx", "firefox", false);
        state.surrounding_text(B, "ctx", "bbb", 1, 1).unwrap();
        assert_eq!(state.focused_key(), Some((B, "ctx")));
        assert_eq!(
            state.focused_text(),
            Some(FocusedText {
                text: "bbb".to_string(),
                caret: 1,
                selection: None,
            })
        );
        // The other sender's context is untouched.
        state.focus_in(A, "ctx", "gedit", false);
        assert_eq!(state.focused_text().unwrap().text, "aaa");
    }

    #[test]
    fn focus_out_of_non_focused_context_is_noop() {
        let mut state = focused_with_text();
        state.focus_out(B, "ctx");
        state.focus_out(A, "other");
        assert_eq!(state.focused_key(), Some((A, "ctx")));
        assert!(state.focused_text().is_some());
        state.focus_out(A, "ctx");
        assert_eq!(state.focused_key(), None);
        assert_eq!(state.focused_text(), None);
    }

    #[test]
    fn cursor_or_anchor_past_end_rejected_with_exact_message() {
        let mut state = focused_with_text();
        assert_eq!(
            state.surrounding_text(A, "ctx", "hello", 6, 0),
            Err(
                "SurroundingText: cursor 6 / anchor 0 is past the end of the 5-character text."
                    .to_string()
            )
        );
        assert_eq!(
            state.surrounding_text(A, "ctx", "hello", 0, 6),
            Err(
                "SurroundingText: cursor 0 / anchor 6 is past the end of the 5-character text."
                    .to_string()
            )
        );
        // At the end is fine.
        assert!(state.surrounding_text(A, "ctx", "hello", 5, 5).is_ok());
        // The rejected calls didn't clobber the stored text.
        assert_eq!(state.focused_text().unwrap().text, "hello");
    }

    #[test]
    fn reversed_anchor_gives_ordered_selection() {
        let mut state = BridgeState::new();
        state.focus_in(A, "ctx", "gedit", false);
        state.surrounding_text(A, "ctx", "hello", 1, 4).unwrap();
        assert_eq!(state.focused_text().unwrap().selection, Some(1..4));
        state.surrounding_text(A, "ctx", "hello", 4, 1).unwrap();
        let focused = state.focused_text().unwrap();
        assert_eq!(focused.caret, 4);
        assert_eq!(focused.selection, Some(1..4));
        state.surrounding_text(A, "ctx", "hello", 2, 2).unwrap();
        assert_eq!(state.focused_text().unwrap().selection, None);
    }

    #[test]
    fn offsets_are_chars_not_bytes() {
        // 13 chars, 18 bytes.
        let text = "héllo 👋 wörld";
        assert_eq!(text.chars().count(), 13);
        let mut state = BridgeState::new();
        state.focus_in(A, "ctx", "gedit", false);
        assert!(state.surrounding_text(A, "ctx", text, 13, 13).is_ok());
        assert_eq!(
            state.surrounding_text(A, "ctx", text, 14, 14),
            Err(
                "SurroundingText: cursor 14 / anchor 14 is past the end of the 13-character text."
                    .to_string()
            )
        );
        // The emoji is a single unit: caret 7 sits right after it.
        state.surrounding_text(A, "ctx", text, 7, 5).unwrap();
        let focused = state.focused_text().unwrap();
        assert_eq!(focused.caret, 7);
        assert_eq!(focused.selection, Some(5..7));
    }

    #[test]
    fn password_field_and_missing_text_give_none() {
        let mut state = BridgeState::new();
        assert_eq!(state.focused_text(), None);
        state.focus_in(A, "ctx", "firefox", true);
        // No surrounding text yet.
        assert_eq!(state.focused_text(), None);
        state.surrounding_text(A, "ctx", "secret", 6, 6).unwrap();
        assert_eq!(state.focused_text(), None);
        // Same context, not a password: the text shows up.
        state.focus_in(A, "ctx", "firefox", false);
        assert!(state.focused_text().is_some());
    }

    #[test]
    fn sender_vanished_drops_only_that_sender() {
        let mut state = BridgeState::new();
        state.focus_in(A, "ctx1", "gedit", false);
        state.focus_in(A, "ctx2", "gedit", false);
        state.focus_in(B, "ctx1", "firefox", false);
        state.sender_vanished(A);
        assert_eq!(
            state.surrounding_text(A, "ctx1", "x", 1, 1),
            Err("SurroundingText: unknown context \"ctx1\"; call FocusIn first.".to_string())
        );
        assert_eq!(
            state.surrounding_text(A, "ctx2", "x", 1, 1),
            Err("SurroundingText: unknown context \"ctx2\"; call FocusIn first.".to_string())
        );
        // The other sender is untouched, and still focused.
        assert!(state.surrounding_text(B, "ctx1", "x", 1, 1).is_ok());
        assert_eq!(state.focused_key(), Some((B, "ctx1")));
        // Vanishing the focused sender clears focus.
        state.sender_vanished(B);
        assert_eq!(state.focused_key(), None);
    }

    #[test]
    fn unknown_context_errors() {
        let state = BridgeState::new();
        assert_eq!(
            state.clicked(A, "ctx", "check"),
            Err("Clicked: unknown context \"ctx\"; call FocusIn first.".to_string())
        );
        assert_eq!(
            state.replace_result(A, "ctx", 1, true, ""),
            Err("ReplaceResult: unknown context \"ctx\"; call FocusIn first.".to_string())
        );
        let mut state = state;
        assert_eq!(
            state.surrounding_text(A, "ctx", "x", 1, 1),
            Err("SurroundingText: unknown context \"ctx\"; call FocusIn first.".to_string())
        );
        // A context known under another sender is still unknown here.
        state.focus_in(B, "ctx", "firefox", false);
        assert_eq!(
            state.clicked(A, "ctx", "check"),
            Err("Clicked: unknown context \"ctx\"; call FocusIn first.".to_string())
        );
    }

    #[test]
    fn unknown_action_rejected_with_exact_message() {
        let state = focused_with_text();
        assert_eq!(
            state.clicked(A, "ctx", "check").unwrap(),
            ClickAction::Check
        );
        assert_eq!(
            state.clicked(A, "ctx", "apply").unwrap(),
            ClickAction::Apply
        );
        assert_eq!(
            state.clicked(A, "ctx", "dismiss").unwrap(),
            ClickAction::Dismiss
        );
        assert_eq!(
            state.clicked(A, "ctx", "bogus"),
            Err(
                "Clicked: unknown action \"bogus\" (expected check, apply or dismiss).".to_string()
            )
        );
    }

    #[test]
    fn request_ids_start_at_one_and_increase() {
        let mut state = BridgeState::new();
        assert_eq!(state.next_request(), 1);
        assert_eq!(state.next_request(), 2);
        assert_eq!(state.next_request(), 3);
    }

    #[test]
    fn destroy_clears_focus_only_for_that_context() {
        let mut state = focused_with_text();
        // Unknown context is a no-op: focus and text survive.
        state.destroy(A, "nope");
        state.destroy(B, "ctx");
        assert_eq!(state.focused_key(), Some((A, "ctx")));
        assert!(state.focused_text().is_some());
        // Another sender's same-named context survives.
        state.focus_in(B, "ctx", "firefox", false);
        state.surrounding_text(B, "ctx", "bbb", 3, 3).unwrap();
        state.focus_in(A, "ctx", "gedit", false);
        state.destroy(B, "ctx");
        assert_eq!(state.focused_key(), Some((A, "ctx")));
        assert_eq!(state.focused_text().unwrap().text, "hello");
        assert_eq!(
            state.surrounding_text(B, "ctx", "x", 1, 1),
            Err("SurroundingText: unknown context \"ctx\"; call FocusIn first.".to_string())
        );
        // Destroying the focused context clears focus.
        state.destroy(A, "ctx");
        assert_eq!(state.focused_key(), None);
        assert_eq!(state.focused_text(), None);
    }

    #[test]
    fn hello_returns_one_and_logs_once_per_sender() {
        let mut state = BridgeState::new();
        let (version, message) = state.hello(A, "fcitx5", "5.1");
        assert_eq!(version, 1);
        assert_eq!(
            message,
            Some("connected to the fcitx5 input method bridge 5.1.".to_string())
        );
        let (version, message) = state.hello(A, "fcitx5", "5.1");
        assert_eq!((version, message), (1, None));
        let (_, message) = state.hello(B, "ibus", "1.5");
        assert!(message.is_some());
    }
}
