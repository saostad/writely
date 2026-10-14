//! Copy-and-paste fallback for apps whose text Accessibility doesn't expose
//! (e.g. Warp), or whose field is too large for the whole-field check.
//! It never runs for a secure (password) field — the caller checks that first.
//!
//! Reading posts ⌘C, waits briefly for the pasteboard to change, reads the
//! plain string, then restores the saved pasteboard. Replacing writes the
//! suggestion (marked transient + concealed so clipboard managers skip it),
//! posts ⌘V, waits, then restores. The user's text is never written to logs.

use std::collections::HashSet;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardItem, NSPasteboardType, NSPasteboardTypeString, NSPasteboardWriting,
};
use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFNumber, CFNumberType, CFRetained, CFString,
};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGKeyCode,
};
use objc2_foundation::{NSArray, NSData, NSInteger, NSString};

/// Key codes for C and V (used with the Command flag for copy/paste).
const KEY_C: CGKeyCode = 8;
const KEY_V: CGKeyCode = 9;
/// Modifier keycodes released before a simulated copy/paste: the hotkey's
/// modifiers (⌃⌥) may still be held when the worker runs.
const KEY_CONTROL: CGKeyCode = 59;
const KEY_OPTION: CGKeyCode = 58;
const KEY_SHIFT: CGKeyCode = 56;

/// How long to wait for the pasteboard to change after copying: 50 ms, then
/// poll `changeCount` every 5 ms for up to 2 s (WritingTools timing; slow
/// apps need longer than the old 400 ms).
const COPY_INITIAL_WAIT_MS: u64 = 50;
const COPY_POLL_MS: u64 = 5;
const COPY_POLLS: u32 = 400;

/// Apps whose AX text is unreliable (Easydict's list): the AX-selection
/// layer is skipped for them and Writely goes straight to copy. `.*` is a
/// prefix wildcard (e.g. `com.jetbrains.*` matches IntelliJ, PyCharm, …).
pub const UNRELIABLE_AX_BUNDLES: &[&str] = &[
    "com.microsoft.VSCode",
    "com.jetbrains.*",
    "com.microsoft.Word",
    "com.microsoft.Powerpoint",
    "com.apple.iWork.Pages",
    "com.apple.iWork.Keynote",
    "com.apple.iWork.Numbers",
    "com.apple.freeform",
    "org.mozilla.firefox",
    "com.openai.chat",
    "com.sublimetext.4",
    "com.googlecode.iterm2",
    "com.apple.dt.Xcode",
    "org.zotero.zotero",
    "com.readdle.PDFExpert-Mac",
];

/// Whether `bundle_id` is on the unreliable-AX list. Pure so it can be tested.
pub fn is_unreliable_bundle(bundle_id: &str) -> bool {
    for pattern in UNRELIABLE_AX_BUNDLES {
        if let Some(prefix) = pattern.strip_suffix(".*") {
            if bundle_id == prefix || bundle_id.starts_with(&format!("{prefix}.")) {
                return true;
            }
        } else if bundle_id == *pattern {
            return true;
        }
    }
    false
}

/// The frontmost app's bundle id, when readable.
fn frontmost_bundle_id() -> Option<String> {
    let app = objc2_app_kit::NSWorkspace::sharedWorkspace().frontmostApplication()?;
    app.bundleIdentifier().map(|id| id.to_string())
}

