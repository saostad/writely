#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod check_text;
mod check_window;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod clipboard;
mod config;
mod diff;
mod local;
mod overlay;
mod popup;
mod providers;
mod settings;
mod text_access;
mod watcher;

use std::ops::Range;
use std::time::{Duration, Instant};

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use text_access::{CaretRect, FieldSnapshot};

/// Which in-flight request a streamed partial reply belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialKind {
    Check,
    Rewrite(providers::RewriteStyle),
    // Sent only by the macOS/Windows compose worker.
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    Compose,
    // Sent only by the macOS/Windows describe-a-change worker.
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    Change,
    TextCheck,
}

#[derive(Debug)]
pub enum UserEvent {
    MenuEvent(MenuEvent),
    CaretChanged,
    // Fired only by the Windows popup's dismiss controls (Cancel, the close
    // button, Esc in the text box): like CaretChanged for the popup state,
    // without the watcher/icon part.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    PopupDismissed,
    IconClicked,
    HotkeyPressed,
    ReplaceClicked,
    // Fired only by the macOS/Windows popup menu; the handler lands with the
    // rewrite wiring, so elsewhere (no menu) this is never constructed.
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    TabClicked(popup::PopupTab),
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    CopyClicked,
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    RetryClicked,
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    CopyFeedbackEnded(u64),
    // Fired only by the macOS/Windows input box.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    InputSubmitted(String),
    // Fired only by the macOS/Windows compose worker.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Composed {
        generation: u64,
        result: Result<String, String>,
    },
    // Fired only by the macOS/Windows describe-a-change worker.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Changed {
        generation: u64,
        result: Result<String, String>,
    },
    PermissionGranted,
    SettingsChanged,
    ProviderMenuState {
        id: u64,
        current: config::Provider,
        ready: [bool; 4],
    },
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    CopiedText {
        generation: u64,
        result: Result<clipboard::CopyResult, String>,
        pid: u32,
        from_hotkey: bool,
        from_terminal: bool,
    },
    Partial {
        generation: u64,
        kind: PartialKind,
        text: String,
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
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    pid: u32,
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    via_clipboard: bool,
    copy_only: bool,
    /// A composed draft (macOS/Windows): the style menu's first item reads
    /// "Draft" and Replace reads "Insert".
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    draft: bool,
    /// Latest describe-a-change result (macOS/Windows).
    custom: Option<String>,
    /// Text the describe-a-change request refines (macOS/Windows).
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    custom_source: String,
    /// Latest describe-a-change request, kept to re-show the box (macOS/Windows).
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    custom_request: String,
    /// A describe-a-change request is in flight (macOS/Windows).
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    custom_in_flight: bool,
}

/// A compose box waiting for the user's description: the snapshot and the
/// empty insert range, filled in once the draft arrives. Only the
/// macOS/Windows input box constructs it.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
struct Composing {
    snap: FieldSnapshot,
    range: Range<usize>,
    pid: u32,
    request: String,
    /// A compose request is in flight, so its partials may be shown; cleared
    /// when the final reply is handled so a late partial can't replace it.
    in_flight: bool,
}

/// The insert range for a compose: an empty range at the caret when the caret
/// offset is known, else the whole (blank) field.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
fn compose_range(caret_known: bool, caret: usize, field_len: usize) -> Range<usize> {
    if caret_known {
        caret..caret
    } else {
        0..field_len
    }
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
        popup::PopupTab::Custom => {
            let cached = pending.custom.as_deref()?;
            match providers::classify_suggestion(cached, &pending.original) {
                providers::SuggestionOutcome::Suggestion => Some(cached),
                providers::SuggestionOutcome::Unclear | providers::SuggestionOutcome::Unchanged => {
                    None
                }
            }
        }
    }
}

/// The text a describe-a-change request refines: what the selected tab shows,
/// or the original text when the tab shows nothing (looks-good, loading or
/// failed).
fn custom_source_text(pending: &Pending) -> &str {
    selected_text(pending).unwrap_or(&pending.original)
}

/// The selected rewrite style when Retry should re-request it: a rewrite tab
/// that is neither cached nor in flight. The Correct tab never retries.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
fn rewrite_retry_style(pending: &Pending) -> Option<providers::RewriteStyle> {
    match pending.selected {
        popup::PopupTab::Rewrite(style) => {
            let index = style_index(style);
            if pending.rewrites[index].is_none() && !pending.rewrite_in_flight[index] {
                Some(style)
            } else {
                None
            }
        }
        popup::PopupTab::Correct => None,
        // Custom errors retry through the input box, never the Retry button.
        popup::PopupTab::Custom => None,
    }
}

/// The "already correct" body for the Correct tab: on macOS and Windows it
/// points at the style menu in the footer; elsewhere it is today's plain
/// message (no menu there).
fn looks_good_message() -> &'static str {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        "Looks good — no changes needed. Pick a style below to rewrite it."
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        "Looks good — no changes needed."
    }
}

#[cfg(target_os = "macos")]
const HOTKEY_LABEL: &str = "⌃⌥G";
#[cfg(target_os = "windows")]
const HOTKEY_LABEL: &str = "Ctrl+Alt+G";

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

/// Mouse-pointer anchor for the hotkey popup on Windows: a zero-size rect
/// at the pointer in physical pixels.
#[cfg(target_os = "windows")]
fn mouse_anchor() -> Option<CaretRect> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut point = POINT::default();
    // SAFETY: point is a live POINT for the cursor position.
    unsafe { GetCursorPos(&mut point) }.ok()?;
    Some(CaretRect {
        x: point.x as f64,
        y: point.y as f64,
        width: 0.0,
        height: 0.0,
        physical: true,
    })
}

/// The foreground window's pid on Windows, when it isn't Writely itself.
#[cfg(target_os = "windows")]
fn foreground_pid() -> Option<u32> {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    // SAFETY: querying the foreground window takes no borrowed state.
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.is_invalid() {
        return None;
    }
    let mut pid: u32 = 0;
    // SAFETY: foreground is a live window and pid is writable.
    let _thread = unsafe { GetWindowThreadProcessId(foreground, Some(&mut pid)) };
    if pid == 0 || pid == std::process::id() {
        None
    } else {
        Some(pid)
    }
}

