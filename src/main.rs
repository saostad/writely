#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod check_text;
mod check_window;
#[cfg(target_os = "macos")]
mod clipboard;
mod config;
mod diff;
mod overlay;
mod popup;
mod providers;
mod settings;
mod text_access;
mod watcher;

use std::ops::Range;

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use text_access::{CaretRect, FieldSnapshot};

#[derive(Debug)]
pub enum UserEvent {
    MenuEvent(MenuEvent),
    CaretChanged,
    IconClicked,
    HotkeyPressed,
    ReplaceClicked,
    // Fired only by the macOS tab row; the handler lands with the rewrite
    // wiring, so off macOS (no tabs) this is never constructed.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    TabClicked(popup::PopupTab),
    PermissionGranted,
    #[cfg(target_os = "macos")]
    CopiedText {
        generation: u64,
        result: Result<clipboard::CopyResult, String>,
        pid: u32,
        from_hotkey: bool,
    },
    Suggestion {
        generation: u64,
        result: Result<String, String>,
    },
    Rewrite {
        generation: u64,
        style: providers::RewriteStyle,
        result: Result<String, String>,
    },
    TextCheckResult {
        generation: u64,
        result: Result<String, String>,
    },
}

/// The in-flight click: snapshot + sentence range + sentence text, waiting
/// for the Gemini reply. The snapshot is `None` on the copy-and-paste path
/// (there is no AX snapshot; Replace pastes instead).
struct Checking {
    snap: Option<FieldSnapshot>,
    range: Range<usize>,
    sentence: String,
    pid: u32,
    via_clipboard: bool,
}

/// A shown suggestion waiting for Replace: the grammar correction plus the
/// per-tab rewrite cache for this popup.
struct Pending {
    snap: Option<FieldSnapshot>,
    range: Range<usize>,
    /// The checked text the tabs rewrite.
    original: String,
    /// The grammar result; `None` when the text was already correct.
    correction: Option<String>,
    /// Cached rewrites, indexed by position in `RewriteStyle::ALL`.
    rewrites: [Option<String>; 5],
    selected: popup::PopupTab,
    /// Per-style in-flight flags, so a style is never requested twice.
    rewrite_in_flight: [bool; 5],
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pid: u32,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    via_clipboard: bool,
}

/// Index into `Pending::rewrites` for a style (its position in
/// `RewriteStyle::ALL`).
fn style_index(style: providers::RewriteStyle) -> usize {
    // Every style is in ALL, so this always finds a position.
    providers::RewriteStyle::ALL
        .iter()
        .position(|candidate| *candidate == style)
        .unwrap_or_default()
}

/// The text the selected tab would insert, or `None` when there is nothing to
/// insert (a still-loading or failed rewrite, a rewrite that leaves the text
/// unchanged, or `Correct` with no correction).
fn selected_text(pending: &Pending) -> Option<&str> {
    match pending.selected {
        popup::PopupTab::Correct => pending.correction.as_deref(),
        popup::PopupTab::Rewrite(style) => {
            let cached = pending.rewrites[style_index(style)].as_deref()?;
            match providers::classify_suggestion(cached, &pending.original) {
                providers::SuggestionOutcome::Suggestion => Some(cached),
                providers::SuggestionOutcome::Unclear | providers::SuggestionOutcome::Unchanged => {
                    None
                }
            }
        }
    }
}

/// The "already correct" body for the Correct tab: on macOS it points at the
/// style menu in the footer; elsewhere it is today's plain message (no menu
/// there).
fn looks_good_message() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Looks good — no changes needed. Pick a style below to rewrite it."
    }
    #[cfg(not(target_os = "macos"))]
    {
        "Looks good — no changes needed."
    }
}

/// Mouse-pointer anchor for the hotkey popup when no caret or container is
/// known: a zero-size rect at the pointer, converted from AppKit
/// bottom-left coords to the top-left coords the popup uses.
#[cfg(target_os = "macos")]
fn mouse_anchor() -> Option<CaretRect> {
    use objc2_app_kit::NSScreen;
    use objc2_foundation::MainThreadMarker;
    let mtm = MainThreadMarker::new()?;
    let screens = NSScreen::screens(mtm);
    if screens.count() == 0 {
        return None;
    }
    let primary_height = screens.objectAtIndex(0).frame().size.height;
    let point = objc2_app_kit::NSEvent::mouseLocation();
    Some(CaretRect {
        x: point.x,
        y: primary_height - point.y,
        width: 0.0,
        height: 0.0,
        physical: false,
    })
}

/// Registers the global hotkey (⌃⌥G) on macOS. Returns the manager (kept
/// alive by the caller) or `None` when registration failed; failures are
/// logged and shown in the Settings status line with the real error.
#[cfg(target_os = "macos")]
fn register_hotkey(
    proxy: &EventLoopProxy<UserEvent>,
) -> Option<global_hotkey::GlobalHotKeyManager> {
    use global_hotkey::hotkey::{Code, HotKey, Modifiers};
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(error) => {
            let message = format!("Could not register the ⌃⌥G hotkey: {error}");
            eprintln!("Writely: {message}");
            settings::set_hotkey_message(Some(message));
            return None;
        }
    };
    let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyG);
    if let Err(error) = manager.register(hotkey) {
        let message = format!("Could not register the ⌃⌥G hotkey: {error}");
        eprintln!("Writely: {message}");
        settings::set_hotkey_message(Some(message));
        return None;
    }
    let proxy = proxy.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state() == HotKeyState::Pressed {
            if let Err(error) = proxy.send_event(UserEvent::HotkeyPressed) {
                eprintln!("Writely: could not deliver the hotkey event: {error}");
            }
        }
    }));
    Some(manager)
}