/// Layer 1: the AX `AXSelectedText` of the focused element, when non-empty.
/// Failures mean "try the next layer", so they are `Ok(None)`, never errors.
fn ax_selected_text() -> Option<String> {
    // SAFETY: creating the system-wide element is a plain FFI call with no arguments.
    let system = unsafe { objc2_application_services::AXUIElement::new_system_wide() };
    crate::text_access::macos_helpers::set_messaging_timeout(&system);
    let value = crate::text_access::macos_helpers::copy_attr(&system, "AXFocusedUIElement").ok()?;
    let ptr = CFRetained::as_ptr(&value).cast::<objc2_application_services::AXUIElement>();
    // SAFETY: AXFocusedUIElement yields an AXUIElement valid while value is alive.
    let element: &objc2_application_services::AXUIElement = unsafe { ptr.as_ref() };
    crate::text_access::macos_helpers::set_messaging_timeout(element);
    let selected =
        crate::text_access::macos_helpers::copy_attr(element, "AXSelectedText").ok()?;
    let text = selected.downcast_ref::<CFString>()?;
    let text = text.to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// Whether the Copy menu item matches: Cmd+C with no extra modifiers.
pub(crate) fn is_copy_menu_item(cmd_char: Option<&str>, cmd_modifiers: Option<i64>) -> bool {
    let is_c = matches!(cmd_char, Some("C") | Some("c"));
    is_c && cmd_modifiers == Some(0)
}

fn read_cmd_char(
    element: &objc2_application_services::AXUIElement,
) -> Option<String> {
    let value =
        crate::text_access::macos_helpers::copy_attr(element, "AXMenuItemCmdChar").ok()?;
    value
        .downcast_ref::<CFString>()
        .map(|text| text.to_string())
}

fn read_cmd_modifiers(
    element: &objc2_application_services::AXUIElement,
) -> Option<i64> {
    let value =
        crate::text_access::macos_helpers::copy_attr(element, "AXMenuItemCmdModifiers").ok()?;
    let number = value.downcast::<CFNumber>().ok()?;
    let mut out: i64 = 0;
    let ptr = NonNull::new((&mut out as *mut i64).cast::<c_void>());
    let mut ptr = ptr?;
    // SAFETY: ptr points at a writable i64 for a signed-integer conversion.
    if unsafe { number.value(CFNumberType::SInt64Type, ptr.as_mut()) } {
        Some(out)
    } else {
        None
    }
}

fn read_enabled(element: &objc2_application_services::AXUIElement) -> Option<bool> {
    let value = crate::text_access::macos_helpers::copy_attr(element, "AXEnabled").ok()?;
    value
        .downcast_ref::<CFBoolean>()
        .map(|flag| flag.as_bool())
}

/// Depth-first search for the enabled Copy menu item (Cmd+C, no extra
/// modifiers) under `element`, pressing it when found (while the AX arrays
/// are alive). Returns `Ok(true)` when Copy was pressed.
fn press_copy_in_children(element: &AXUIElement, depth: u8) -> Result<bool, String> {
    if depth == 0 {
        return Ok(false);
    }
    let Ok(value) = crate::text_access::macos_helpers::copy_attr(element, "AXChildren") else {
        return Ok(false);
    };
    let Ok(array) = value.downcast::<CFArray>() else {
        return Ok(false);
    };
    let count = array.count();
    for index in 0..count {
        // SAFETY: index is in bounds and the array isn't mutated while the
        // borrowed child is used below (including the recursion).
        let raw = unsafe { array.value_at_index(index) };
        if raw.is_null() {
            continue;
        }
        // SAFETY: AXChildren yields AXUIElements valid while the array is alive.
        let child: &AXUIElement = unsafe { &*(raw as *const AXUIElement) };
        let cmd_char = read_cmd_char(child);
        if cmd_char.is_some() {
            let modifiers = read_cmd_modifiers(child);
            let enabled = read_enabled(child).unwrap_or(true);
            if enabled && is_copy_menu_item(cmd_char.as_deref(), modifiers) {
                let action = CFString::from_str("AXPress");
                // SAFETY: child and action are valid AX objects of the expected kinds.
                let error = unsafe { child.perform_action(&action) };
                if error != AXError::Success {
                    return Err(format!(
                        "Could not press the Copy menu item (AXError {}).",
                        error.0
                    ));
                }
                return Ok(true);
            }
        }
        if press_copy_in_children(child, depth - 1)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Layer 2: press the frontmost app's menu-bar Edit › Copy via AX (no key
/// simulation). Returns `Ok(true)` when Copy was pressed.
fn menu_bar_copy() -> Result<bool, String> {
    use objc2_application_services::AXUIElement;
    let pid = std::process::id();
    let frontmost = objc2_app_kit::NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .map(|app| app.processIdentifier())
        .unwrap_or(0);
    if frontmost <= 0 || frontmost as u32 == pid {
        return Ok(false);
    }
    // SAFETY: the pid comes from NSWorkspace and identifies a live process.
    let app = unsafe { AXUIElement::new_application(frontmost) };
    crate::text_access::macos_helpers::set_messaging_timeout(&app);
    let menubar_value = match crate::text_access::macos_helpers::copy_attr(&app, "AXMenuBar") {
        Ok(value) => value,
        Err(error) => {
            return Err(format!("Could not read the menu bar (AXError {}).", error.0));
        }
    };
    let ptr = CFRetained::as_ptr(&menubar_value).cast::<AXUIElement>();
    // SAFETY: AXMenuBar yields an AXUIElement valid while menubar_value is alive.
    let menubar: &AXUIElement = unsafe { ptr.as_ref() };
    press_copy_in_children(menubar, 8)
}

/// Posts one key event (down or up) with the given flags.
fn post_key(keycode: CGKeyCode, key_down: bool, flags: CGEventFlags) -> Result<(), String> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .ok_or_else(|| "Could not post the key event (no event source).".to_string())?;
    let event = CGEvent::new_keyboard_event(Some(&source), keycode, key_down)
        .ok_or_else(|| "Could not post the key event (CGEventCreateKeyboardEvent failed).".to_string())?;
    // SAFETY: event is a valid CGEvent and the flags are a plain bitmask.
    CGEvent::set_flags(Some(&event), flags);
    // SAFETY: event is a valid CGEvent posted to the session tap.
    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&event));
    Ok(())
}