/// Registers the global hotkey (⌃⌥G) on macOS. Returns the manager (kept
/// alive by the caller) or `None` when registration failed; failures are
/// logged and shown in the Settings status line with the real error.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn register_hotkey(
    proxy: &EventLoopProxy<UserEvent>,
) -> Option<global_hotkey::GlobalHotKeyManager> {
    use global_hotkey::hotkey::{Code, HotKey, Modifiers};
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(error) => {
            let message = format!("Could not register the {HOTKEY_LABEL} hotkey: {error}");
            eprintln!("Writely: {message}");
            settings::set_hotkey_message(Some(message));
            return None;
        }
    };
    let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyG);
    if let Err(error) = manager.register(hotkey) {
        let message = format!("Could not register the {HOTKEY_LABEL} hotkey: {error}");
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
/// grammar check does. `Ok` carries `(settings, key)` with `key == None` for
/// providers that need no key; `Err` is the user-facing popup message with its severity.
fn resolve_settings() -> Result<(config::AppSettings, Option<String>), SettingsFailure> {
    let app_settings = config::load_settings().map_err(SettingsFailure::error)?;
    if app_settings.current_model().is_empty() {
        return Err(SettingsFailure::guidance(
            "Choose a model first: click the Writely menu-bar icon › Settings…".to_string(),
        ));
    }
    // Providers that need no key run locally; the others read the
    // selected provider's key from the keychain (never shown or logged).
    let key: Option<String> = if app_settings.provider.needs_key() {
        settings::load_api_key_for(app_settings.provider).map_err(SettingsFailure::error)?
    } else {
        None
    };
    if key.is_none() && app_settings.provider.needs_key() {
        return Err(SettingsFailure::guidance(format!(
            "Add your {} API key first: click the Writely menu-bar icon › Settings…",
            app_settings.provider.display_name()
        )));
    }
    Ok((app_settings, key))
}

/// Sends one streamed partial reply from a worker. A send failure is logged
/// once per worker (`failed` is the worker's flag); later partials are
/// skipped, but the final result is still sent as usual.
fn send_partial(
    proxy: &EventLoopProxy<UserEvent>,
    failed: &mut bool,
    generation: u64,
    kind: PartialKind,
    text: &str,
) {
    if *failed {
        return;
    }
    if let Err(error) = proxy.send_event(UserEvent::Partial {
        generation,
        kind,
        text: text.to_string(),
    }) {
        eprintln!("Writely: could not deliver the partial reply: {error}");
        *failed = true;
    }
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
            let partial_proxy = worker_proxy.clone();
            let mut partial_failed = false;
            let result =
                providers::suggest_stream(&app_settings, key.as_deref(), &sentence, &mut |text| {
                    send_partial(
                        &partial_proxy,
                        &mut partial_failed,
                        my_generation,
                        PartialKind::Check,
                        text,
                    );
                });
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
/// `providers::rewrite_stream` off the main thread and sends `UserEvent::Rewrite`.
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
            let partial_proxy = worker_proxy.clone();
            let mut partial_failed = false;
            let result = providers::rewrite_stream(
                &app_settings,
                key.as_deref(),
                &text,
                style,
                &mut |text| {
                    send_partial(
                        &partial_proxy,
                        &mut partial_failed,
                        my_generation,
                        PartialKind::Rewrite(style),
                        text,
                    );
                },
            );
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

/// Remembers the snapshot and the empty insert range and shows the compose
/// box. macOS and Windows; Linux keeps "Type a sentence first."
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn start_composing(
    snap: FieldSnapshot,
    composing: &mut Option<Composing>,
    anchor: Option<CaretRect>,
    last_anchor: &mut Option<CaretRect>,
) {
    let range = compose_range(snap.caret_known, snap.caret, snap.text.chars().count());
    let pid = snap.pid;
    *composing = Some(Composing {
        snap,
        range,
        pid,
        request: String::new(),
        in_flight: false,
    });
    if let Some(rect) = anchor {
        *last_anchor = Some(rect);
        popup::show_input(popup::InputKind::Compose, "", None, &rect);
    } else {
        eprintln!("Writely: could not show the suggestion: the caret position is unknown.");
    }
}

/// One compose worker (same pattern as the grammar check): calls
/// `providers::compose_stream` off the main thread and sends `UserEvent::Composed`.
/// Returns the spawn error for the caller to show.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn spawn_compose_thread(
    proxy: &EventLoopProxy<UserEvent>,
    app_settings: config::AppSettings,
    key: Option<String>,
    request: String,
    my_generation: u64,
) -> Result<(), String> {
    let worker_proxy = proxy.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("writely-compose".to_string())
        .spawn(move || {
            // The key lives only in this thread's request; it is never
            // logged or put in an error.
            let partial_proxy = worker_proxy.clone();
            let mut partial_failed = false;
            let result =
                providers::compose_stream(&app_settings, key.as_deref(), &request, &mut |text| {
                    send_partial(
                        &partial_proxy,
                        &mut partial_failed,
                        my_generation,
                        PartialKind::Compose,
                        text,
                    );
                });
            if let Err(error) = worker_proxy.send_event(UserEvent::Composed {
                generation: my_generation,
                result,
            }) {
                eprintln!("Writely: could not deliver the composed text: {error}");
            }
        })
    {
        return Err(format!("Could not write the text: {error}"));
    }
    Ok(())
}

/// One describe-a-change worker (same pattern as the rewrite): calls
/// `providers::change_stream` off the main thread and sends `UserEvent::Changed`.
/// Returns the spawn error for the caller to show.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn spawn_change_thread(
    proxy: &EventLoopProxy<UserEvent>,
    app_settings: config::AppSettings,
    key: Option<String>,
    source: String,
    request: String,
    my_generation: u64,
) -> Result<(), String> {
    let worker_proxy = proxy.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("writely-change".to_string())
        .spawn(move || {
            // The key lives only in this thread's request; it is never
            // logged or put in an error.
            let partial_proxy = worker_proxy.clone();
            let mut partial_failed = false;
            let result = providers::change_stream(
                &app_settings,
                key.as_deref(),
                &source,
                &request,
                &mut |text| {
                    send_partial(
                        &partial_proxy,
                        &mut partial_failed,
                        my_generation,
                        PartialKind::Change,
                        text,
                    );
                },
            );
            if let Err(error) = worker_proxy.send_event(UserEvent::Changed {
                generation: my_generation,
                result,
            }) {
                eprintln!("Writely: could not deliver the changed text: {error}");
            }
        })
    {
        return Err(format!("Could not rewrite the text: {error}"));
    }
    Ok(())
}

/// Handles an input-box submit: a describe-a-change submit when a suggestion
/// is pending on the Custom tab, else a compose-box submit (only when a
/// compose is open and no suggestion is showing).
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn handle_input_submitted(
    proxy: &EventLoopProxy<UserEvent>,
    request: String,
    current_generation: u64,
    composing: &mut Option<Composing>,
    pending: &mut Option<Pending>,
    last_anchor: &mut Option<CaretRect>,
) {
    if pending
        .as_ref()
        .is_some_and(|active| active.selected == popup::PopupTab::Custom)
    {
        handle_change_submitted(proxy, request, current_generation, pending, last_anchor);
        return;
    }
    let Some(active) = composing.as_mut() else {
        return;
    };
    if pending.is_some() || !popup::is_visible() {
        return;
    }
    let Some(anchor) = (*last_anchor).or(Some(active.snap.caret_rect)) else {
        eprintln!("Writely: could not show the suggestion: the caret position is unknown.");
        return;
    };
    *last_anchor = Some(anchor);
    if request.chars().count() > providers::MAX_REQUEST_CHARS {
        popup::show_input(
            popup::InputKind::Compose,
            &request,
            Some("Keep the description under 1,000 characters."),
            &anchor,
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
                Some(anchor),
                None,
                last_anchor,
            );
            return;
        }
    };
    active.request = request.clone();
    active.in_flight = true;
    show_popup("Writing…", false, false, Some(anchor), None, last_anchor);
    if let Err(error) = spawn_compose_thread(proxy, app_settings, key, request, current_generation)
    {
        active.in_flight = false;
        let request = &active.request;
        popup::show_input(popup::InputKind::Compose, request, Some(&error), &anchor);
    }
}

/// Handles a describe-a-change submit: shows "Rewriting…" and spawns the
/// change worker. Only when a suggestion is pending on the Custom tab.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn handle_change_submitted(
    proxy: &EventLoopProxy<UserEvent>,
    request: String,
    current_generation: u64,
    pending: &mut Option<Pending>,
    last_anchor: &mut Option<CaretRect>,
) {
    let Some(active) = pending.as_mut() else {
        return;
    };
    if active.selected != popup::PopupTab::Custom || !popup::is_visible() {
        return;
    }
    let Some(anchor) = *last_anchor else {
        eprintln!("Writely: could not show the rewrite: the caret position is unknown.");
        return;
    };
    *last_anchor = Some(anchor);
    if request.chars().count() > providers::MAX_REQUEST_CHARS {
        popup::show_input(
            popup::InputKind::Change,
            &request,
            Some("Keep the description under 1,000 characters."),
            &anchor,
        );
        return;
    }
    popup::show_tabbed(
        popup::PopupTab::Custom,
        popup::TabContent::Message {
            text: CUSTOM_PROGRESS_TEXT,
            is_error: false,
            loading: true,
            retry: false,
        },
        &anchor,
    );
    active.custom_in_flight = true;
    let (app_settings, key) = match resolve_settings() {
        Ok(loaded) => loaded,
        Err(failure) => {
            active.custom_in_flight = false;
            popup::show_tabbed(
                popup::PopupTab::Custom,
                popup::TabContent::Message {
                    text: &failure.message,
                    is_error: failure.is_error,
                    loading: false,
                    retry: false,
                },
                &anchor,
            );
            return;
        }
    };
    active.custom_request = request.clone();
    let source = active.custom_source.clone();
    if let Err(error) = spawn_change_thread(
        proxy,
        app_settings,
        key,
        source,
        request,
        current_generation,
    ) {
        active.custom_in_flight = false;
        let request = &active.custom_request;
        popup::show_input(popup::InputKind::Change, request, Some(&error), &anchor);
    }
}

/// Handles the compose worker's reply: errors and UNCLEAR re-show the box
/// with the typed text kept; a draft becomes a `Pending` shown as a diff of
/// the draft against itself (plain text) in draft mode.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn handle_composed(
    current_generation: u64,
    event_generation: u64,
    result: Result<String, String>,
    composing: &mut Option<Composing>,
    pending: &mut Option<Pending>,
    last_anchor: &mut Option<CaretRect>,
) {
    // Drop late replies: a newer click, or typing that hid the popup, wins.
    if event_generation != current_generation || !popup::is_visible() {
        return;
    }
    if let Some(active) = composing.as_mut() {
        active.in_flight = false;
    }
    let Some(active) = composing.as_ref() else {
        return;
    };
    let Some(anchor) = (*last_anchor).or(Some(active.snap.caret_rect)) else {
        eprintln!("Writely: could not show the suggestion: the caret position is unknown.");
        return;
    };
    *last_anchor = Some(anchor);
    match result {
        Err(error) => {
            let message = format!("Could not write the text: {error}");
            eprintln!("Writely: {message}");
            popup::show_input(
                popup::InputKind::Compose,
                &active.request,
                Some(&message),
                &anchor,
            );
        }
        Ok(draft) => {
            if providers::classify_suggestion(&draft, "") == providers::SuggestionOutcome::Unclear {
                popup::show_input(
                    popup::InputKind::Compose,
                    &active.request,
                    Some(
                        "Writely couldn't understand the description. Try describing it differently.",
                    ),
                    &anchor,
                );
                return;
            }
            let Some(composed) = composing.take() else {
                return;
            };
            popup::set_draft_mode(true);
            popup::show_tabbed(
                popup::PopupTab::Correct,
                popup::TabContent::Diff {
                    original: &draft,
                    suggestion: &draft,
                },
                &anchor,
            );
            *pending = Some(Pending {
                snap: Some(composed.snap),
                range: composed.range,
                original: draft.clone(),
                correction: Some(draft),
                rewrites: [None, None, None, None, None],
                selected: popup::PopupTab::Correct,
                rewrite_in_flight: [false; 5],
                pid: composed.pid,
                via_clipboard: false,
                copy_only: false,
                draft: true,
                custom: None,
                custom_source: String::new(),
                custom_request: String::new(),
                custom_in_flight: false,
            });
        }
    }
}