/// Shows the popup at `anchor` (falling back to `fallback`), remembers the
/// position for later updates, and reports whether it was shown.
fn show_popup(
    text: &str,
    is_error: bool,
    show_button: bool,
    anchor: Option<CaretRect>,
    fallback: Option<CaretRect>,
    last_anchor: &mut Option<CaretRect>,
) {
    if let Some(rect) = anchor.or(fallback) {
        *last_anchor = Some(rect);
        popup::show_message(text, is_error, show_button, &rect);
    } else {
        eprintln!("Writely: could not show the suggestion: the caret position is unknown.");
    }
}

/// Why settings/key loading failed: the popup message and whether it renders
/// as an error (red). Load failures are errors; "set up a model/key first"
/// prompts are plain guidance.
struct SettingsFailure {
    message: String,
    is_error: bool,
}

impl SettingsFailure {
    fn error(message: String) -> Self {
        Self {
            message,
            is_error: true,
        }
    }

    fn guidance(message: String) -> Self {
        Self {
            message,
            is_error: false,
        }
    }
}

/// Loads the app settings and the selected provider's API key, exactly as the
/// grammar check does. `Ok` carries `(settings, key)` with `key == None` only
/// for Ollama; `Err` is the user-facing popup message with its severity.
fn resolve_settings() -> Result<(config::AppSettings, Option<String>), SettingsFailure> {
    let app_settings = config::load_settings().map_err(SettingsFailure::error)?;
    if app_settings.current_model().is_empty() {
        return Err(SettingsFailure::guidance(
            "Choose a model first: click the Writely menu-bar icon › Settings…".to_string(),
        ));
    }
    // Ollama runs locally and needs no key; the other providers read the
    // selected provider's key from the keychain (never shown or logged).
    let key: Option<String> = match app_settings.provider {
        config::Provider::Ollama => None,
        provider => settings::load_api_key_for(provider).map_err(SettingsFailure::error)?,
    };
    if key.is_none() && app_settings.provider != config::Provider::Ollama {
        return Err(SettingsFailure::guidance(format!(
            "Add your {} API key first: click the Writely menu-bar icon › Settings…",
            app_settings.provider.display_name()
        )));
    }
    Ok((app_settings, key))
}

#[allow(clippy::too_many_arguments)]
fn spawn_check_thread(
    proxy: &EventLoopProxy<UserEvent>,
    app_settings: config::AppSettings,
    key: Option<String>,
    sentence: String,
    my_generation: u64,
    checking: &mut Option<Checking>,
    anchor: Option<CaretRect>,
    last_anchor: &mut Option<CaretRect>,
) {
    let worker_proxy = proxy.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("writely-suggest".to_string())
        .spawn(move || {
            // The key lives only in this thread's request; it is never
            // logged or put in an error.
            let result = providers::suggest(&app_settings, key.as_deref(), &sentence);
            if let Err(error) = worker_proxy.send_event(UserEvent::Suggestion {
                generation: my_generation,
                result,
            }) {
                eprintln!("Writely: could not deliver the suggestion: {error}");
            }
        })
    {
        *checking = None;
        show_popup(
            &format!("Could not check the sentence: {error}"),
            true,
            false,
            anchor,
            None,
            last_anchor,
        );
    }
}

/// One rewrite worker (same pattern as the grammar check): calls
/// `providers::rewrite` off the main thread and sends `UserEvent::Rewrite`.
/// Returns the spawn error for the caller to show; the popup already says
/// the style's progress text, so the caller replaces it with the failure.
fn spawn_rewrite_thread(
    proxy: &EventLoopProxy<UserEvent>,
    app_settings: config::AppSettings,
    key: Option<String>,
    text: String,
    style: providers::RewriteStyle,
    my_generation: u64,
) -> Result<(), String> {
    let worker_proxy = proxy.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("writely-rewrite".to_string())
        .spawn(move || {
            // The key lives only in this thread's request; it is never
            // logged or put in an error.
            let result = providers::rewrite(&app_settings, key.as_deref(), &text, style);
            if let Err(error) = worker_proxy.send_event(UserEvent::Rewrite {
                generation: my_generation,
                style,
                result,
            }) {
                eprintln!("Writely: could not deliver the rewrite: {error}");
            }
        })
    {
        return Err(format!("Could not rewrite the text: {error}"));
    }
    Ok(())
}