/// Releases Control/Option/Shift (Pot/Windows releases held modifiers first):
/// the hotkey's modifiers may still be held when the copy runs.
pub fn release_modifiers() {
    for keycode in [KEY_CONTROL, KEY_OPTION, KEY_SHIFT] {
        if let Err(error) = post_key(keycode, false, CGEventFlags::empty()) {
            eprintln!("Writely: could not release a modifier key: {error}");
        }
    }
}

/// Waits for the pasteboard to change: 50 ms, then every 5 ms for up to 2 s.
/// Runs in a worker thread (Item 4), never on the main thread.
fn wait_for_pasteboard_change(pasteboard: &NSPasteboard, before: NSInteger) -> bool {
    std::thread::sleep(Duration::from_millis(COPY_INITIAL_WAIT_MS));
    for _ in 0..COPY_POLLS {
        if pasteboard.changeCount() != before {
            return true;
        }
        std::thread::sleep(Duration::from_millis(COPY_POLL_MS));
    }
    pasteboard.changeCount() != before
}

/// Restores the saved pasteboard only when nothing else changed it since our
/// copy (WritingTools `restoreIfUnchanged`); otherwise the user's new
/// clipboard content is left alone.
fn restore_if_unchanged(
    pasteboard: &NSPasteboard,
    saved: &[SavedItem],
    our_change: NSInteger,
) {
    if pasteboard.changeCount() == our_change {
        restore_clipboard(pasteboard, saved);
    } else {
        eprintln!("Writely: clipboard changed by another app; not restored.");
    }
}

/// Reads the plain string from the pasteboard.
fn pasteboard_string(pasteboard: &NSPasteboard) -> Option<String> {
    // SAFETY: NSPasteboardTypeString is a framework-provided constant.
    let string_type: &NSPasteboardType = unsafe { NSPasteboardTypeString };
    pasteboard
        .stringForType(string_type)
        .map(|value| value.to_string())
}

fn log_layer_once(pid: u32, bundle: &Option<String>, layer: &str) {
    let what = match bundle {
        Some(bundle) => format!("checked the text via {layer} for {bundle} (pasteboard restored)."),
        None => format!("checked the text via {layer} (pasteboard restored)."),
    };
    log_clipboard_once(pid, &what);
}