/// Handles the describe-a-change worker's reply: a result is cached and shown
/// as a diff against the original when Custom is still selected; errors and
/// UNCLEAR re-show the box with the typed request kept. Like `handle_rewrite`,
/// the generation is never bumped and nothing shows on another tab.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn handle_changed(
    current_generation: u64,
    event_generation: u64,
    result: Result<String, String>,
    pending: &mut Option<Pending>,
    last_anchor: &mut Option<CaretRect>,
) {
    // Drop late replies: a newer click, typing that hid the popup, or a move
    // to another tab wins.
    if event_generation != current_generation || !popup::is_visible() {
        return;
    }
    let Some(active) = pending.as_mut() else {
        return;
    };
    active.custom_in_flight = false;
    let tab = popup::PopupTab::Custom;
    let still_selected = active.selected == tab;
    match result {
        Ok(changed) => {
            if providers::classify_suggestion(&changed, &active.original)
                == providers::SuggestionOutcome::Unclear
            {
                // Not cached, so picking the tab again retries.
                if still_selected {
                    match *last_anchor {
                        Some(anchor) => popup::show_input(
                            popup::InputKind::Change,
                            &active.custom_request,
                            Some(
                                "Writely couldn't understand the request. Try describing it differently.",
                            ),
                            &anchor,
                        ),
                        None => eprintln!(
                            "Writely: could not show the rewrite: the caret position is unknown."
                        ),
                    }
                }
                return;
            }
            if still_selected {
                match *last_anchor {
                    Some(anchor) => {
                        show_rewrite_result(tab, &changed, &active.original, &anchor);
                    }
                    None => eprintln!(
                        "Writely: could not show the rewrite: the caret position is unknown."
                    ),
                }
            }
            active.custom = Some(changed);
        }
        Err(error) => {
            // Not cached, so picking the tab again retries.
            let message = format!("Could not rewrite the text: {error}");
            eprintln!("Writely: {message}");
            if still_selected {
                match *last_anchor {
                    Some(anchor) => popup::show_input(
                        popup::InputKind::Change,
                        &active.custom_request,
                        Some(&message),
                        &anchor,
                    ),
                    None => eprintln!(
                        "Writely: could not show the rewrite: the caret position is unknown."
                    ),
                }
            }
        }
    }
}