/// Copy-and-paste flow (macOS): the layered chain (AX selection, menu-bar
/// Copy, simulated ⌘C) runs in a `writely-copy` worker thread — it can take
/// up to 2 s — which sends `UserEvent::CopiedText`. Never runs for a secure
/// field (the caller checks that first).
#[cfg(target_os = "macos")]
fn handle_icon_clipboard(
    proxy: &EventLoopProxy<UserEvent>,
    pid: u32,
    anchor: Option<CaretRect>,
    generation: &mut u64,
    last_anchor: &mut Option<CaretRect>,
    from_hotkey: bool,
) {
    show_popup("Checking…", false, false, anchor, None, last_anchor);
    let my_generation = *generation;
    let worker_proxy = proxy.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("writely-copy".to_string())
        .spawn(move || {
            let result = clipboard::layered_copy_text(pid);
            if let Err(send_error) = worker_proxy.send_event(UserEvent::CopiedText {
                generation: my_generation,
                result,
                pid,
                from_hotkey,
            }) {
                eprintln!("Writely: could not deliver the copied text: {send_error}");
            }
        })
    {
        show_popup(
            &format!("Could not copy the selected text: {error}"),
            true,
            false,
            anchor,
            None,
            last_anchor,
        );
    }
}

/// Handles the worker's layered-copy result: starts a check on the copied
/// text, or tells the user to select text first.
#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn handle_copied_text(
    proxy: &EventLoopProxy<UserEvent>,
    current_generation: u64,
    event_generation: u64,
    result: Result<clipboard::CopyResult, String>,
    pid: u32,
    from_hotkey: bool,
    checking: &mut Option<Checking>,
    pending: &mut Option<Pending>,
    last_anchor: &mut Option<CaretRect>,
) {
    // Drop late results: a newer click, or typing that hid the popup, wins.
    if event_generation != current_generation || !popup::is_visible() {
        return;
    }
    let empty_message = if from_hotkey {
        "Select the text you want to check, then press ⌃⌥G."
    } else {
        "Select the text you want to check, then click W."
    };
    let anchor = *last_anchor;
    let text = match result {
        Err(error) => {
            show_popup(&error, true, false, anchor, None, last_anchor);
            return;
        }
        Ok(clipboard::CopyResult::NothingNew) => {
            show_popup(empty_message, false, false, anchor, None, last_anchor);
            return;
        }
        Ok(clipboard::CopyResult::Text(text)) => text,
    };
    if text.trim().is_empty() {
        show_popup(empty_message, false, false, anchor, None, last_anchor);
        return;
    }
    if text.chars().count() > text_access::MAX_SELECTION_CHARS {
        show_popup(
            text_access::SELECTION_TOO_LONG_MESSAGE,
            false,
            false,
            anchor,
            None,
            last_anchor,
        );
        return;
    }
    let (app_settings, key) = match resolve_settings() {
        Ok(loaded) => loaded,
        Err(failure) => {
            show_popup(
                &failure.message,
                failure.is_error,
                false,
                anchor,
                None,
                last_anchor,
            );
            return;
        }
    };
    let end = text.chars().count();
    show_popup("Checking…", false, false, anchor, None, last_anchor);
    *checking = Some(Checking {
        snap: None,
        range: 0..end,
        sentence: text.clone(),
        pid,
        via_clipboard: true,
    });
    *pending = None;
    spawn_check_thread(
        proxy,
        app_settings,
        key,
        text,
        event_generation,
        checking,
        anchor,
        last_anchor,
    );
}

