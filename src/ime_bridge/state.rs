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

/// What the focused field offers: full surrounding text, or the
/// selection-only fallback for apps that don't share their text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusedKind {
    Text(FocusedText),
    SelectOnly,
}

struct ContextData {
    program: String,
    password: bool,
    text: Option<String>,
    cursor: u32,
    anchor: u32,
    text_unavailable: bool,
    can_select: bool,
}

/// All input-method bridge contexts. See `docs/linux-ime-bridge.md`.
pub struct BridgeState {
    contexts: HashMap<ContextKey, ContextData>,
    focused: Option<ContextKey>,
    next_replace_id: u32,
    /// Senders that already got their `Hello` log line.
    hello_logged: HashSet<String>,
}

/// One in-flight bridge answer (`Replace`, `SelectRequest` or
/// `ReplaceSelection`) and its result. Results for any other
/// (sender, context, request) triple are ignored. The event loop is
/// single-threaded, so only one request is ever in flight.
pub struct ReplaceWait {
    pending: Option<(String, String, u32)>,
    result: Option<(bool, String, String)>,
}

impl ReplaceWait {
    pub fn new() -> Self {
        Self {
            pending: None,
            result: None,
        }
    }

    /// Marks a request as in flight, dropping any previous answer.
    pub fn begin(&mut self, sender: &str, context: &str, request: u32) {
        self.pending = Some((sender.to_string(), context.to_string(), request));
        self.result = None;
    }

    /// Stores the result when it answers the pending request.
    /// Returns true when stored; anything else is ignored.
    pub fn insert(
        &mut self,
        sender: &str,
        context: &str,
        request: u32,
        ok: bool,
        error: &str,
    ) -> bool {
        self.insert_full(sender, context, request, ok, error, "")
    }

    /// Stores a result with a text payload (`Selected`'s text).
    /// Returns true when stored; anything else is ignored.
    pub fn insert_full(
        &mut self,
        sender: &str,
        context: &str,
        request: u32,
        ok: bool,
        error: &str,
        text: &str,
    ) -> bool {
        let matches = self.pending.as_ref().is_some_and(
            |(pending_sender, pending_context, pending_request)| {
                pending_sender == sender
                    && pending_context == context
                    && *pending_request == request
            },
        );
        if matches {
            self.result = Some((ok, error.to_string(), text.to_string()));
        }
        matches
    }

    /// Takes the stored answer with its text payload, if any.
    pub fn take_full(&mut self) -> Option<(bool, String, String)> {
        self.result.take()
    }

    /// True when an answer is waiting to be taken.
    pub fn has_result(&self) -> bool {
        self.result.is_some()
    }

    /// Drops the pending request and any answer.
    pub fn clear(&mut self) {
        self.pending = None;
        self.result = None;
    }
}