/// Copy-and-paste flow (macOS): the layered chain (AX selection, menu-bar
/// Copy, simulated ⌘C) runs in a `writely-copy` worker thread — it can take
/// up to 2 s — which sends `UserEvent::CopiedText`. Never runs for a secure
/// field (the caller checks that first).
#[cfg(any(target_os = "macos", target_os = "windows"))]
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
            let from_terminal = text_access::is_terminal_app(pid);
            let result = clipboard::layered_copy_text(pid);
            if let Err(send_error) = worker_proxy.send_event(UserEvent::CopiedText {
                generation: my_generation,
                result,
                pid,
                from_hotkey,
                from_terminal,
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
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[allow(clippy::too_many_arguments)]
fn handle_copied_text(
    proxy: &EventLoopProxy<UserEvent>,
    current_generation: u64,
    event_generation: u64,
    result: Result<clipboard::CopyResult, String>,
    pid: u32,
    from_hotkey: bool,
    from_terminal: bool,
    checking: &mut Option<Checking>,
    pending: &mut Option<Pending>,
    last_anchor: &mut Option<CaretRect>,
) {
    // Drop late results: a newer click, or typing that hid the popup, wins.
    if event_generation != current_generation || !popup::is_visible() {
        return;
    }
    #[cfg(target_os = "windows")]
    let empty_message = if from_hotkey {
        "Select the text you want to check, then press Ctrl+Alt+G."
    } else {
        "Select the text you want to check, then click W."
    };
    #[cfg(not(target_os = "windows"))]
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
    // A terminal selection that ends in a newline is a real selection,
    // not an editor's whole-line copy: skip the guard for terminal text.
    if !from_terminal && clipboard::looks_like_line_copy(&text) {
        #[cfg(target_os = "windows")]
        let line_copy_message = if from_hotkey {
            "Writely copied a whole line, which usually means nothing was selected. Select the text you want to check, then press Ctrl+Alt+G."
        } else {
            "Writely copied a whole line, which usually means nothing was selected. Select the text you want to check, then click W."
        };
        #[cfg(not(target_os = "windows"))]
        let line_copy_message = if from_hotkey {
            "Writely copied a whole line, which usually means nothing was selected. Select the text you want to check, then press ⌃⌥G."
        } else {
            "Writely copied a whole line, which usually means nothing was selected. Select the text you want to check, then click W."
        };
        eprintln!("Writely: the copied text was a whole line; asked the user to select text.");
        show_popup(line_copy_message, false, false, anchor, None, last_anchor);
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

#[allow(clippy::too_many_arguments)]
fn handle_icon_clicked(
    proxy: &EventLoopProxy<UserEvent>,
    generation: &mut u64,
    checking: &mut Option<Checking>,
    pending: &mut Option<Pending>,
    failed_check: &mut Option<Checking>,
    composing: &mut Option<Composing>,
    last_anchor: &mut Option<CaretRect>,
    from_hotkey: bool,
) {
    // A new click invalidates any in-flight request; stale replies are dropped.
    *generation = generation.wrapping_add(1);
    let my_generation = *generation;
    *pending = None;
    *checking = None;
    *failed_check = None;
    *composing = None;
    // `live_pid` is only consumed by macOS-only paths below; keep the
    // binding on all targets and allow it to be unused elsewhere.
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "windows")),
        allow(unused_variables)
    )]
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
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let (live_anchor, live_pid) = if from_hotkey && live_anchor.is_none() {
        (mouse_anchor().or(live_anchor), live_pid)
    } else {
        (live_anchor, live_pid)
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = from_hotkey;
    // On Windows a hotkey press with no caret still needs a pid for the
    // copy fallback: use the foreground app (never Writely itself).
    #[cfg(target_os = "windows")]
    let live_pid = if from_hotkey {
        live_pid.or_else(foreground_pid)
    } else {
        live_pid
    };
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
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if app_settings.clipboard_fallback && !text_access::focused_is_secure() {
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
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                {
                    start_composing(snap, composing, anchor, last_anchor);
                    return;
                }
                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                {
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
            text_access::NoCaretDecision::TooLong => {
                // The whole field doesn't fit: on macOS try the clipboard
                // (which re-applies the limit to the copied text).
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                if let Some(pid) = live_pid
                    .or(Some(snap.pid))
                    .filter(|_| app_settings.clipboard_fallback)
                {
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
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                {
                    start_composing(snap, composing, anchor, last_anchor);
                    return;
                }
                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                {
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
        }
    };
    if sentence.trim().is_empty() {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            start_composing(snap, composing, anchor, last_anchor);
            return;
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
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

/// The live-state facts `partial_target` needs, copied out of
/// `checking`/`pending`/`composing` so the drop rules stay pure.
#[derive(Clone, Copy, Default)]
struct PartialState {
    checking: bool,
    pending: bool,
    rewrite_selected: bool,
    rewrite_cached: bool,
    rewrite_in_flight: bool,
    composing_in_flight: bool,
    change_selected: bool,
    change_in_flight: bool,
    text_check_in_flight: bool,
}

/// Whether a partial may be shown: exactly when the final event for the same
/// kind would not be dropped. A stale generation always drops; the popup
/// kinds also need the popup visible and their in-flight state still live
/// (each final handler clears that state, so a partial can never replace a
/// final result). `TextCheck` has no popup: its window enforces the same
/// generation/in-flight rule itself.
fn partial_target(
    kind: PartialKind,
    generation: u64,
    current_generation: u64,
    popup_visible: bool,
    state: PartialState,
) -> bool {
    if generation != current_generation {
        return false;
    }
    match kind {
        PartialKind::TextCheck => state.text_check_in_flight,
        _ if !popup_visible => false,
        PartialKind::Check => state.checking,
        PartialKind::Rewrite(_) => {
            state.pending
                && state.rewrite_selected
                && !state.rewrite_cached
                && state.rewrite_in_flight
        }
        PartialKind::Compose => state.composing_in_flight,
        PartialKind::Change => state.pending && state.change_selected && state.change_in_flight,
    }
}

/// Shows a streamed partial where the waiting message is shown today; the
/// final handlers replace it exactly as before.
#[allow(clippy::too_many_arguments)]
fn handle_partial(
    current_generation: u64,
    generation: u64,
    kind: PartialKind,
    text: String,
    checking: &Option<Checking>,
    pending: &Option<Pending>,
    composing: &Option<Composing>,
    last_anchor: &mut Option<CaretRect>,
) {
    // The Check text window counts its own generations and drops stale
    // replies itself, like `TextCheckResult`.
    if kind == PartialKind::TextCheck {
        check_window::show_partial(generation, &text);
        return;
    }
    let state = PartialState {
        checking: checking.is_some(),
        pending: pending.is_some(),
        rewrite_selected: matches!(kind, PartialKind::Rewrite(style) if pending
            .as_ref()
            .is_some_and(|active| active.selected == popup::PopupTab::Rewrite(style))),
        rewrite_cached: matches!(kind, PartialKind::Rewrite(style) if pending
            .as_ref()
            .is_some_and(|active| active.rewrites[style_index(style)].is_some())),
        rewrite_in_flight: matches!(kind, PartialKind::Rewrite(style) if pending
            .as_ref()
            .is_some_and(|active| active.rewrite_in_flight[style_index(style)])),
        composing_in_flight: composing.as_ref().is_some_and(|active| active.in_flight),
        change_selected: pending
            .as_ref()
            .is_some_and(|active| active.selected == popup::PopupTab::Custom),
        change_in_flight: pending
            .as_ref()
            .is_some_and(|active| active.custom_in_flight),
        // Delegated to the Check text window above.
        text_check_in_flight: false,
    };
    if !partial_target(
        kind,
        generation,
        current_generation,
        popup::is_visible(),
        state,
    ) {
        return;
    }
    match kind {
        PartialKind::Check => {
            show_popup(&text, false, false, *last_anchor, None, last_anchor);
        }
        PartialKind::Rewrite(style) => {
            let Some(anchor) = *last_anchor else {
                eprintln!("Writely: could not show the rewrite: the caret position is unknown.");
                return;
            };
            popup::show_tabbed(
                popup::PopupTab::Rewrite(style),
                popup::TabContent::Message {
                    text: &text,
                    is_error: false,
                    loading: true,
                    retry: false,
                },
                &anchor,
            );
        }
        PartialKind::Compose => {
            show_popup(
                &text,
                false,
                false,
                *last_anchor,
                composing.as_ref().map(|active| active.snap.caret_rect),
                last_anchor,
            );
        }
        PartialKind::Change => {
            let Some(anchor) = *last_anchor else {
                eprintln!("Writely: could not show the rewrite: the caret position is unknown.");
                return;
            };
            popup::show_tabbed(
                popup::PopupTab::Custom,
                popup::TabContent::Message {
                    text: &text,
                    is_error: false,
                    loading: true,
                    retry: false,
                },
                &anchor,
            );
        }
        PartialKind::TextCheck => {
            // Handled above.
        }
    }
}

fn handle_suggestion(
    current_generation: u64,
    event_generation: u64,
    result: Result<String, String>,
    checking: &mut Option<Checking>,
    pending: &mut Option<Pending>,
    failed_check: &mut Option<Checking>,
    last_anchor: &mut Option<CaretRect>,
) {
    // Drop late replies: a newer click, or typing that hid the popup, wins.
    if event_generation != current_generation || !popup::is_visible() {
        return;
    }
    match result {
        Err(error) => {
            *pending = None;
            *failed_check = checking.take();
            if let Some(failed) = failed_check.as_ref() {
                if let Some(rect) =
                    (*last_anchor).or(failed.snap.as_ref().map(|snap| snap.caret_rect))
                {
                    *last_anchor = Some(rect);
                    popup::show_error_with_retry(&error, &rect);
                } else {
                    eprintln!(
                        "Writely: could not show the suggestion: the caret position is unknown."
                    );
                }
            } else {
                show_popup(&error, true, false, *last_anchor, None, last_anchor);
            }
        }
        Ok(suggestion) => {
            let Some(active) = checking.take() else {
                return;
            };
            let copy_only = active.via_clipboard && text_access::is_terminal_app(active.pid);
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
                        popup::set_primary_label(if copy_only { "Copy" } else { "Replace" });
                        // A normal check is never a draft: clear draft mode from a compose.
                        #[cfg(any(target_os = "macos", target_os = "windows"))]
                        popup::set_draft_mode(false);
                        popup::show_tabbed(
                            popup::PopupTab::Correct,
                            popup::TabContent::Message {
                                text: looks_good_message(),
                                is_error: false,
                                loading: false,
                                retry: false,
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
                        copy_only,
                        draft: false,
                        custom: None,
                        custom_source: String::new(),
                        custom_request: String::new(),
                        custom_in_flight: false,
                    });
                }
                providers::SuggestionOutcome::Suggestion => {
                    if let Some(rect) =
                        (*last_anchor).or(active.snap.as_ref().map(|snap| snap.caret_rect))
                    {
                        *last_anchor = Some(rect);
                        popup::set_primary_label(if copy_only { "Copy" } else { "Replace" });
                        // A normal check is never a draft: clear draft mode from a compose.
                        #[cfg(any(target_os = "macos", target_os = "windows"))]
                        popup::set_draft_mode(false);
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
                        copy_only,
                        draft: false,
                        custom: None,
                        custom_source: String::new(),
                        custom_request: String::new(),
                        custom_in_flight: false,
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
                    retry: false,
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
                    retry: false,
                },
                anchor,
            );
        }
    }
}

/// Progress text while a describe-a-change request loads. macOS only.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const CUSTOM_PROGRESS_TEXT: &str = "Rewriting…";

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
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    popup::set_draft_mode(active.draft);
    // A describe-a-change request refines the text on screen: capture it
    // before switching tabs, so a second "Describe a change…" refines the
    // latest result.
    if tab == popup::PopupTab::Custom {
        active.custom_source = custom_source_text(active).to_string();
    }
    active.selected = tab;
    popup::set_primary_label(if active.copy_only { "Copy" } else { "Replace" });
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
                        retry: false,
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
                        retry: false,
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
                        retry: false,
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
                                retry: false,
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
                            retry: true,
                        },
                        &anchor,
                    );
                }
            }
        }
        popup::PopupTab::Custom => {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            {
                if active.custom_in_flight {
                    // A request is already running: keep showing its loading
                    // state instead of starting a second one.
                    popup::show_tabbed(
                        tab,
                        popup::TabContent::Message {
                            text: CUSTOM_PROGRESS_TEXT,
                            is_error: false,
                            loading: true,
                            retry: false,
                        },
                        &anchor,
                    );
                } else {
                    popup::show_input(popup::InputKind::Change, "", None, &anchor);
                }
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                // Never constructed on Linux; the input box is macOS/Windows only.
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
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    popup::set_draft_mode(active.draft);
    let index = style_index(style);
    active.rewrite_in_flight[index] = false;
    let tab = popup::PopupTab::Rewrite(style);
    popup::set_primary_label(if active.copy_only { "Copy" } else { "Replace" });
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
                                retry: true,
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

// On Windows the paste-fallback block always returns, so the shared tail is unreachable there.
#[cfg_attr(target_os = "windows", allow(unreachable_code))]
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
    // Terminals: copy the suggestion to the clipboard instead of pasting
    // it over a selection (a terminal selection is a screen selection).
    #[cfg(target_os = "windows")]
    if active.copy_only {
        match copy_suggestion_message(&text) {
            Ok(()) => {
                show_popup(
                    "Copied. Paste it where you need it.",
                    false,
                    false,
                    *last_anchor,
                    None,
                    last_anchor,
                );
            }
            Err(message) => {
                show_popup(&message, true, false, *last_anchor, None, last_anchor);
            }
        }
        return;
    }
    // The copy-and-paste path pastes the suggestion over the selection.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
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
            // Paste fallback on Windows: select the checked range through
            // UI Automation, verify it, and paste over it. The original
            // replace error is never dropped.
            #[cfg(target_os = "windows")]
            if clipboard_fallback_enabled() {
                let paste_result = text_access::select_range(&snap, active.range.clone())
                    .and_then(|()| clipboard::paste_suggestion(active.pid, &text));
                match paste_result {
                    Ok(()) => {
                        popup::hide();
                    }
                    Err(paste_error) => {
                        show_popup(
                            &format!("{error} Writely also couldn't paste it: {paste_error}"),
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
            // Paste fallback for partial snapshots: the text is still
            // selected from step 1 of the AX path, so re-select the snapshot
            // range first and paste over it instead of showing an error.
            #[cfg(target_os = "macos")]
            if !active.via_clipboard
                && !text_access::covers_whole_field(&snap)
                && clipboard_fallback_enabled()
            {
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

/// Whether the clipboard fallback is on. Unreadable settings keep the
/// default (on), matching how the rest of the app treats them.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn clipboard_fallback_enabled() -> bool {
    config::load_settings()
        .map(|settings| settings.clipboard_fallback)
        .unwrap_or(true)
}

/// Copies the suggestion to the clipboard for the copy button and the
/// terminal branch of `handle_replace_clicked`, so the error string lives in
/// one place.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn copy_suggestion_message(text: &str) -> Result<(), String> {
    clipboard::copy_suggestion(text)
        .map_err(|error| format!("Could not copy the suggestion: {error}"))
}

/// Copies the shown suggestion without taking `pending`: the popup stays open
/// and Replace still works afterwards. No text means nothing to copy.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn handle_copy_clicked(
    pending: &Option<Pending>,
    proxy: &EventLoopProxy<UserEvent>,
    copy_feedback_id: &mut u64,
    last_anchor: &mut Option<CaretRect>,
) {
    let Some(active) = pending.as_ref() else {
        return;
    };
    let Some(text) = selected_text(active).map(str::to_string) else {
        return;
    };
    match copy_suggestion_message(&text) {
        Ok(()) => {
            popup::set_copy_feedback(true);
            *copy_feedback_id = copy_feedback_id.wrapping_add(1);
            let id = *copy_feedback_id;
            let proxy = proxy.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                if let Err(error) = proxy.send_event(UserEvent::CopyFeedbackEnded(id)) {
                    eprintln!("Writely: could not deliver the copy feedback event: {error}");
                }
            });
        }
        Err(message) => {
            show_popup(&message, true, false, *last_anchor, None, last_anchor);
        }
    }
}

/// Re-sends the failed request behind the Retry button: an uncached, idle
/// rewrite on the selected tab, else the failed grammar check. Settings
/// problems are shown without Retry; anything else does nothing.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn handle_retry_clicked(
    proxy: &EventLoopProxy<UserEvent>,
    generation: &mut u64,
    checking: &mut Option<Checking>,
    pending: &mut Option<Pending>,
    failed_check: &mut Option<Checking>,
    last_anchor: &mut Option<CaretRect>,
) {
    if !popup::is_visible() {
        return;
    }
    // Rewrite retry: the selected tab's style is still uncached and idle, so
    // re-clicking the tab re-requests it and shows the loading state.
    let retry_style = pending.as_ref().and_then(rewrite_retry_style);
    if let Some(style) = retry_style {
        handle_tab_clicked(
            proxy,
            popup::PopupTab::Rewrite(style),
            *generation,
            pending,
            last_anchor,
        );
        return;
    }
    // Check retry: re-send the failed sentence with a new generation, so a
    // stale reply from the failed request can't land.
    let Some(active) = failed_check.take() else {
        return;
    };
    *generation = generation.wrapping_add(1);
    let my_generation = *generation;
    show_popup("Checking…", false, false, *last_anchor, None, last_anchor);
    let (app_settings, key) = match resolve_settings() {
        Ok(loaded) => loaded,
        Err(failure) => {
            show_popup(
                &failure.message,
                failure.is_error,
                false,
                *last_anchor,
                None,
                last_anchor,
            );
            return;
        }
    };
    let sentence = active.sentence.clone();
    let anchor = *last_anchor;
    *checking = Some(active);
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

/// Text of the tray menu's pause item for the current paused state.
fn pause_item_text(paused: bool) -> &'static str {
    if paused {
        "Resume Writely"
    } else {
        "Pause Writely"
    }
}

/// Tooltip while Accessibility permission is missing.
const ACCESSIBILITY_TOOLTIP: &str = "Writely needs Accessibility permission — see Settings";

/// Idle time after the last caret move before the W icon reappears.
const ICON_IDLE_DELAY: Duration = Duration::from_millis(1000);

/// A copy of an RGBA buffer with every alpha byte scaled by 0.4 (rounded
/// down); the RGB bytes are untouched.
fn dimmed_rgba(rgba: &[u8]) -> Vec<u8> {
    rgba.iter()
        .enumerate()
        .map(|(index, byte)| {
            if index % 4 == 3 {
                (f32::from(*byte) * 0.4) as u8
            } else {
                *byte
            }
        })
        .collect()
}

/// Refresh the tray icon and tooltip for the paused state. Failures are
/// logged and never block the toggle.
fn update_tray_paused(tray: &TrayIcon, paused: bool) {
    let rgba = if paused {
        dimmed_rgba(include_bytes!("../icons/tray-32.rgba"))
    } else {
        include_bytes!("../icons/tray-32.rgba").to_vec()
    };
    match Icon::from_rgba(rgba, 32, 32) {
        Ok(icon) => {
            if let Err(error) = tray.set_icon(Some(icon)) {
                eprintln!("Writely: could not update the tray icon: {error}");
            }
            // set_icon drops the template flag on macOS, so restore it.
            #[cfg(target_os = "macos")]
            tray.set_icon_as_template(true);
        }
        Err(error) => {
            eprintln!("Writely: could not update the tray icon: {error}");
        }
    }
    let tooltip = if paused {
        "Writely (paused)"
    } else if settings::permission_message().is_some() {
        // Pausing overwrote the Accessibility tooltip; put it back rather
        // than plain "Writely" while permission is still missing.
        ACCESSIBILITY_TOOLTIP
    } else {
        "Writely"
    };
    if let Err(error) = tray.set_tooltip(Some(tooltip)) {
        eprintln!("Writely: could not update the tray tooltip: {error}");
    }
}

/// Index into a `ProviderMenuState` readiness array (Gemini, Together, Ollama, Local).
fn ready_index(provider: config::Provider) -> usize {
    match provider {
        config::Provider::Gemini => 0,
        config::Provider::Together => 1,
        config::Provider::Ollama => 2,
        config::Provider::Local => 3,
    }
}

/// The Provider submenu label: the display name when ready, otherwise
/// "{name} (not set up)".
fn provider_item_text(provider: config::Provider, ready: bool) -> String {
    if ready {
        provider.display_name().to_string()
    } else {
        format!("{} (not set up)", provider.display_name())
    }
}

/// The Provider submenu diff: for each provider (Gemini, Together, Ollama, Local),
/// the checked state and label to apply, each `None` when unchanged from
/// `previous`. A missing cache means everything is applied.
fn menu_updates(
    previous: Option<(config::Provider, [bool; 4])>,
    current: config::Provider,
    ready: [bool; 4],
) -> Vec<(config::Provider, Option<bool>, Option<String>)> {
    [
        config::Provider::Gemini,
        config::Provider::Together,
        config::Provider::Ollama,
        config::Provider::Local,
    ]
    .iter()
    .map(|provider| {
        let checked = current == *provider;
        let text = provider_item_text(*provider, ready[ready_index(*provider)]);
        match previous {
            Some((previous_current, previous_ready)) => {
                let checked_changed = checked != (previous_current == *provider);
                let was_text =
                    provider_item_text(*provider, previous_ready[ready_index(*provider)]);
                (
                    *provider,
                    checked_changed.then_some(checked),
                    (text != was_text).then_some(text),
                )
            }
            None => (*provider, Some(checked), Some(text)),
        }
    })
    .collect()
}

/// Requests a Provider submenu refresh off the main thread: bumps the id and
/// spawns a `writely-provider-menu` thread that loads the settings and keys,
/// then sends `UserEvent::ProviderMenuState`. A load error sends nothing so
/// the items stay unchanged.
fn request_provider_menu_refresh(proxy: &EventLoopProxy<UserEvent>, menu_refresh_id: &mut u64) {
    *menu_refresh_id = menu_refresh_id.wrapping_add(1);
    let id = *menu_refresh_id;
    let proxy = proxy.clone();
    let spawn = std::thread::Builder::new()
        .name("writely-provider-menu".to_string())
        .spawn(move || {
            let settings = match config::load_settings() {
                Ok(settings) => settings,
                Err(error) => {
                    eprintln!("Writely: {error}");
                    return;
                }
            };
            let providers = [
                config::Provider::Gemini,
                config::Provider::Together,
                config::Provider::Ollama,
                config::Provider::Local,
            ];
            let mut ready = [false; 4];
            for (index, provider) in providers.iter().enumerate() {
                ready[index] = if *provider == config::Provider::Local {
                    local::install::local_ready(&settings)
                } else {
                    let has_key = if !provider.needs_key() {
                        false
                    } else {
                        match settings::has_api_key_for(*provider) {
                            Ok(has_key) => has_key,
                            Err(error) => {
                                eprintln!("Writely: {error}");
                                false
                            }
                        }
                    };
                    config::provider_ready(*provider, &settings.model_for(*provider), has_key)
                };
            }
            if let Err(error) = proxy.send_event(UserEvent::ProviderMenuState {
                id,
                current: settings.provider,
                ready,
            }) {
                eprintln!("Writely: could not refresh the Provider menu: {error}");
            }
        });
    if let Err(error) = spawn {
        eprintln!("Writely: could not refresh the Provider menu: {error}");
    }
}

fn handle_permission_granted(
    proxy: &EventLoopProxy<UserEvent>,
    caret_watcher: &mut Option<watcher::Watcher>,
    tray_icon: &Option<TrayIcon>,
    paused: bool,
) {
    settings::set_permission_message(None);
    // While paused the watcher stays off; resuming starts it (if permission
    // allows) so monitoring never restarts behind the user's back.
    if !paused && caret_watcher.is_none() {
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

/// Whether the icon idle timer has fired: no deadline means nothing is due.
fn icon_ready(due: Option<Instant>, now: Instant) -> bool {
    matches!(due, Some(deadline) if now >= deadline)
}

/// Shows the W icon at the current caret, hiding it when there is no caret.
/// Errors are logged once per distinct message.
fn show_icon_at_caret(last_caret_error: &mut Option<String>) {
    match text_access::caret_info() {
        Ok(Some(info)) => {
            *last_caret_error = None;
            overlay::show_at(&info.caret_rect);
        }
        Ok(None) => {
            *last_caret_error = None;
            overlay::hide();
        }
        Err(error) => {
            overlay::hide();
            if last_caret_error.as_deref() != Some(error.as_str()) {
                eprintln!("Writely: {error}");
                *last_caret_error = Some(error);
            }
        }
    }
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
    settings::init(watcher_proxy.clone());
    check_window::init(watcher_proxy.clone());

    let mut tray_icon: Option<TrayIcon> = None;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let mut _hotkey_manager: Option<global_hotkey::GlobalHotKeyManager> = None;
    let mut settings_id = tray_icon::menu::MenuId::new("settings");
    let mut pause_id = tray_icon::menu::MenuId::new("pause");
    let mut pause_item: Option<MenuItem> = None;
    let mut provider_items: Vec<(config::Provider, CheckMenuItem)> = Vec::new();
    let mut menu_refresh_id: u64 = 0;
    let mut provider_menu_state: Option<(config::Provider, [bool; 4])> = None;
    let mut quit_id = tray_icon::menu::MenuId::new("quit");
    #[cfg(target_os = "linux")]
    let mut check_text_id = tray_icon::menu::MenuId::new("check-text");
    let mut caret_watcher: Option<watcher::Watcher> = None;
    let mut last_caret_error: Option<String> = None;
    let mut icon_due: Option<Instant> = None;
    // Popup flow state: bumped on every click and on hide so late Gemini
    // replies are dropped.
    let mut paused = false;
    let mut generation: u64 = 0;
    let mut checking: Option<Checking> = None;
    let mut pending: Option<Pending> = None;
    let mut composing: Option<Composing> = None;
    let mut last_anchor: Option<CaretRect> = None;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let mut copy_feedback_id: u64 = 0;
    let mut failed_check: Option<Checking> = None;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::NewEvents(StartCause::Init) => {
                local::lifecycle::app_started();
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
                let provider_submenu = Submenu::new("Provider", true);
                let gemini_item =
                    CheckMenuItem::with_id("provider-gemini", "Gemini", true, false, None);
                let together_item = CheckMenuItem::with_id(
                    "provider-together",
                    "Together AI",
                    true,
                    false,
                    None,
                );
                let ollama_item =
                    CheckMenuItem::with_id("provider-ollama", "Ollama", true, false, None);
                let local_item = CheckMenuItem::with_id(
                    "provider-local",
                    "On this computer",
                    true,
                    false,
                    None,
                );
                if let Err(error) = provider_submenu
                    .append_items(&[&gemini_item, &together_item, &ollama_item, &local_item])
                {
                    eprintln!("Writely: could not create the tray icon: {error}");
                    std::process::exit(1);
                }
                #[cfg(target_os = "linux")]
                let check_text_item =
                    MenuItem::with_id("check-text", "Check text…", true, None);
                // No monitoring on Wayland, so no pause item there either.
                let new_pause_item: Option<MenuItem> = if watcher::is_wayland_session() {
                    None
                } else {
                    Some(MenuItem::with_id(
                        "pause",
                        pause_item_text(false),
                        true,
                        None,
                    ))
                };
                settings_id = settings_item.id().clone();
                quit_id = quit_item.id().clone();
                if let Some(item) = &new_pause_item {
                    pause_id = item.id().clone();
                }
                #[cfg(target_os = "linux")]
                {
                    check_text_id = check_text_item.id().clone();
                }

                let separator = PredefinedMenuItem::separator();
                #[cfg(target_os = "linux")]
                let menu_result = match &new_pause_item {
                    Some(pause) => Menu::with_items(&[
                        pause,
                        &separator,
                        &check_text_item,
                        &provider_submenu,
                        &settings_item,
                        &quit_item,
                    ]),
                    None => Menu::with_items(&[
                        &check_text_item,
                        &provider_submenu,
                        &settings_item,
                        &quit_item,
                    ]),
                };
                #[cfg(not(target_os = "linux"))]
                let menu_result = match &new_pause_item {
                    Some(pause) => Menu::with_items(&[
                        pause,
                        &separator,
                        &provider_submenu,
                        &settings_item,
                        &quit_item,
                    ]),
                    None => Menu::with_items(&[&provider_submenu, &settings_item, &quit_item]),
                };
                pause_item = new_pause_item;
                provider_items = vec![
                    (config::Provider::Gemini, gemini_item),
                    (config::Provider::Together, together_item),
                    (config::Provider::Ollama, ollama_item),
                    (config::Provider::Local, local_item),
                ];
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
                request_provider_menu_refresh(&watcher_proxy, &mut menu_refresh_id);

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
                                if let Err(tooltip_error) =
                                    tray.set_tooltip(Some(ACCESSIBILITY_TOOLTIP))
                                {
                                    eprintln!(
                                        "Writely: could not update the tray tooltip: {tooltip_error}"
                                    );
                                }
                            }
                            spawn_permission_poll(watcher_proxy.clone());
                        }
                    }
                }
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                {
                    _hotkey_manager = register_hotkey(&watcher_proxy);
                }
            }
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                if icon_ready(icon_due, Instant::now()) {
                    icon_due = None;
                    if !paused {
                        show_icon_at_caret(&mut last_caret_error);
                    }
                }
            }
            Event::UserEvent(UserEvent::MenuEvent(event)) => {
                #[cfg(target_os = "linux")]
                if event.id == check_text_id {
                    if let Err(error) = check_window::show() {
                        eprintln!("Writely: {error}");
                    }
                }
                if pause_item.is_some() && event.id == pause_id {
                    paused = !paused;
                    if let Some(item) = pause_item.as_ref() {
                        item.set_text(pause_item_text(paused));
                    }
                    if paused {
                        popup::hide();
                        overlay::hide();
                        icon_due = None;
                        generation = generation.wrapping_add(1);
                        checking = None;
                        pending = None;
                        composing = None;
                        // Linux: the AT-SPI watcher thread has no way to stop
                        // (no Drop on WatcherInner), so it is kept and its
                        // events are ignored while paused; never start a
                        // second one on resume.
                        #[cfg(any(target_os = "macos", target_os = "windows"))]
                        {
                            caret_watcher = None;
                        }
                        eprintln!("Writely: paused.");
                    } else {
                        // On Linux the watcher was kept, so this only
                        // restarts it on macOS/Windows — and only with
                        // permission; otherwise the permission flow starts
                        // it later.
                        if caret_watcher.is_none()
                            && text_access::check_permission(false).is_ok()
                        {
                            caret_watcher = start_watcher(&watcher_proxy);
                        }
                        eprintln!("Writely: resumed.");
                    }
                    if let Some(tray) = tray_icon.as_ref() {
                        update_tray_paused(tray, paused);
                    }
                }
                if let Some(provider) = provider_items.iter().find_map(|(provider, item)| {
                    (event.id == *item.id()).then_some(*provider)
                }) {
                    // A reply from the old provider must not land after the
                    // switch: drop the shown popup state like the pause path.
                    popup::hide();
                    generation = generation.wrapping_add(1);
                    checking = None;
                    pending = None;
                    failed_check = None;
                    composing = None;
                    // A CheckMenuItem toggles itself on click; undo that
                    // instantly so the clicked item shows checked right away.
                    for (listed, item) in &provider_items {
                        item.set_checked(*listed == provider);
                    }
                    // Keep the cache in step with the instant check: a failed
                    // switch then differs from the refresh and gets reset.
                    if let Some((current, _)) = provider_menu_state.as_mut() {
                        *current = provider;
                    }
                    match config::switch_provider(provider) {
                        Err(error) => {
                            let detail = format!(
                                "could not switch to {}: {error}",
                                provider.display_name()
                            );
                            eprintln!("Writely: {detail}");
                            settings::set_provider_message(Some(detail));
                            request_provider_menu_refresh(
                                &watcher_proxy,
                                &mut menu_refresh_id,
                            );
                            if let Err(error) = settings::show_settings() {
                                eprintln!("Writely: {error}");
                            }
                        }
                        Ok(()) => {
                            settings::provider_switched(provider);
                            local::lifecycle::settings_changed();
                            // The cached readiness is what the menu label
                            // showed; with no cache yet treat it as ready.
                            let ready = provider_menu_state
                                .map(|(_, ready)| ready[ready_index(provider)])
                                .unwrap_or(true);
                            if ready {
                                eprintln!("Writely: switched to {}.", provider.display_name());
                            } else if let Err(error) = settings::show_settings() {
                                eprintln!("Writely: {error}");
                            }
                            request_provider_menu_refresh(
                                &watcher_proxy,
                                &mut menu_refresh_id,
                            );
                        }
                    }
                }
                if event.id == settings_id {
                    popup::hide();
                    // A stale permission message (e.g. granted after startup)
                    // is cleared here so Settings opens fresh. Skipped on
                    // Wayland: the watcher stays off and the Wayland message stays.
                    // Skipped while paused: the watcher must not restart.
                    if !paused
                        && !watcher::is_wayland_session()
                        && text_access::check_permission(false).is_ok()
                        && caret_watcher.is_none()
                    {
                        handle_permission_granted(
                            &watcher_proxy,
                            &mut caret_watcher,
                            &tray_icon,
                            paused,
                        );
                    }
                    if let Err(error) = settings::show_settings() {
                        eprintln!("Writely: {error}");
                    }
                } else if event.id == quit_id {
                    drop(tray_icon.take());
                    caret_watcher = None;
                    local::server::stop("Writely is quitting");
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UserEvent::CaretChanged) => {
                // While paused the icon stays hidden: ignore the event
                // (including one queued before the pause). The coalesced
                // flag is still drained so the next caret move after
                // resuming wakes the app again.
                if paused {
                    watcher::clear_pending();
                    return;
                }
                // Typing or moving focus hides the popup and drops any
                // in-flight suggestion — but not while the user types in
                // our own input box (its focus switch fires this too).
                if popup::is_visible()
                    && !popup::focus_handoff_recent()
                    && !popup::input_has_focus()
                {
                    popup::hide();
                    generation = generation.wrapping_add(1);
                    checking = None;
                    pending = None;
                    failed_check = None;
                    composing = None;
                }
                if let Some(watcher) = caret_watcher.as_mut() {
                    watcher.handle_caret_changed();
                }
                // The icon hides while typing and reappears after 1 s idle; a
                // new caret event pushes the deadline later. No caret query
                // here, so per-keystroke work shrinks.
                overlay::hide();
                icon_due = Some(Instant::now() + ICON_IDLE_DELAY);
            }
            Event::UserEvent(UserEvent::PopupDismissed) => {
                // From the Windows popup's dismiss controls: exactly the
                // popup-state part of CaretChanged, without the watcher
                // and icon part.
                popup::hide();
                generation = generation.wrapping_add(1);
                checking = None;
                pending = None;
                failed_check = None;
                composing = None;
            }
            Event::UserEvent(UserEvent::IconClicked) => {
                // While paused the icon is hidden; a queued click from
                // before the pause must not re-show anything.
                if paused {
                    return;
                }
                handle_icon_clicked(
                    &watcher_proxy,
                    &mut generation,
                    &mut checking,
                    &mut pending,
                    &mut failed_check,
                    &mut composing,
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
                    &mut failed_check,
                    &mut composing,
                    &mut last_anchor,
                    true,
                );
            }
            Event::UserEvent(UserEvent::ReplaceClicked) => {
                handle_replace_clicked(&mut pending, &mut last_anchor);
            }
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Event::UserEvent(UserEvent::CopyClicked) => {
                handle_copy_clicked(
                    &pending,
                    &watcher_proxy,
                    &mut copy_feedback_id,
                    &mut last_anchor,
                );
            }
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Event::UserEvent(UserEvent::CopyFeedbackEnded(id)) => {
                // A stale timer must not reset a newer checkmark.
                if id == copy_feedback_id {
                    popup::set_copy_feedback(false);
                }
            }
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Event::UserEvent(UserEvent::RetryClicked) => {
                handle_retry_clicked(
                    &watcher_proxy,
                    &mut generation,
                    &mut checking,
                    &mut pending,
                    &mut failed_check,
                    &mut last_anchor,
                );
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
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Event::UserEvent(UserEvent::InputSubmitted(request)) => {
                handle_input_submitted(
                    &watcher_proxy,
                    request,
                    generation,
                    &mut composing,
                    &mut pending,
                    &mut last_anchor,
                );
            }
            Event::UserEvent(UserEvent::PermissionGranted) => {
                handle_permission_granted(&watcher_proxy, &mut caret_watcher, &tray_icon, paused);
            }
            Event::UserEvent(UserEvent::SettingsChanged) => {
                local::lifecycle::settings_changed();
                request_provider_menu_refresh(&watcher_proxy, &mut menu_refresh_id);
            }
            Event::UserEvent(UserEvent::ProviderMenuState { id, current, ready }) => {
                // A stale reply must not overwrite a newer one. Memory updates
                // only: no file or keychain access here.
                if id == menu_refresh_id {
                    for (provider, checked, text) in
                        menu_updates(provider_menu_state, current, ready)
                    {
                        if let Some((_, item)) =
                            provider_items.iter().find(|(listed, _)| *listed == provider)
                        {
                            if let Some(checked) = checked {
                                item.set_checked(checked);
                            }
                            if let Some(text) = text {
                                item.set_text(text);
                            }
                        }
                    }
                    provider_menu_state = Some((current, ready));
                }
            }
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Event::UserEvent(UserEvent::CopiedText {
                generation: event_generation,
                result,
                pid,
                from_hotkey,
                from_terminal,
            }) => {
                handle_copied_text(
                    &watcher_proxy,
                    generation,
                    event_generation,
                    result,
                    pid,
                    from_hotkey,
                    from_terminal,
                    &mut checking,
                    &mut pending,
                    &mut last_anchor,
                );
            }
            Event::UserEvent(UserEvent::Partial {
                generation: event_generation,
                kind,
                text,
            }) => {
                handle_partial(
                    generation,
                    event_generation,
                    kind,
                    text,
                    &checking,
                    &pending,
                    &composing,
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
                    &mut failed_check,
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
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Event::UserEvent(UserEvent::Composed {
                generation: event_generation,
                result,
            }) => {
                handle_composed(
                    generation,
                    event_generation,
                    result,
                    &mut composing,
                    &mut pending,
                    &mut last_anchor,
                );
            }
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Event::UserEvent(UserEvent::Changed {
                generation: event_generation,
                result,
            }) => {
                handle_changed(
                    generation,
                    event_generation,
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
        // Wake up when the icon idle timer fires. Exit wins: quitting must
        // not schedule another wake-up.
        if let Some(due) = icon_due {
            if !matches!(*control_flow, ControlFlow::Exit) {
                *control_flow = ControlFlow::WaitUntil(due);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        compose_range, custom_source_text, dimmed_rgba, icon_ready, menu_updates, partial_target,
        pause_item_text, provider_item_text, ready_index, rewrite_retry_style, selected_text,
        style_index, PartialKind, PartialState, Pending,
    };
    use crate::config::Provider;
    use crate::popup::PopupTab;
    use crate::providers::RewriteStyle;
    use std::time::{Duration, Instant};

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
            copy_only: false,
            draft: false,
            custom: None,
            custom_source: String::new(),
            custom_request: String::new(),
            custom_in_flight: false,
        }
    }

    fn live_partial_state() -> PartialState {
        PartialState {
            checking: true,
            pending: true,
            rewrite_selected: true,
            rewrite_cached: false,
            rewrite_in_flight: true,
            composing_in_flight: true,
            change_selected: true,
            change_in_flight: true,
            text_check_in_flight: true,
        }
    }

    #[test]
    fn partial_target_accepts_each_kind_in_its_live_state() {
        let style = RewriteStyle::ALL[0];
        for kind in [
            PartialKind::Check,
            PartialKind::Rewrite(style),
            PartialKind::Compose,
            PartialKind::Change,
            PartialKind::TextCheck,
        ] {
            assert!(
                partial_target(kind, 7, 7, true, live_partial_state()),
                "{kind:?}"
            );
        }
        // The Check text window is not a popup, so popup visibility is
        // irrelevant for it.
        assert!(partial_target(
            PartialKind::TextCheck,
            7,
            7,
            false,
            live_partial_state()
        ));
    }

    #[test]
    fn partial_target_drops_stale_hidden_and_finished() {
        let style = RewriteStyle::ALL[0];
        // Stale generation and a hidden popup drop every popup kind.
        assert!(!partial_target(
            PartialKind::Check,
            6,
            7,
            true,
            live_partial_state()
        ));
        assert!(!partial_target(
            PartialKind::TextCheck,
            6,
            7,
            true,
            live_partial_state()
        ));
        assert!(!partial_target(
            PartialKind::Rewrite(style),
            7,
            7,
            false,
            live_partial_state()
        ));
        // Check after the final result: `checking` was taken.
        assert!(!partial_target(
            PartialKind::Check,
            7,
            7,
            true,
            PartialState {
                checking: false,
                ..live_partial_state()
            }
        ));
        // Rewrite for an unselected style, an already cached result, and one
        // whose final event cleared the in-flight flag.
        for state in [
            PartialState {
                rewrite_selected: false,
                ..live_partial_state()
            },
            PartialState {
                rewrite_cached: true,
                ..live_partial_state()
            },
            PartialState {
                rewrite_in_flight: false,
                ..live_partial_state()
            },
            PartialState {
                pending: false,
                ..live_partial_state()
            },
        ] {
            assert!(!partial_target(
                PartialKind::Rewrite(style),
                7,
                7,
                true,
                state
            ));
        }
        // Compose and change drop once their in-flight state is gone; a
        // change partial never shows on another tab.
        assert!(!partial_target(
            PartialKind::Compose,
            7,
            7,
            true,
            PartialState {
                composing_in_flight: false,
                ..live_partial_state()
            }
        ));
        assert!(!partial_target(
            PartialKind::Change,
            7,
            7,
            true,
            PartialState {
                change_in_flight: false,
                ..live_partial_state()
            }
        ));
        assert!(!partial_target(
            PartialKind::Change,
            7,
            7,
            true,
            PartialState {
                change_selected: false,
                ..live_partial_state()
            }
        ));
        assert!(!partial_target(
            PartialKind::TextCheck,
            7,
            7,
            true,
            PartialState {
                text_check_in_flight: false,
                ..live_partial_state()
            }
        ));
    }

    #[test]
    fn pause_item_text_toggles() {
        assert_eq!(pause_item_text(false), "Pause Writely");
        assert_eq!(pause_item_text(true), "Resume Writely");
    }

    #[test]
    fn dimmed_rgba_scales_alpha_only() {
        let input = [10, 20, 30, 255, 40, 50, 60, 100, 70, 80, 90, 1];
        let dimmed = dimmed_rgba(&input);
        assert_eq!(dimmed.len(), input.len());
        assert_eq!(&dimmed[0..3], &[10, 20, 30]);
        assert_eq!(dimmed[3], 102);
        assert_eq!(&dimmed[4..7], &[40, 50, 60]);
        assert_eq!(dimmed[7], 40);
        assert_eq!(&dimmed[8..11], &[70, 80, 90]);
        assert_eq!(dimmed[11], 0);
    }

    #[test]
    fn dimmed_rgba_tray_icon() {
        let rgba = include_bytes!("../icons/tray-32.rgba");
        assert_eq!(rgba.len(), 32 * 32 * 4);
        let dimmed = dimmed_rgba(rgba);
        assert_eq!(dimmed.len(), 32 * 32 * 4);
        for (index, byte) in dimmed.iter().enumerate() {
            if index % 4 == 3 {
                assert!(*byte <= 102, "alpha {byte} above 102 at {index}");
            } else {
                assert_eq!(*byte, rgba[index]);
            }
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

    #[test]
    fn selected_text_custom_tab() {
        // Uncached: nothing to insert.
        let mut pending = test_pending();
        pending.selected = PopupTab::Custom;
        assert_eq!(selected_text(&pending), None);
        // Cached result: insert it.
        pending.custom = Some("She goes to school, you know!".to_string());
        assert_eq!(
            selected_text(&pending),
            Some("She goes to school, you know!")
        );
        // Cached but unchanged: nothing to insert.
        pending.custom = Some("She go to school.".to_string());
        assert_eq!(selected_text(&pending), None);
    }

    #[test]
    fn custom_source_prefers_shown_text() {
        // Correction shown: refine the correction.
        let pending = test_pending();
        assert_eq!(custom_source_text(&pending), "She goes to school.");
        // Looks-good: refine the original.
        let mut looks_good = test_pending();
        looks_good.correction = None;
        assert_eq!(custom_source_text(&looks_good), "She go to school.");
        // After a custom result: refine that result.
        let mut custom = test_pending();
        custom.selected = PopupTab::Custom;
        custom.custom = Some("She really goes to school.".to_string());
        assert_eq!(custom_source_text(&custom), "She really goes to school.");
    }

    #[test]
    fn rewrite_retry_style_correct_tab_never_retries() {
        let pending = test_pending();
        assert_eq!(pending.selected, PopupTab::Correct);
        assert_eq!(rewrite_retry_style(&pending), None);
    }

    #[test]
    fn rewrite_retry_style_rewrite_tab() {
        // Uncached and not in flight: retry re-requests the style.
        let mut pending = test_pending();
        pending.selected = PopupTab::Rewrite(RewriteStyle::Friendly);
        assert_eq!(rewrite_retry_style(&pending), Some(RewriteStyle::Friendly));
        // Cached: nothing to re-request.
        pending.rewrites[style_index(RewriteStyle::Friendly)] =
            Some("She goes to school, you know!".to_string());
        assert_eq!(rewrite_retry_style(&pending), None);
        // In flight: the running request wins.
        pending.rewrites[style_index(RewriteStyle::Friendly)] = None;
        pending.rewrite_in_flight[style_index(RewriteStyle::Friendly)] = true;
        assert_eq!(rewrite_retry_style(&pending), None);
    }

    #[test]
    fn ready_index_follows_provider_order() {
        assert_eq!(ready_index(Provider::Gemini), 0);
        assert_eq!(ready_index(Provider::Together), 1);
        assert_eq!(ready_index(Provider::Ollama), 2);
        assert_eq!(ready_index(Provider::Local), 3);
    }

    #[test]
    fn provider_item_text_marks_not_ready() {
        assert_eq!(provider_item_text(Provider::Gemini, true), "Gemini");
        assert_eq!(provider_item_text(Provider::Together, true), "Together AI");
        assert_eq!(provider_item_text(Provider::Ollama, true), "Ollama");
        assert_eq!(
            provider_item_text(Provider::Local, true),
            "On this computer"
        );
        assert_eq!(
            provider_item_text(Provider::Gemini, false),
            "Gemini (not set up)"
        );
        assert_eq!(
            provider_item_text(Provider::Together, false),
            "Together AI (not set up)"
        );
        assert_eq!(
            provider_item_text(Provider::Ollama, false),
            "Ollama (not set up)"
        );
        assert_eq!(
            provider_item_text(Provider::Local, false),
            "On this computer (not set up)"
        );
    }

    #[test]
    fn menu_updates_no_cache_applies_everything() {
        let updates = menu_updates(None, Provider::Gemini, [true, false, true, false]);
        assert_eq!(
            updates,
            vec![
                (Provider::Gemini, Some(true), Some("Gemini".to_string())),
                (
                    Provider::Together,
                    Some(false),
                    Some("Together AI (not set up)".to_string())
                ),
                (Provider::Ollama, Some(false), Some("Ollama".to_string())),
                (
                    Provider::Local,
                    Some(false),
                    Some("On this computer (not set up)".to_string())
                ),
            ]
        );
    }

    #[test]
    fn menu_updates_unchanged_applies_nothing() {
        let updates = menu_updates(
            Some((Provider::Gemini, [true, true, true, true])),
            Provider::Gemini,
            [true, true, true, true],
        );
        assert!(updates
            .iter()
            .all(|(_, checked, text)| checked.is_none() && text.is_none()));
    }

    #[test]
    fn menu_updates_failed_switch_resets_checkmark() {
        // The click set the cache to Together, then the switch failed, so
        // the refresh still says Gemini: Gemini is re-checked and Together
        // is unchecked, with labels untouched.
        let updates = menu_updates(
            Some((Provider::Together, [true, true, true, true])),
            Provider::Gemini,
            [true, true, true, true],
        );
        assert_eq!(updates[0].1, Some(true));
        assert_eq!(updates[1].1, Some(false));
        assert_eq!(updates[2].1, None);
        assert_eq!(updates[3].1, None);
        assert!(updates.iter().all(|(_, _, text)| text.is_none()));
    }

    #[test]
    fn menu_updates_readiness_change_updates_label_only() {
        let updates = menu_updates(
            Some((Provider::Gemini, [true, true, true, true])),
            Provider::Gemini,
            [true, false, true, true],
        );
        assert!(updates.iter().all(|(_, checked, _)| checked.is_none()));
        assert_eq!(updates[1].2.as_deref(), Some("Together AI (not set up)"));
        assert!(updates[0].2.is_none() && updates[2].2.is_none() && updates[3].2.is_none());
    }

    #[test]
    fn compose_range_targets_caret_or_whole_blank_field() {
        assert_eq!(compose_range(true, 7, 20), 7..7);
        assert_eq!(compose_range(false, 0, 0), 0..0);
        assert_eq!(compose_range(false, 0, 12), 0..12);
    }

    #[test]
    fn icon_ready_fires_at_or_after_due() {
        let now = Instant::now();
        assert!(!icon_ready(None, now));
        assert!(!icon_ready(Some(now + Duration::from_millis(100)), now));
        assert!(icon_ready(Some(now), now));
        assert!(icon_ready(Some(now - Duration::from_millis(1)), now));
    }
}