fn handle_icon_clicked(
    proxy: &EventLoopProxy<UserEvent>,
    generation: &mut u64,
    checking: &mut Option<Checking>,
    pending: &mut Option<Pending>,
    last_anchor: &mut Option<CaretRect>,
    from_hotkey: bool,
) {
    // A new click invalidates any in-flight request; stale replies are dropped.
    *generation = generation.wrapping_add(1);
    let my_generation = *generation;
    *pending = None;
    *checking = None;
    // `live_pid` is only consumed by macOS-only paths below; keep the
    // binding on all targets and allow it to be unused elsewhere.
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    let (live_anchor, live_pid) = match text_access::caret_info() {
        Ok(Some(info)) => (Some(info.caret_rect), Some(info.pid)),
        Ok(None) => (None, None),
        Err(error) => {
            eprintln!("Writely: {error}");
            (None, None)
        }
    };
    // The hotkey popup anchors at the caret if one is known, else at the
    // input-area container (already in `live_anchor`), else at the mouse.
    #[cfg(target_os = "macos")]
    let (live_anchor, live_pid) = if from_hotkey && live_anchor.is_none() {
        (mouse_anchor().or(live_anchor), live_pid)
    } else {
        (live_anchor, live_pid)
    };
    #[cfg(not(target_os = "macos"))]
    let _ = from_hotkey;
    let (app_settings, key) = match resolve_settings() {
        Ok(loaded) => loaded,
        Err(failure) => {
            show_popup(
                &failure.message,
                failure.is_error,
                false,
                live_anchor,
                None,
                last_anchor,
            );
            return;
        }
    };
    let Some(snap) = (match text_access::focused_field() {
        Ok(snap) => snap,
        Err(error) => {
            show_popup(&error, true, false, live_anchor, None, last_anchor);
            return;
        }
    }) else {
        // No AX text for the focused element (e.g. Warp): on macOS fall back
        // to copy-and-paste, but never for a secure field.
        #[cfg(target_os = "macos")]
        if !text_access::macos_helpers::focused_is_secure() {
            if let Some(pid) = live_pid {
                handle_icon_clipboard(
                    proxy,
                    pid,
                    live_anchor,
                    generation,
                    last_anchor,
                    from_hotkey,
                );
                return;
            }
        }
        show_popup(
            "Writely can't read this text field.",
            false,
            false,
            live_anchor,
            None,
            last_anchor,
        );
        return;
    };
    let anchor = live_anchor.or(Some(snap.caret_rect));
    // A non-empty selection is checked (and replaced) as a whole; otherwise
    // the sentence at the caret is used.
    let mut selection_use: Option<(Range<usize>, String)> = None;
    if let Some(sel) = snap.selection.clone() {
        match text_access::check_selection(&snap.text, sel) {
            text_access::CheckedSelection::TooLong => {
                show_popup(
                    text_access::SELECTION_TOO_LONG_MESSAGE,
                    false,
                    false,
                    anchor,
                    None,
                    last_anchor,
                );
                return;
            }
            text_access::CheckedSelection::Use(range) => {
                let sentence: String = snap
                    .text
                    .chars()
                    .skip(range.start)
                    .take(range.end - range.start)
                    .collect();
                selection_use = Some((range, sentence));
            }
            text_access::CheckedSelection::Fallback => {}
        }
    }
    let (range, sentence) = if let Some(use_range) = selection_use {
        use_range
    } else if !snap.caret_known {
        // No caret offset (e.g. Firefox): check the whole field when it fits,
        // otherwise the clipboard fallback (or the too-long message where
        // that isn't available).
        match text_access::check_without_caret(&snap.text) {
            text_access::NoCaretDecision::CheckWholeField(range) => {
                let sentence: String = snap
                    .text
                    .chars()
                    .skip(range.start)
                    .take(range.end - range.start)
                    .collect();
                (range, sentence)
            }
            text_access::NoCaretDecision::Empty => {
                show_popup(
                    "Type a sentence first.",
                    false,
                    false,
                    anchor,
                    None,
                    last_anchor,
                );
                return;
            }
            text_access::NoCaretDecision::TooLong => {
                // The whole field doesn't fit: on macOS try the clipboard
                // (which re-applies the limit to the copied text).
                #[cfg(target_os = "macos")]
                if let Some(pid) = live_pid.or(Some(snap.pid)) {
                    handle_icon_clipboard(proxy, pid, anchor, generation, last_anchor, from_hotkey);
                    return;
                }
                show_popup(
                    text_access::SELECTION_TOO_LONG_MESSAGE,
                    false,
                    false,
                    anchor,
                    None,
                    last_anchor,
                );
                return;
            }
        }
    } else {
        match text_access::sentence::sentence_range(&snap.text, snap.caret) {
            Some(range) => {
                let sentence: String = snap
                    .text
                    .chars()
                    .skip(range.start)
                    .take(range.end - range.start)
                    .collect();
                (range, sentence)
            }
            None => {
                show_popup(
                    "Type a sentence first.",
                    false,
                    false,
                    anchor,
                    None,
                    last_anchor,
                );
                return;
            }
        }
    };
    if sentence.trim().is_empty() {
        show_popup(
            "Type a sentence first.",
            false,
            false,
            anchor,
            None,
            last_anchor,
        );
        return;
    }
    show_popup("Checking…", false, false, anchor, None, last_anchor);
    let pid = snap.pid;
    *checking = Some(Checking {
        snap: Some(snap),
        range,
        sentence: sentence.clone(),
        pid,
        via_clipboard: false,
    });
    spawn_check_thread(
        proxy,
        app_settings,
        key,
        sentence,
        my_generation,
        checking,
        anchor,
        last_anchor,
    );
}

fn handle_suggestion(
    current_generation: u64,
    event_generation: u64,
    result: Result<String, String>,
    checking: &mut Option<Checking>,
    pending: &mut Option<Pending>,
    last_anchor: &mut Option<CaretRect>,
) {
    // Drop late replies: a newer click, or typing that hid the popup, wins.
    if event_generation != current_generation || !popup::is_visible() {
        return;
    }
    match result {
        Err(error) => {
            *checking = None;
            *pending = None;
            show_popup(&error, true, false, *last_anchor, None, last_anchor);
        }
        Ok(suggestion) => {
            let Some(active) = checking.take() else {
                return;
            };
            match providers::classify_suggestion(&suggestion, &active.sentence) {
                providers::SuggestionOutcome::Unclear => {
                    show_popup(
                        providers::UNCLEAR_MESSAGE,
                        false,
                        false,
                        *last_anchor,
                        active.snap.as_ref().map(|snap| snap.caret_rect),
                        last_anchor,
                    );
                }
                providers::SuggestionOutcome::Unchanged => {
                    if let Some(rect) =
                        (*last_anchor).or(active.snap.as_ref().map(|snap| snap.caret_rect))
                    {
                        *last_anchor = Some(rect);
                        popup::show_tabbed(
                            popup::PopupTab::Correct,
                            popup::TabContent::Message {
                                text: looks_good_message(),
                                is_error: false,
                                loading: false,
                            },
                            &rect,
                        );
                    } else {
                        eprintln!(
                            "Writely: could not show the suggestion: the caret position is unknown."
                        );
                    }
                    *pending = Some(Pending {
                        snap: active.snap,
                        range: active.range,
                        original: active.sentence,
                        correction: None,
                        rewrites: [None, None, None, None, None],
                        selected: popup::PopupTab::Correct,
                        rewrite_in_flight: [false; 5],
                        pid: active.pid,
                        via_clipboard: active.via_clipboard,
                    });
                }
                providers::SuggestionOutcome::Suggestion => {
                    if let Some(rect) =
                        (*last_anchor).or(active.snap.as_ref().map(|snap| snap.caret_rect))
                    {
                        *last_anchor = Some(rect);
                        popup::show_tabbed(
                            popup::PopupTab::Correct,
                            popup::TabContent::Diff {
                                original: &active.sentence,
                                suggestion: &suggestion,
                            },
                            &rect,
                        );
                    } else {
                        eprintln!(
                            "Writely: could not show the suggestion: the caret position is unknown."
                        );
                    }
                    *pending = Some(Pending {
                        snap: active.snap,
                        range: active.range,
                        original: active.sentence,
                        correction: Some(suggestion),
                        rewrites: [None, None, None, None, None],
                        selected: popup::PopupTab::Correct,
                        rewrite_in_flight: [false; 5],
                        pid: active.pid,
                        via_clipboard: active.via_clipboard,
                    });
                }
            }
        }
    }
}