impl Default for ReplaceWait {
    fn default() -> Self {
        Self::new()
    }
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
                        text_unavailable: false,
                        can_select: false,
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
        // A valid SurroundingText cancels an earlier TextUnavailable.
        data.text_unavailable = false;
        Ok(())
    }

    /// The focused context has no surrounding text. `can_select` says
    /// whether the bridge can select the line for the select fallback.
    pub fn text_unavailable(
        &mut self,
        sender: &str,
        context: &str,
        can_select: bool,
    ) -> Result<(), String> {
        let key = (sender.to_string(), context.to_string());
        let Some(data) = self.contexts.get_mut(&key) else {
            return Err(format!(
                "TextUnavailable: unknown context \"{context}\"; call FocusIn first."
            ));
        };
        data.text_unavailable = true;
        data.can_select = can_select;
        Ok(())
    }

    /// Validates a `Selected` answer. The service stores the result.
    pub fn selected(
        &self,
        sender: &str,
        context: &str,
        _request: u32,
        _ok: bool,
        _text: &str,
        _error: &str,
    ) -> Result<(), String> {
        let key = (sender.to_string(), context.to_string());
        if !self.contexts.contains_key(&key) {
            return Err(format!(
                "Selected: unknown context \"{context}\"; call FocusIn first."
            ));
        }
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
    pub fn next_request(&mut self) -> u32 {
        let request = self.next_replace_id;
        self.next_replace_id = self.next_replace_id.wrapping_add(1).max(1);
        request
    }

    /// What the focused field offers, or `None` when nothing is
    /// focused, the field is a password, or there is neither text nor a
    /// usable select fallback.
    pub fn focused_kind(&self) -> Option<FocusedKind> {
        let key = self.focused.as_ref()?;
        let data = self.contexts.get(key)?;
        if data.password {
            return None;
        }
        if data.text_unavailable {
            return if data.can_select {
                Some(FocusedKind::SelectOnly)
            } else {
                None
            };
        }
        self.focused_text().map(FocusedKind::Text)
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

    /// The latest surrounding text for one context, if it exists and text
    /// arrived. Used for stale-text detection and bridge-mode reads.
    pub fn current_text(&self, sender: &str, context: &str) -> Option<String> {
        let key = (sender.to_string(), context.to_string());
        self.contexts.get(&key).and_then(|data| data.text.clone())
    }

    /// True when the (sender, context) pair still exists.
    pub fn context_exists(&self, sender: &str, context: &str) -> bool {
        let key = (sender.to_string(), context.to_string());
        self.contexts.contains_key(&key)
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
        assert_eq!(
            state.selected(A, "ctx", 1, true, "x", ""),
            Err("Selected: unknown context \"ctx\"; call FocusIn first.".to_string())
        );
        let mut state = state;
        assert_eq!(
            state.surrounding_text(A, "ctx", "x", 1, 1),
            Err("SurroundingText: unknown context \"ctx\"; call FocusIn first.".to_string())
        );
        assert_eq!(
            state.text_unavailable(A, "ctx", true),
            Err("TextUnavailable: unknown context \"ctx\"; call FocusIn first.".to_string())
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
    fn current_text_tracks_latest_surrounding_text() {
        let mut state = BridgeState::new();
        assert_eq!(state.current_text(A, "ctx"), None);
        assert!(!state.context_exists(A, "ctx"));
        state.focus_in(A, "ctx", "gedit", false);
        // No surrounding text yet.
        assert_eq!(state.current_text(A, "ctx"), None);
        assert!(state.context_exists(A, "ctx"));
        state.surrounding_text(A, "ctx", "hello", 5, 5).unwrap();
        assert_eq!(state.current_text(A, "ctx"), Some("hello".to_string()));
        // Stale detection: a snapshot of "hello" no longer matches.
        state.surrounding_text(A, "ctx", "hello!", 6, 6).unwrap();
        assert_ne!(state.current_text(A, "ctx").as_deref(), Some("hello"));
        // Per-sender: another sender's same-named context has its own text.
        state.focus_in(B, "ctx", "firefox", false);
        assert_eq!(state.current_text(B, "ctx"), None);
        // Destroyed contexts have no text.
        state.destroy(A, "ctx");
        assert_eq!(state.current_text(A, "ctx"), None);
        assert!(!state.context_exists(A, "ctx"));
        assert!(state.context_exists(B, "ctx"));
    }

    #[test]
    fn replace_wait_ignores_other_requests_and_senders() {
        let mut wait = ReplaceWait::new();
        // Nothing pending: everything is ignored.
        assert!(!wait.insert(A, "ctx", 1, true, ""));
        assert_eq!(wait.take_full(), None);
        wait.begin(A, "ctx", 2);
        // Other request id, other sender, other context: ignored.
        assert!(!wait.insert(A, "ctx", 1, true, ""));
        assert!(!wait.insert(A, "ctx", 3, true, ""));
        assert!(!wait.insert(B, "ctx", 2, true, ""));
        assert!(!wait.insert(A, "other", 2, true, ""));
        assert!(!wait.has_result());
        assert_eq!(wait.take_full(), None);
        // The exact triple is stored.
        assert!(wait.insert(A, "ctx", 2, false, "boom"));
        assert!(wait.has_result());
        assert_eq!(
            wait.take_full(),
            Some((false, "boom".to_string(), String::new()))
        );
        assert_eq!(wait.take_full(), None);
        // begin() drops any previous answer.
        wait.begin(A, "ctx", 3);
        assert_eq!(wait.take_full(), None);
        wait.clear();
        assert!(!wait.insert(A, "ctx", 3, true, ""));
    }

    #[test]
    fn text_unavailable_then_surrounding_text_gives_text() {
        let mut state = BridgeState::new();
        state.focus_in(A, "ctx", "chromium", false);
        assert_eq!(state.focused_kind(), None);
        state.text_unavailable(A, "ctx", true).unwrap();
        assert_eq!(state.focused_kind(), Some(FocusedKind::SelectOnly));
        assert_eq!(state.focused_text(), None);
        state.surrounding_text(A, "ctx", "hello", 5, 5).unwrap();
        assert_eq!(
            state.focused_kind(),
            Some(FocusedKind::Text(FocusedText {
                text: "hello".to_string(),
                caret: 5,
                selection: None,
            }))
        );
        // TextUnavailable again shadows the stale text until fresh text arrives.
        state.text_unavailable(A, "ctx", true).unwrap();
        assert_eq!(state.focused_kind(), Some(FocusedKind::SelectOnly));
    }

    #[test]
    fn text_unavailable_without_select_fallback_gives_none() {
        let mut state = BridgeState::new();
        state.focus_in(A, "ctx", "chromium", true);
        state.text_unavailable(A, "ctx", true).unwrap();
        // Password fields never offer the select fallback.
        assert_eq!(state.focused_kind(), None);
        state.focus_in(A, "ctx", "chromium", false);
        state.text_unavailable(A, "ctx", false).unwrap();
        // No select capability: nothing to show.
        assert_eq!(state.focused_kind(), None);
        assert_eq!(state.focused_text(), None);
    }

    #[test]
    fn selected_answers_match_by_request_and_sender() {
        let mut wait = ReplaceWait::new();
        wait.begin(A, "ctx", 2);
        // Other request id, other sender, other context: ignored.
        assert!(!wait.insert_full(A, "ctx", 1, true, "", "stale"));
        assert!(!wait.insert_full(B, "ctx", 2, true, "", "stale"));
        assert!(!wait.insert_full(A, "other", 2, true, "", "stale"));
        assert!(!wait.has_result());
        assert_eq!(wait.take_full(), None);
        // The exact triple stores the text payload.
        assert!(wait.insert_full(A, "ctx", 2, true, "", "line text"));
        assert!(wait.has_result());
        assert_eq!(
            wait.take_full(),
            Some((true, String::new(), "line text".to_string()))
        );
        assert_eq!(wait.take_full(), None);
        // Failures carry the error instead.
        wait.begin(A, "ctx", 3);
        assert!(wait.insert_full(A, "ctx", 3, false, "boom", ""));
        assert_eq!(
            wait.take_full(),
            Some((false, "boom".to_string(), String::new()))
        );
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
