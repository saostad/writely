//! Copy-and-paste fallback for apps whose text Accessibility doesn't expose
//! (e.g. Warp), or whose field is too large for the whole-field check.
//! It never runs for a secure (password) field — the caller checks that first.
//!
//! Reading posts ⌘C, waits briefly for the pasteboard to change, reads the
//! plain string, then restores the saved pasteboard. Replacing writes the
//! suggestion (marked transient + concealed so clipboard managers skip it),
//! posts ⌘V, waits, then restores. The user's text is never written to logs.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardItem, NSPasteboardType, NSPasteboardTypeString, NSPasteboardWriting,
};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGKeyCode,
};
use objc2_foundation::{NSArray, NSData, NSString};

/// Key codes for C and V (used with the Command flag for copy/paste).
const KEY_C: CGKeyCode = 8;
const KEY_V: CGKeyCode = 9;

/// How long to wait for the pasteboard to change after posting ⌘C:
/// 20 ms polls, up to 400 ms total. The UI is never blocked longer than that.
const COPY_POLL_MS: u64 = 20;
const COPY_POLLS: u32 = 20;

/// How long to wait after posting ⌘V before restoring the pasteboard.
const PASTE_WAIT_MS: u64 = 300;

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