/// Layered selection reading (Easydict/Pot rule): AX `AXSelectedText` first
/// (unless the frontmost app is on the unreliable list), then the menu-bar
/// Copy item via AX, then simulated ⌘C with held modifiers released first.
/// Copy layers snapshot the clipboard, wait 50 ms then poll every 5 ms for up
/// to 2 s, and restore only when nothing else changed it. The text itself is
/// never logged; only which layer worked (once per pid).
pub fn layered_copy_text(pid: u32) -> Result<CopyResult, String> {
    let bundle = frontmost_bundle_id();
    let unreliable = bundle.as_deref().is_some_and(is_unreliable_bundle);
    // Layer 1: AX selection (no clipboard involved).
    if !unreliable {
        if let Some(text) = ax_selected_text() {
            log_layer_once(pid, &bundle, "AX selection");
            return Ok(CopyResult::Text(text));
        }
    }
    // Layers 2 and 3 copy through the clipboard.
    let pasteboard = NSPasteboard::generalPasteboard();
    // Layer 2: menu-bar Copy needs no key simulation.
    if let Some(text) = copy_via_menu(&pasteboard)? {
        log_layer_once(pid, &bundle, "menu-bar Copy");
        return Ok(CopyResult::Text(text));
    }
    match copy_via_simulated_key(&pasteboard) {
        Ok(Some(text)) => {
            log_layer_once(pid, &bundle, "simulated ⌘C");
            Ok(CopyResult::Text(text))
        }
        Ok(None) => Ok(CopyResult::NothingNew),
        Err(error) => Err(error),
    }
}

/// Menu-bar Copy layer with its clipboard handling: snapshot, press, wait,
/// read, restore-if-unchanged. `Ok(None)` means "no new text copied".
fn copy_via_menu(pasteboard: &NSPasteboard) -> Result<Option<String>, String> {
    let saved = save_clipboard(pasteboard);
    let pressed = menu_bar_copy();
    // Snapshot before the copy: restore the user's content when the press
    // produced nothing. (The press ran against the live board; re-snapshot
    // semantics are: `saved` predates it.)
    match pressed {
        Ok(true) => {
            let before = pasteboard.changeCount();
            // The press already ran; poll from now (apps fill the board
            // within milliseconds of the press).
            let changed = wait_for_pasteboard_change(pasteboard, before);
            let text = pasteboard_string(pasteboard);
            let our_change = pasteboard.changeCount();
            restore_if_unchanged(pasteboard, &saved, our_change);
            match (changed, text) {
                (true, Some(copied)) if !copied.is_empty() => Ok(Some(copied)),
                _ => Ok(None),
            }
        }
        Ok(false) => {
            restore_clipboard(pasteboard, &saved);
            Ok(None)
        }
        Err(error) => {
            restore_clipboard(pasteboard, &saved);
            eprintln!("Writely: menu-bar Copy unavailable: {error}");
            Ok(None)
        }
    }
}

/// Simulated ⌘C layer with its clipboard handling. `Ok(None)` means "no new
/// text copied".
fn copy_via_simulated_key(pasteboard: &NSPasteboard) -> Result<Option<String>, String> {
    let saved = save_clipboard(pasteboard);
    let before = pasteboard.changeCount();
    release_modifiers();
    if let Err(error) = post_command_key(KEY_C) {
        restore_clipboard(pasteboard, &saved);
        eprintln!("Writely: could not copy the selected text: {error}");
        return Err(format!("Could not copy the selected text: {error}"));
    }
    let changed = wait_for_pasteboard_change(pasteboard, before);
    let text = pasteboard_string(pasteboard);
    let our_change = pasteboard.changeCount();
    restore_if_unchanged(pasteboard, &saved, our_change);
    match (changed, text) {
        (true, Some(copied)) if !copied.is_empty() => Ok(Some(copied)),
        _ => Ok(None),
    }
}