/// Shows a finished rewrite on its tab: the word diff, or the note that this
/// style leaves the text unchanged (or that the text was unclear).
fn show_rewrite_result(tab: popup::PopupTab, rewrite: &str, original: &str, anchor: &CaretRect) {
    match providers::classify_suggestion(rewrite, original) {
        providers::SuggestionOutcome::Suggestion => {
            popup::show_tabbed(
                tab,
                popup::TabContent::Diff {
                    original,
                    suggestion: rewrite,
                },
                anchor,
            );
        }
        providers::SuggestionOutcome::Unchanged => {
            popup::show_tabbed(
                tab,
                popup::TabContent::Message {
                    text: "This style doesn't change your text.",
                    is_error: false,
                    loading: false,
                },
                anchor,
            );
        }
        providers::SuggestionOutcome::Unclear => {
            popup::show_tabbed(
                tab,
                popup::TabContent::Message {
                    text: providers::UNCLEAR_MESSAGE,
                    is_error: false,
                    loading: false,
                },
                anchor,
            );
        }
    }
}

fn handle_tab_clicked(
    proxy: &EventLoopProxy<UserEvent>,
    tab: popup::PopupTab,
    current_generation: u64,
    pending: &mut Option<Pending>,
    last_anchor: &mut Option<CaretRect>,
) {
    let Some(active) = pending.as_mut() else {
        return;
    };
    if !popup::is_visible() {
        return;
    };
    let Some(anchor) = *last_anchor else {
        eprintln!("Writely: could not show the rewrite: the caret position is unknown.");
        return;
    };
    active.selected = tab;
    match tab {
        popup::PopupTab::Correct => match active.correction.as_deref() {
            Some(correction) => {
                popup::show_tabbed(
                    tab,
                    popup::TabContent::Diff {
                        original: &active.original,
                        suggestion: correction,
                    },
                    &anchor,
                );
            }
            None => {
                popup::show_tabbed(
                    tab,
                    popup::TabContent::Message {
                        text: looks_good_message(),
                        is_error: false,
                        loading: false,
                    },
                    &anchor,
                );
            }
        },
        popup::PopupTab::Rewrite(style) => {
            let index = style_index(style);
            if let Some(cached) = active.rewrites[index].as_deref() {
                show_rewrite_result(tab, cached, &active.original, &anchor);
            } else if active.rewrite_in_flight[index] {
                // A request for this style is already running: keep showing
                // its loading state instead of starting a second one.
                popup::show_tabbed(
                    tab,
                    popup::TabContent::Message {
                        text: style.progress_text(),
                        is_error: false,
                        loading: true,
                    },
                    &anchor,
                );
            } else {
                popup::show_tabbed(
                    tab,
                    popup::TabContent::Message {
                        text: style.progress_text(),
                        is_error: false,
                        loading: true,
                    },
                    &anchor,
                );
                active.rewrite_in_flight[index] = true;
                let (app_settings, key) = match resolve_settings() {
                    Ok(loaded) => loaded,
                    Err(failure) => {
                        active.rewrite_in_flight[index] = false;
                        popup::show_tabbed(
                            tab,
                            popup::TabContent::Message {
                                text: &failure.message,
                                is_error: failure.is_error,
                                loading: false,
                            },
                            &anchor,
                        );
                        return;
                    }
                };
                let text = active.original.clone();
                if let Err(error) =
                    spawn_rewrite_thread(proxy, app_settings, key, text, style, current_generation)
                {
                    active.rewrite_in_flight[index] = false;
                    popup::show_tabbed(
                        tab,
                        popup::TabContent::Message {
                            text: &error,
                            is_error: true,
                            loading: false,
                        },
                        &anchor,
                    );
                }
            }
        }
    }
}