/// How long to wait after posting ⌘V before restoring the pasteboard.
const PASTE_WAIT_MS: u64 = 300;

#[derive(Debug)]
pub enum CopyResult {
    /// The newly copied text.
    Text(String),
    /// Nothing new was copied (or it was empty): the user should select text.
    NothingNew,
}

/// One saved pasteboard item: every type with its data.
struct SavedItem {
    entries: Vec<(Retained<NSPasteboardType>, Retained<NSData>)>,
}

/// Pids already told which clipboard path was used, so each tier logs once
/// per pid instead of on every click. The text itself is never logged.
static CLIPBOARD_LOGGED: LazyLock<Mutex<HashSet<u32>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn log_clipboard_once(pid: u32, what: &str) {
    let mut logged = CLIPBOARD_LOGGED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if logged.insert(pid) {
        eprintln!("Writely: {what}");
    }
}

fn save_clipboard(pasteboard: &NSPasteboard) -> Vec<SavedItem> {
    let mut saved = Vec::new();
    let Some(items) = pasteboard.pasteboardItems() else {
        return saved;
    };
    for index in 0..items.count() {
        let item = items.objectAtIndex(index);
        let types = item.types();
        let mut entries = Vec::new();
        for type_index in 0..types.count() {
            let data_type = types.objectAtIndex(type_index);
            if let Some(data) = item.dataForType(&data_type) {
                entries.push((data_type, data));
            }
        }
        saved.push(SavedItem { entries });
    }
    saved
}

fn restore_clipboard(pasteboard: &NSPasteboard, saved: &[SavedItem]) {
    pasteboard.clearContents();
    if saved.is_empty() {
        return;
    }
    let mut objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = Vec::new();
    for item in saved {
        let fresh = NSPasteboardItem::new();
        for (data_type, data) in &item.entries {
            fresh.setData_forType(data, data_type);
        }
        objects.push(ProtocolObject::from_retained(fresh));
    }
    let refs: Vec<&ProtocolObject<dyn NSPasteboardWriting>> =
        objects.iter().map(|object| &**object).collect();
    let array = NSArray::from_slice(&refs);
    if !pasteboard.writeObjects(&array) {
        eprintln!("Writely: could not restore the clipboard after using it.");
    }
}

/// Posts a Command+key keystroke (down and up) to the frontmost app.
fn post_command_key(keycode: CGKeyCode) -> Result<(), String> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .ok_or_else(|| "Could not post the key event (no event source).".to_string())?;
    for key_down in [true, false] {
        let event =
            CGEvent::new_keyboard_event(Some(&source), keycode, key_down).ok_or_else(|| {
                "Could not post the key event (CGEventCreateKeyboardEvent failed).".to_string()
            })?;
        // SAFETY: event is a valid CGEvent and the flags are a plain bitmask.
        CGEvent::set_flags(Some(&event), CGEventFlags::MaskCommand);
        // SAFETY: event is a valid CGEvent posted to the session tap.
        CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&event));
    }
    Ok(())
}

/// Copies the frontmost app's current selection via ⌘C and returns it.
/// The pasteboard is saved beforehand and restored afterwards either way.
///
/// Superseded by `layered_copy_text` (Item 4); kept until Item 6 removes it.
#[allow(dead_code)]
pub fn copy_selected_text(pid: u32) -> Result<CopyResult, String> {
    let pasteboard = NSPasteboard::generalPasteboard();
    let saved = save_clipboard(&pasteboard);
    let before = pasteboard.changeCount();
    let post_result = post_command_key(KEY_C);
    if let Err(error) = post_result {
        restore_clipboard(&pasteboard, &saved);
        eprintln!("Writely: could not copy the selected text: {error}");
        return Err(format!("Could not copy the selected text: {error}"));
    }
    let mut changed = false;
    for _ in 0..COPY_POLLS {
        std::thread::sleep(Duration::from_millis(COPY_POLL_MS));
        if pasteboard.changeCount() != before {
            changed = true;
            break;
        }
    }
    // SAFETY: NSPasteboardTypeString is a framework-provided constant.
    let string_type: &NSPasteboardType = unsafe { NSPasteboardTypeString };
    let text = pasteboard
        .stringForType(string_type)
        .map(|value| value.to_string());
    restore_clipboard(&pasteboard, &saved);
    log_clipboard_once(
        pid,
        "checked the text via copy-and-paste (pasteboard restored).",
    );
    match (changed, text) {
        (true, Some(copied)) if !copied.is_empty() => Ok(CopyResult::Text(copied)),
        _ => Ok(CopyResult::NothingNew),
    }
}