fn handle_rewrite(
    current_generation: u64,
    event_generation: u64,
    style: providers::RewriteStyle,
    result: Result<String, String>,
    pending: &mut Option<Pending>,
    last_anchor: &mut Option<CaretRect>,
) {
    // Drop late replies: a newer click, typing that hid the popup, or a move
    // to another tab wins. The generation is never bumped for a rewrite.
    if event_generation != current_generation || !popup::is_visible() {
        return;
    }
    let Some(active) = pending.as_mut() else {
        return;
    };
    let index = style_index(style);
    active.rewrite_in_flight[index] = false;
    let tab = popup::PopupTab::Rewrite(style);
    let still_selected = active.selected == tab;
    match result {
        Ok(rewrite) => {
            let outcome = providers::classify_suggestion(&rewrite, &active.original);
            if still_selected {
                match *last_anchor {
                    Some(anchor) => show_rewrite_result(tab, &rewrite, &active.original, &anchor),
                    None => eprintln!(
                        "Writely: could not show the rewrite: the caret position is unknown."
                    ),
                }
            }
            // Unclear is not cached, so clicking the tab again retries.
            if outcome != providers::SuggestionOutcome::Unclear {
                active.rewrites[index] = Some(rewrite);
            }
        }
        Err(error) => {
            // Not cached, so clicking the tab again retries.
            let message = format!("Could not rewrite the text ({}): {error}", style.label());
            eprintln!("Writely: {message}");
            if still_selected {
                match *last_anchor {
                    Some(anchor) => {
                        popup::show_tabbed(
                            tab,
                            popup::TabContent::Message {
                                text: &message,
                                is_error: true,
                                loading: false,
                            },
                            &anchor,
                        );
                    }
                    None => eprintln!(
                        "Writely: could not show the rewrite: the caret position is unknown."
                    ),
                }
            }
        }
    }
}

fn handle_replace_clicked(pending: &mut Option<Pending>, last_anchor: &mut Option<CaretRect>) {
    let Some(active) = pending.take() else {
        return;
    };
    let Some(text) = selected_text(&active).map(str::to_string) else {
        // Nothing to insert (loading, error, or an already-correct tab):
        // put the pending suggestion back and do nothing.
        *pending = Some(active);
        return;
    };
    // The copy-and-paste path pastes the suggestion over the selection.
    #[cfg(target_os = "macos")]
    if active.via_clipboard {
        match clipboard::paste_suggestion(active.pid, &text) {
            Ok(()) => {
                popup::hide();
            }
            Err(error) => {
                show_popup(&error, true, false, *last_anchor, None, last_anchor);
            }
        }
        return;
    }
    let Some(snap) = active.snap else {
        eprintln!("Writely: no text to replace.");
        return;
    };
    match text_access::replace_range(&snap, active.range.clone(), &text) {
        Ok(()) => {
            popup::hide();
        }
        Err(error) => {
            // Paste fallback for partial snapshots: the text is still
            // selected from step 1 of the AX path, so re-select the snapshot
            // range first and paste over it instead of showing an error.
            #[cfg(target_os = "macos")]
            if !active.via_clipboard && !text_access::covers_whole_field(&snap) {
                match text_access::macos_helpers::reselect_range(&snap, active.range.clone()) {
                    Ok(()) => {
                        match clipboard::paste_suggestion(active.pid, &text) {
                            Ok(()) => {
                                popup::hide();
                            }
                            Err(paste_error) => {
                                show_popup(
                                    &paste_error,
                                    true,
                                    false,
                                    *last_anchor,
                                    Some(snap.caret_rect),
                                    last_anchor,
                                );
                            }
                        }
                        return;
                    }
                    Err(_) => {
                        // Re-select failed: fall through to the AX error below.
                    }
                }
            }
            show_popup(
                &error,
                true,
                false,
                *last_anchor,
                Some(snap.caret_rect),
                last_anchor,
            );
        }
    }
}

/// Start the caret watcher, logging failures the same way as startup.
fn start_watcher(proxy: &EventLoopProxy<UserEvent>) -> Option<watcher::Watcher> {
    match watcher::Watcher::start(proxy.clone()) {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            eprintln!("Writely: {error}");
            None
        }
    }
}

/// Poll `check_permission(false)` every 2 s until Accessibility is granted,
/// then send one `PermissionGranted` and exit. Never prompts.
fn spawn_permission_poll(proxy: EventLoopProxy<UserEvent>) {
    if let Err(error) = std::thread::Builder::new()
        .name("writely-permission".to_string())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            if text_access::check_permission(false).is_ok() {
                if let Err(send_error) = proxy.send_event(UserEvent::PermissionGranted) {
                    eprintln!("Writely: could not deliver the permission event: {send_error}");
                }
                break;
            }
        })
    {
        eprintln!("Writely: could not start the permission watcher: {error}");
    }
}

fn handle_permission_granted(
    proxy: &EventLoopProxy<UserEvent>,
    caret_watcher: &mut Option<watcher::Watcher>,
    tray_icon: &Option<TrayIcon>,
) {
    settings::set_permission_message(None);
    if caret_watcher.is_none() {
        *caret_watcher = start_watcher(proxy);
    }
    if let Some(tray) = tray_icon {
        if let Err(error) = tray.set_tooltip(Some("Writely")) {
            eprintln!("Writely: could not update the tray tooltip: {error}");
        }
    }
    settings::notify_permission_granted();
    eprintln!("Writely: Accessibility permission granted.");
}

fn main() {
    // Only the macOS setup mutates the event loop before running it.
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        event_loop.set_activate_ignoring_other_apps(false);
    }

    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        if let Err(error) = proxy.send_event(UserEvent::MenuEvent(event)) {
            eprintln!("Writely: could not deliver the tray menu event: {error}");
        }
    }));
    let watcher_proxy = event_loop.create_proxy();
    overlay::init(watcher_proxy.clone());
    popup::init(watcher_proxy.clone());
    check_window::init(watcher_proxy.clone());

    let mut tray_icon: Option<TrayIcon> = None;
    #[cfg(target_os = "macos")]
    let mut _hotkey_manager: Option<global_hotkey::GlobalHotKeyManager> = None;
    let mut settings_id = tray_icon::menu::MenuId::new("settings");
    let mut quit_id = tray_icon::menu::MenuId::new("quit");
    #[cfg(target_os = "linux")]
    let mut check_text_id = tray_icon::menu::MenuId::new("check-text");
    let mut caret_watcher: Option<watcher::Watcher> = None;
    let mut last_caret_error: Option<String> = None;
    // Popup flow state: bumped on every click and on hide so late Gemini
    // replies are dropped.
    let mut generation: u64 = 0;
    let mut checking: Option<Checking> = None;
    let mut pending: Option<Pending> = None;
    let mut last_anchor: Option<CaretRect> = None;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::NewEvents(StartCause::Init) => {
                // Apply the start-at-login setting (on by default): this
                // also heals the entry if the app moved since last launch.
                match config::load_settings() {
                    Ok(app_settings) => {
                        if let Err(error) = autostart::sync(app_settings.launch_at_login) {
                            eprintln!("Writely: {error}");
                            settings::set_autostart_message(Some(error));
                        }
                    }
                    Err(error) => eprintln!("Writely: {error}"),
                }
                let settings_item = MenuItem::with_id("settings", "Settings…", true, None);
                let quit_item = MenuItem::with_id("quit", "Quit Writely", true, None);
                #[cfg(target_os = "linux")]
                let check_text_item =
                    MenuItem::with_id("check-text", "Check text…", true, None);
                settings_id = settings_item.id().clone();
                quit_id = quit_item.id().clone();
                #[cfg(target_os = "linux")]
                {
                    check_text_id = check_text_item.id().clone();
                }

                #[cfg(target_os = "linux")]
                let menu_result =
                    Menu::with_items(&[&check_text_item, &settings_item, &quit_item]);
                #[cfg(not(target_os = "linux"))]
                let menu_result = Menu::with_items(&[&settings_item, &quit_item]);
                let menu = match menu_result {
                    Ok(menu) => menu,
                    Err(error) => {
                        eprintln!("Writely: could not create the tray icon: {error}");
                        std::process::exit(1);
                    }
                };

                let icon =
                    match Icon::from_rgba(include_bytes!("../icons/tray-32.rgba").to_vec(), 32, 32)
                    {
                        Ok(icon) => icon,
                        Err(error) => {
                            eprintln!("Writely: could not create the tray icon: {error}");
                            std::process::exit(1);
                        }
                    };

                let builder = TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    .with_icon(icon)
                    .with_tooltip("Writely");
                #[cfg(target_os = "macos")]
                let builder = builder.with_icon_as_template(true);

                match builder.build() {
                    Ok(icon) => {
                        tray_icon = Some(icon);
                    }
                    Err(error) => {
                        eprintln!("Writely: could not create the tray icon: {error}");
                        std::process::exit(1);
                    }
                }

                if watcher::is_wayland_session() {
                    const WAITING: &str = "On Wayland, Writely can't see what you type in other apps. Open “Check text…” from the Writely menu and paste your text there.";
                    eprintln!("Writely: {WAITING}");
                    settings::set_permission_message(Some(WAITING.to_string()));
                } else {
                    match text_access::check_permission(true) {
                        Ok(()) => {
                            caret_watcher = start_watcher(&watcher_proxy);
                        }
                        Err(error) => {
                            eprintln!("Writely: {error}");
                            settings::set_permission_message(Some(error));
                            if let Some(tray) = &tray_icon {
                                if let Err(tooltip_error) = tray.set_tooltip(Some(
                                    "Writely needs Accessibility permission — see Settings",
                                )) {
                                    eprintln!(
                                        "Writely: could not update the tray tooltip: {tooltip_error}"
                                    );
                                }
                            }
                            spawn_permission_poll(watcher_proxy.clone());
                        }
                    }
                }
                #[cfg(target_os = "macos")]
                {
                    _hotkey_manager = register_hotkey(&watcher_proxy);
                }
            }
            Event::UserEvent(UserEvent::MenuEvent(event)) => {
                #[cfg(target_os = "linux")]
                if event.id == check_text_id {
                    if let Err(error) = check_window::show() {
                        eprintln!("Writely: {error}");
                    }
                }
                if event.id == settings_id {
                    popup::hide();
                    // A stale permission message (e.g. granted after startup)
                    // is cleared here so Settings opens fresh. Skipped on
                    // Wayland: the watcher stays off and the Wayland message stays.
                    if !watcher::is_wayland_session()
                        && text_access::check_permission(false).is_ok()
                        && caret_watcher.is_none()
                    {
                        handle_permission_granted(
                            &watcher_proxy,
                            &mut caret_watcher,
                            &tray_icon,
                        );
                    }
                    if let Err(error) = settings::show_settings() {
                        eprintln!("Writely: {error}");
                    }
                } else if event.id == quit_id {
                    drop(tray_icon.take());
                    caret_watcher = None;
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UserEvent::CaretChanged) => {
                // Typing or moving focus hides the popup and drops any
                // in-flight suggestion.
                if popup::is_visible() {
                    popup::hide();
                    generation = generation.wrapping_add(1);
                    checking = None;
                    pending = None;
                }
                if let Some(watcher) = caret_watcher.as_mut() {
                    watcher.handle_caret_changed();
                }
                match text_access::caret_info() {
                    Ok(Some(info)) => {
                        last_caret_error = None;
                        overlay::show_at(&info.caret_rect);
                    }
                    Ok(None) => {
                        last_caret_error = None;
                        overlay::hide();
                    }
                    Err(error) => {
                        overlay::hide();
                        if last_caret_error.as_deref() != Some(error.as_str()) {
                            eprintln!("Writely: {error}");
                            last_caret_error = Some(error);
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::IconClicked) => {
                handle_icon_clicked(
                    &watcher_proxy,
                    &mut generation,
                    &mut checking,
                    &mut pending,
                    &mut last_anchor,
                    false,
                );
            }
            Event::UserEvent(UserEvent::HotkeyPressed) => {
                handle_icon_clicked(
                    &watcher_proxy,
                    &mut generation,
                    &mut checking,
                    &mut pending,
                    &mut last_anchor,
                    true,
                );
            }
            Event::UserEvent(UserEvent::ReplaceClicked) => {
                handle_replace_clicked(&mut pending, &mut last_anchor);
            }
            Event::UserEvent(UserEvent::TabClicked(tab)) => {
                handle_tab_clicked(
                    &watcher_proxy,
                    tab,
                    generation,
                    &mut pending,
                    &mut last_anchor,
                );
            }
            Event::UserEvent(UserEvent::PermissionGranted) => {
                handle_permission_granted(&watcher_proxy, &mut caret_watcher, &tray_icon);
            }
            #[cfg(target_os = "macos")]
            Event::UserEvent(UserEvent::CopiedText {
                generation: event_generation,
                result,
                pid,
                from_hotkey,
            }) => {
                handle_copied_text(
                    &watcher_proxy,
                    generation,
                    event_generation,
                    result,
                    pid,
                    from_hotkey,
                    &mut checking,
                    &mut pending,
                    &mut last_anchor,
                );
            }
            Event::UserEvent(UserEvent::Suggestion {
                generation: event_generation,
                result,
            }) => {
                handle_suggestion(
                    generation,
                    event_generation,
                    result,
                    &mut checking,
                    &mut pending,
                    &mut last_anchor,
                );
            }
            Event::UserEvent(UserEvent::Rewrite {
                generation: event_generation,
                style,
                result,
            }) => {
                handle_rewrite(
                    generation,
                    event_generation,
                    style,
                    result,
                    &mut pending,
                    &mut last_anchor,
                );
            }
            Event::UserEvent(UserEvent::TextCheckResult {
                generation: event_generation,
                result,
            }) => {
                check_window::handle_result(event_generation, result);
            }
            _ => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{selected_text, style_index, Pending};
    use crate::popup::PopupTab;
    use crate::providers::RewriteStyle;

    fn test_pending() -> Pending {
        Pending {
            snap: None,
            range: 0..16,
            original: "She go to school.".to_string(),
            correction: Some("She goes to school.".to_string()),
            rewrites: [None, None, None, None, None],
            selected: PopupTab::Correct,
            rewrite_in_flight: [false; 5],
            pid: 0,
            via_clipboard: false,
        }
    }

    #[test]
    fn style_index_matches_all_order() {
        for (index, style) in RewriteStyle::ALL.iter().enumerate() {
            assert_eq!(style_index(*style), index);
        }
    }

    #[test]
    fn selected_text_correct_tab() {
        let pending = test_pending();
        assert_eq!(selected_text(&pending), Some("She goes to school."));
        let mut without_correction = test_pending();
        without_correction.correction = None;
        assert_eq!(selected_text(&without_correction), None);
    }

    #[test]
    fn selected_text_rewrite_tab() {
        // Uncached: nothing to insert.
        let mut pending = test_pending();
        pending.selected = PopupTab::Rewrite(RewriteStyle::Friendly);
        assert_eq!(selected_text(&pending), None);
        // Cached rewrite: insert it.
        pending.rewrites[style_index(RewriteStyle::Friendly)] =
            Some("She goes to school, you know!".to_string());
        assert_eq!(
            selected_text(&pending),
            Some("She goes to school, you know!")
        );
        // Cached but unchanged: nothing to insert.
        pending.rewrites[style_index(RewriteStyle::Friendly)] =
            Some("She go to school.".to_string());
        assert_eq!(selected_text(&pending), None);
    }
}