/// Pastes `suggestion` into the frontmost app via ⌘V, then restores the
/// saved pasteboard. The suggestion is marked transient + concealed so
/// clipboard managers skip it.
pub fn paste_suggestion(pid: u32, suggestion: &str) -> Result<(), String> {
    let pasteboard = NSPasteboard::generalPasteboard();
    let saved = save_clipboard(&pasteboard);
    pasteboard.clearContents();
    // SAFETY: NSPasteboardTypeString is a framework-provided constant.
    let string_type: &NSPasteboardType = unsafe { NSPasteboardTypeString };
    let transient = NSString::from_str("org.nspasteboard.TransientType");
    let concealed = NSString::from_str("org.nspasteboard.ConcealedType");
    let declared: &[&NSPasteboardType] = &[string_type, &transient, &concealed];
    let types = NSArray::from_slice(declared);
    // SAFETY: types holds valid pasteboard type strings and there is no owner.
    let _ = unsafe { pasteboard.declareTypes_owner(&types, None) };
    let string = NSString::from_str(suggestion);
    if !pasteboard.setString_forType(&string, string_type) {
        restore_clipboard(&pasteboard, &saved);
        return Err("Could not write the suggestion to the clipboard.".to_string());
    }
    // Mark the item so clipboard managers skip it; failures here are not
    // fatal for the paste itself.
    pasteboard.setString_forType(&string, &transient);
    pasteboard.setString_forType(&string, &concealed);
    if let Err(error) = post_command_key(KEY_V) {
        restore_clipboard(&pasteboard, &saved);
        eprintln!("Writely: could not paste the suggestion: {error}");
        return Err(format!("Could not paste the suggestion: {error}"));
    }
    std::thread::sleep(Duration::from_millis(PASTE_WAIT_MS));
    restore_clipboard(&pasteboard, &saved);
    log_clipboard_once(
        pid,
        "replaced the text via copy-and-paste (pasteboard restored).",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{is_copy_menu_item, is_unreliable_bundle};

    #[test]
    fn unreliable_list_skips_ax_for_listed_apps() {
        assert!(is_unreliable_bundle("com.microsoft.VSCode"));
        assert!(is_unreliable_bundle("org.mozilla.firefox"));
        assert!(is_unreliable_bundle("com.apple.dt.Xcode"));
        // The `com.jetbrains.*` wildcard covers JetBrains IDEs.
        assert!(is_unreliable_bundle("com.jetbrains.intellij"));
        assert!(is_unreliable_bundle("com.jetbrains.pycharm"));
        assert!(!is_unreliable_bundle("com.google.Chrome"));
        assert!(!is_unreliable_bundle("com.apple.TextEdit"));
        assert!(!is_unreliable_bundle(""));
    }

    #[test]
    fn copy_menu_item_needs_cmd_c_with_no_modifiers() {
        assert!(is_copy_menu_item(Some("C"), Some(0)));
        assert!(is_copy_menu_item(Some("c"), Some(0)));
        assert!(!is_copy_menu_item(Some("C"), Some(1)));
        assert!(!is_copy_menu_item(Some("V"), Some(0)));
        assert!(!is_copy_menu_item(None, Some(0)));
        assert!(!is_copy_menu_item(Some("C"), None));
    }
}
