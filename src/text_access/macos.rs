use std::collections::HashSet;
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::ops::Range;
use std::ptr::NonNull;
use std::sync::{LazyLock, Mutex};

use objc2_application_services::{
    kAXTrustedCheckOptionPrompt, AXError, AXIsProcessTrustedWithOptions, AXUIElement, AXValue,
    AXValueType,
};
use objc2_core_foundation::{
    kCFBooleanFalse, kCFBooleanTrue, CFBoolean, CFDictionary, CFIndex, CFNumber, CFNumberType,
    CFRange, CFRetained, CFString, CFType, CGRect, Type,
};

use super::{
    char_to_utf16_offset, covers_whole_field, replaced_text, utf16_len, utf16_to_char_offset,
    window_for_caret, window_for_selection, CaretInfo, CaretRect, FieldSnapshot,
    WHOLE_READ_LIMIT_UTF16,
};

pub(crate) struct PlatformHandle {
    element: CFRetained<AXUIElement>,
}

const PERMISSION_ERROR: &str = "Writely needs Accessibility permission to see where you're typing. Open System Settings › Privacy & Security › Accessibility, turn on Writely. Writely starts working by itself once the switch is on.";

/// Pids already told that the caret position is unavailable, so the message
/// below logs once per pid instead of on every keystroke.
static NO_CARET_LOGGED: LazyLock<Mutex<HashSet<u32>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn log_no_caret_once(pid: u32) {
    let mut logged = NO_CARET_LOGGED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if logged.insert(pid) {
        eprintln!("Writely can't find the text cursor in this app, so the icon is hidden.");
    }
}

/// Pids already told that the app has no partial-read support, so the
/// message below logs once per pid instead of on every click.
static PARTIAL_FALLBACK_LOGGED: LazyLock<Mutex<HashSet<u32>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn log_partial_fallback_once(pid: u32, app: &str) {
    let mut logged = PARTIAL_FALLBACK_LOGGED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if logged.insert(pid) {
        eprintln!("Writely: {app} doesn't support partial reads; read the whole field.");
    }
}

fn timeout_error(error: AXError) -> String {
    format!(
        "The focused app didn't respond to Writely in time (AXError {}).",
        error.0
    )
}

fn read_error(error: AXError) -> String {
    if error == AXError::CannotComplete {
        timeout_error(error)
    } else {
        format!("Could not read the focused text field: AXError {}", error.0)
    }
}

pub(crate) fn set_messaging_timeout(element: &AXUIElement) {
    // SAFETY: element is a valid AXUIElement and the timeout is positive.
    let error = unsafe { element.set_messaging_timeout(0.25) };
    if error != AXError::Success {
        eprintln!(
            "Writely: could not set the Accessibility timeout (AXError {}).",
            error.0
        );
    }
}

pub(crate) fn copy_attr(element: &AXUIElement, name: &str) -> Result<CFRetained<CFType>, AXError> {
    let attr = CFString::from_str(name);
    let mut raw: *const CFType = std::ptr::null();
    let out = NonNull::from(&mut raw);
    // SAFETY: element and attr are valid CF objects and out points at a live local slot.
    let error = unsafe { element.copy_attribute_value(&attr, out) };
    if error != AXError::Success {
        return Err(error);
    }
    let Some(ptr) = NonNull::new(raw.cast_mut()) else {
        return Err(AXError::Failure);
    };
    // SAFETY: a successful copy returns a +1-retained CF object owned by the caller.
    Ok(unsafe { CFRetained::from_raw(ptr) })
}

fn copy_string(element: &AXUIElement, name: &str) -> Result<Option<String>, AXError> {
    let value = copy_attr(element, name)?;
    match value.downcast_ref::<CFString>() {
        Some(text) => Ok(Some(text.to_string())),
        None => Ok(None),
    }
}

fn copy_ax_value(element: &AXUIElement, name: &str) -> Result<CFRetained<AXValue>, AXError> {
    let value = copy_attr(element, name)?;
    value.downcast::<AXValue>().map_err(|_| AXError::Failure)
}

/// Whether the element is editable text, for the watcher's per-focus cache:
/// not a secure field and `AXValue` is settable. Called once per focus change.
pub(crate) fn element_is_editable(element: &AXUIElement) -> bool {
    match copy_string(element, "AXSubrole") {
        Ok(Some(subrole)) if subrole == "AXSecureTextField" => return false,
        // A missing subrole tells nothing, and a transient failure surfaces on
        // the next read; neither hides the icon here.
        Ok(_) | Err(_) => {}
    }
    let attr = CFString::from_str("AXValue");
    let mut flag: u8 = 0;
    let Some(out) = NonNull::new(&mut flag as *mut u8).map(|ptr| ptr.cast()) else {
        return true;
    };
    // SAFETY: element and attr are valid, and out points at a writable u8 with
    // the same layout as the Darwin Boolean the call writes.
    let error = unsafe { element.is_attribute_settable(&attr, out) };
    if error == AXError::Success {
        flag != 0
    } else if error == AXError::AttributeUnsupported {
        // Not settable (e.g. static text): not editable.
        false
    } else {
        // Transient failure surfaces on the next read; don't hide the icon.
        true
    }
}

fn ax_range(value: &AXValue) -> Option<CFRange> {
    let mut range = MaybeUninit::<CFRange>::uninit();
    let ptr = NonNull::new(range.as_mut_ptr().cast::<c_void>())?;
    // SAFETY: ptr points at writable CFRange storage and value is an AXValue.
    if unsafe { value.value(AXValueType::CFRange, ptr) } {
        // SAFETY: AXValueGetValue filled the CFRange when it returned true.
        Some(unsafe { range.assume_init() })
    } else {
        None
    }
}

fn ax_rect(value: &AXValue) -> Option<CGRect> {
    let mut rect = MaybeUninit::<CGRect>::uninit();
    let ptr = NonNull::new(rect.as_mut_ptr().cast::<c_void>())?;
    // SAFETY: ptr points at writable CGRect storage and value is an AXValue.
    if unsafe { value.value(AXValueType::CGRect, ptr) } {
        // SAFETY: AXValueGetValue filled the CGRect when it returned true.
        Some(unsafe { rect.assume_init() })
    } else {
        None
    }
}

fn make_ax_range(location: usize, length: usize) -> Option<CFRetained<AXValue>> {
    let mut range = CFRange {
        location: location as CFIndex,
        length: length as CFIndex,
    };
    let ptr = NonNull::new((&mut range as *mut CFRange).cast::<c_void>())?;
    // SAFETY: ptr points at a live CFRange for the duration of the call.
    unsafe { AXValue::new(AXValueType::CFRange, ptr) }
}

fn set_attr(element: &AXUIElement, name: &str, value: &CFType) -> AXError {
    let attr = CFString::from_str(name);
    // SAFETY: element, attr and value are valid CF objects of the expected kinds.
    unsafe { element.set_attribute_value(&attr, value) }
}

fn bounds_for_range(
    element: &AXUIElement,
    location: usize,
    length: usize,
) -> Result<CGRect, AXError> {
    let param = make_ax_range(location, length).ok_or(AXError::Failure)?;
    let param_type: &CFType = &param;
    let attr = CFString::from_str("AXBoundsForRange");
    let mut raw: *const CFType = std::ptr::null();
    let out = NonNull::from(&mut raw);
    // SAFETY: element and attr are valid and out points at a live local slot.
    let error = unsafe { element.copy_parameterized_attribute_value(&attr, param_type, out) };
    if error != AXError::Success {
        return Err(error);
    }
    let Some(ptr) = NonNull::new(raw.cast_mut()) else {
        return Err(AXError::Failure);
    };
    // SAFETY: a successful copy returns a +1-retained CF object owned by the caller.
    let value = unsafe { CFRetained::from_raw(ptr) };
    let ax_value = value.downcast::<AXValue>().map_err(|_| AXError::Failure)?;
    ax_rect(&ax_value).ok_or(AXError::Failure)
}

/// Reads `[location, location + length)` UTF-16 units via the parameterized
/// `AXStringForRange` attribute, so large documents are never read in full.
fn string_for_range(
    element: &AXUIElement,
    location: usize,
    length: usize,
) -> Result<String, AXError> {
    let param = make_ax_range(location, length).ok_or(AXError::Failure)?;
    let param_type: &CFType = &param;
    let attr = CFString::from_str("AXStringForRange");
    let mut raw: *const CFType = std::ptr::null();
    let out = NonNull::from(&mut raw);
    // SAFETY: element and attr are valid and out points at a live local slot.
    let error = unsafe { element.copy_parameterized_attribute_value(&attr, param_type, out) };
    if error != AXError::Success {
        return Err(error);
    }
    let Some(ptr) = NonNull::new(raw.cast_mut()) else {
        return Err(AXError::Failure);
    };
    // SAFETY: a successful copy returns a +1-retained CF object owned by the caller.
    let value = unsafe { CFRetained::from_raw(ptr) };
    match value.downcast_ref::<CFString>() {
        Some(text) => Ok(text.to_string()),
        None => Err(AXError::Failure),
    }
}

fn number_of_characters(element: &AXUIElement) -> Result<usize, AXError> {
    let value = copy_attr(element, "AXNumberOfCharacters")?;
    let number = value.downcast::<CFNumber>().map_err(|_| AXError::Failure)?;
    let mut out: i64 = 0;
    let ptr = NonNull::new((&mut out as *mut i64).cast::<c_void>());
    let Some(mut ptr) = ptr else {
        return Err(AXError::Failure);
    };
    // SAFETY: ptr points at a writable i64 for a signed-integer conversion.
    if unsafe { number.value(CFNumberType::SInt64Type, ptr.as_mut()) } {
        Ok(out.max(0) as usize)
    } else {
        Err(AXError::Failure)
    }
}

fn app_name(pid: u32) -> String {
    // SAFETY: creating an application element for a live pid is a plain FFI call.
    let app = unsafe { AXUIElement::new_application(pid as i32) };
    set_messaging_timeout(&app);
    match copy_string(&app, "AXTitle") {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        _ => "The app".to_string(),
    }
}

pub(crate) fn check_permission(prompt: bool) -> Result<(), String> {
    let key: &CFString = {
        // SAFETY: kAXTrustedCheckOptionPrompt is a framework-provided constant string.
        unsafe { kAXTrustedCheckOptionPrompt }
    };
    let value: &CFBoolean = match if prompt {
        // SAFETY: kCFBooleanTrue is a framework-provided constant.
        unsafe { kCFBooleanTrue }
    } else {
        // SAFETY: kCFBooleanFalse is a framework-provided constant.
        unsafe { kCFBooleanFalse }
    } {
        Some(value) => value,
        None => {
            eprintln!("Writely: could not read the Accessibility prompt option constant.");
            return Err(PERMISSION_ERROR.to_string());
        }
    };
    let options: CFRetained<CFDictionary<CFString, CFBoolean>> =
        CFDictionary::from_slices(&[key], &[value]);
    let ptr = CFRetained::as_ptr(&options).cast::<CFDictionary>();
    // SAFETY: CFDictionary generics are phantom data, so the untyped borrow shares the layout and options outlives it.
    let untyped: &CFDictionary = unsafe { ptr.as_ref() };
    // SAFETY: untyped is a valid CFDictionary with the prompt option for this call.
    let trusted = unsafe { AXIsProcessTrustedWithOptions(Some(untyped)) };
    if trusted {
        Ok(())
    } else {
        Err(PERMISSION_ERROR.to_string())
    }
}

pub(crate) fn focused_field() -> Result<Option<FieldSnapshot>, String> {
    // SAFETY: creating the system-wide element is a plain FFI call with no arguments.
    let system = unsafe { AXUIElement::new_system_wide() };
    set_messaging_timeout(&system);
    let focused = match copy_attr(&system, "AXFocusedUIElement") {
        Ok(value) => value,
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {
            return Ok(None);
        }
        Err(error) => return Err(read_error(error)),
    };
    let element_ref: &AXUIElement = {
        let ptr = CFRetained::as_ptr(&focused).cast::<AXUIElement>();
        // SAFETY: AXFocusedUIElement yields an AXUIElement valid while focused is alive.
        unsafe { ptr.as_ref() }
    };
    let element: CFRetained<AXUIElement> = element_ref.retain();
    set_messaging_timeout(&element);

    let mut pid: i32 = 0;
    let pid_out = NonNull::from(&mut pid);
    // SAFETY: pid_out points at a writable i32 for the duration of the call.
    let pid_error = unsafe { element.pid(pid_out) };
    if pid_error != AXError::Success {
        return Err(read_error(pid_error));
    }
    if pid < 0 || pid as u32 == std::process::id() {
        return Ok(None);
    }

    match copy_string(&element, "AXSubrole") {
        Ok(Some(subrole)) if subrole == "AXSecureTextField" => return Ok(None),
        Ok(_) => {}
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {}
        Err(error) => return Err(read_error(error)),
    }

    let range_value = match copy_ax_value(&element, "AXSelectedTextRange") {
        Ok(value) => value,
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {
            return Ok(None);
        }
        Err(error) => return Err(read_error(error)),
    };
    let selected = match ax_range(&range_value) {
        Some(range) => range,
        None => return Err(read_error(AXError::Failure)),
    };
    let caret_utf16 = if selected.location < 0 {
        0usize
    } else {
        selected.location as usize
    };
    let sel_len_utf16 = if selected.length < 0 {
        0usize
    } else {
        selected.length as usize
    };
    let sel_end_utf16 = caret_utf16.saturating_add(sel_len_utf16);
    let pid_u32 = pid as u32;

    // Bounded read: only a window of text around the caret, never the whole
    // document on keystroke-adjacent paths. When there is a selection the
    // window covers the whole selection.
    let total_utf16 = match number_of_characters(&element) {
        Ok(total) => total,
        // Fields without a length attribute fall back to the legacy whole
        // read below.
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {
            return whole_field_snapshot(element, pid_u32, caret_utf16, sel_len_utf16);
        }
        Err(error) => return Err(read_error(error)),
    };
    let sel_end_clamped = sel_end_utf16.min(total_utf16);
    let sel_start_clamped = caret_utf16.min(total_utf16);
    let (start, len) = if sel_len_utf16 > 0 {
        window_for_selection(sel_start_clamped, sel_end_clamped, total_utf16)
    } else {
        window_for_caret(sel_start_clamped, total_utf16)
    };
    match string_for_range(&element, start, len) {
        Ok(window) => {
            let caret = utf16_to_char_offset(&window, sel_start_clamped.saturating_sub(start));
            let selection = if sel_len_utf16 > 0 {
                let sel_start =
                    utf16_to_char_offset(&window, sel_start_clamped.saturating_sub(start));
                let sel_end = utf16_to_char_offset(&window, sel_end_clamped.saturating_sub(start));
                if sel_start != sel_end {
                    Some(sel_start..sel_end)
                } else {
                    None
                }
            } else {
                None
            };
            let Some(caret_rect) = caret_rect(&element, sel_start_clamped) else {
                log_no_caret_once(pid_u32);
                return Ok(None);
            };
            Ok(Some(FieldSnapshot {
                pid: pid_u32,
                text: window,
                caret,
                caret_rect,
                selection,
                window_start_utf16: start,
                field_len_utf16: total_utf16,
                handle: PlatformHandle { element },
            }))
        }
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {
            let name = app_name(pid_u32);
            log_partial_fallback_once(pid_u32, &name);
            if total_utf16 > WHOLE_READ_LIMIT_UTF16 {
                return Err("This text field is too large for Writely.".to_string());
            }
            whole_field_snapshot(element, pid_u32, sel_start_clamped, sel_len_utf16)
        }
        Err(error) => Err(read_error(error)),
    }
}

/// Legacy whole-field read for apps without `AXStringForRange` (or without
/// `AXNumberOfCharacters`). Only used on click paths, never per keystroke.
fn whole_field_snapshot(
    element: CFRetained<AXUIElement>,
    pid: u32,
    caret_utf16: usize,
    sel_len_utf16: usize,
) -> Result<Option<FieldSnapshot>, String> {
    let text = match copy_string(&element, "AXValue") {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(None),
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {
            return Ok(None);
        }
        Err(error) => return Err(read_error(error)),
    };
    let caret = utf16_to_char_offset(&text, caret_utf16);
    let selection = if sel_len_utf16 > 0 {
        let sel_end_utf16 = caret_utf16.saturating_add(sel_len_utf16);
        let sel_start = utf16_to_char_offset(&text, caret_utf16);
        let sel_end = utf16_to_char_offset(&text, sel_end_utf16);
        if sel_start != sel_end {
            Some(sel_start..sel_end)
        } else {
            None
        }
    } else {
        None
    };
    let Some(caret_rect) = caret_rect(&element, caret_utf16) else {
        log_no_caret_once(pid);
        return Ok(None);
    };
    let field_len_utf16 = utf16_len(&text);
    Ok(Some(FieldSnapshot {
        pid,
        text,
        caret,
        caret_rect,
        selection,
        window_start_utf16: 0,
        field_len_utf16,
        handle: PlatformHandle { element },
    }))
}

pub(crate) fn caret_info() -> Result<Option<CaretInfo>, String> {
    // Cheap path: reuse the watcher's cached focused element (no AXValue read,
    // no pid/subrole re-reads). A burst then costs just AXSelectedTextRange +
    // AXBoundsForRange.
    if let Some((element, pid, editable)) = crate::watcher::macos::cached_focused() {
        if pid == std::process::id() {
            return Ok(None);
        }
        if !editable {
            return Ok(None);
        }
        let range_value = match copy_ax_value(&element, "AXSelectedTextRange") {
            Ok(value) => value,
            Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {
                return Ok(None);
            }
            Err(error) => return Err(read_error(error)),
        };
        let selected = match ax_range(&range_value) {
            Some(range) => range,
            None => return Err(read_error(AXError::Failure)),
        };
        let caret_utf16 = if selected.location < 0 {
            0usize
        } else {
            selected.location as usize
        };
        let Some(rect) = caret_rect(&element, caret_utf16) else {
            log_no_caret_once(pid);
            return Ok(None);
        };
        return Ok(Some(CaretInfo {
            pid,
            caret_rect: rect,
        }));
    }
    // SAFETY: creating the system-wide element is a plain FFI call with no arguments.
    let system = unsafe { AXUIElement::new_system_wide() };
    set_messaging_timeout(&system);
    let focused = match copy_attr(&system, "AXFocusedUIElement") {
        Ok(value) => value,
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {
            return Ok(None);
        }
        Err(error) => return Err(read_error(error)),
    };
    let element_ref: &AXUIElement = {
        let ptr = CFRetained::as_ptr(&focused).cast::<AXUIElement>();
        // SAFETY: AXFocusedUIElement yields an AXUIElement valid while focused is alive.
        unsafe { ptr.as_ref() }
    };
    let element: CFRetained<AXUIElement> = element_ref.retain();
    set_messaging_timeout(&element);

    let mut pid: i32 = 0;
    let pid_out = NonNull::from(&mut pid);
    // SAFETY: pid_out points at a writable i32 for the duration of the call.
    let pid_error = unsafe { element.pid(pid_out) };
    if pid_error != AXError::Success {
        return Err(read_error(pid_error));
    }
    if pid < 0 || pid as u32 == std::process::id() {
        return Ok(None);
    }

    match copy_string(&element, "AXSubrole") {
        Ok(Some(subrole)) if subrole == "AXSecureTextField" => return Ok(None),
        Ok(_) => {}
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {}
        Err(error) => return Err(read_error(error)),
    }

    let range_value = match copy_ax_value(&element, "AXSelectedTextRange") {
        Ok(value) => value,
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {
            return Ok(None);
        }
        Err(error) => return Err(read_error(error)),
    };
    let selected = match ax_range(&range_value) {
        Some(range) => range,
        None => return Err(read_error(AXError::Failure)),
    };
    let caret_utf16 = if selected.location < 0 {
        0usize
    } else {
        selected.location as usize
    };
    let Some(rect) = caret_rect(&element, caret_utf16) else {
        log_no_caret_once(pid as u32);
        return Ok(None);
    };
    Ok(Some(CaretInfo {
        pid: pid as u32,
        caret_rect: rect,
    }))
}

/// The caret screen rectangle, or `None` when neither bounds lookup yields a
/// real caret position. There is no fallback: without a real position the
/// icon stays hidden.
fn caret_rect(element: &AXUIElement, caret_utf16: usize) -> Option<CaretRect> {
    if let Ok(rect) = bounds_for_range(element, caret_utf16, 0) {
        if rect.size.width != 0.0 || rect.size.height != 0.0 {
            return Some(CaretRect {
                x: rect.origin.x,
                y: rect.origin.y,
                width: rect.size.width,
                height: rect.size.height,
                physical: false,
            });
        }
    }
    if caret_utf16 > 0 {
        if let Ok(rect) = bounds_for_range(element, caret_utf16 - 1, 1) {
            if rect.size.width != 0.0 || rect.size.height != 0.0 {
                return Some(CaretRect {
                    x: rect.origin.x + rect.size.width,
                    y: rect.origin.y,
                    width: 0.0,
                    height: rect.size.height,
                    physical: false,
                });
            }
        }
    }
    None
}

pub(crate) fn read_window(
    handle: &PlatformHandle,
    start_utf16: usize,
    len_utf16: usize,
) -> Result<String, String> {
    set_messaging_timeout(&handle.element);
    match string_for_range(&handle.element, start_utf16, len_utf16) {
        Ok(text) => Ok(text),
        Err(error) => Err(read_error(error)),
    }
}

pub(crate) fn read_text(handle: &PlatformHandle) -> Result<String, String> {
    set_messaging_timeout(&handle.element);
    match copy_string(&handle.element, "AXValue") {
        Ok(Some(text)) => Ok(text),
        Ok(None) => Err(read_error(AXError::Failure)),
        Err(error) => Err(read_error(error)),
    }
}

pub(crate) fn replace_range(
    snap: &FieldSnapshot,
    range: Range<usize>,
    new_text: &str,
) -> Result<(), String> {
    let element: &AXUIElement = &snap.handle.element;
    set_messaging_timeout(element);
    let name = app_name(snap.pid);
    // Window-relative range math; the offset is added only for AX.
    let start = snap.window_start_utf16 + char_to_utf16_offset(&snap.text, range.start);
    let end = snap.window_start_utf16 + char_to_utf16_offset(&snap.text, range.end);
    let length = end.saturating_sub(start);

    let range_value = match make_ax_range(start, length) {
        Some(value) => value,
        None => {
            return Err(format!(
                "{name} didn't accept the replacement text (AXError {}).",
                AXError::Failure.0
            ));
        }
    };
    let range_type: &CFType = &range_value;
    let select_error = set_attr(element, "AXSelectedTextRange", range_type);
    if select_error == AXError::CannotComplete {
        return Err(timeout_error(select_error));
    }
    if select_error != AXError::Success {
        return Err(format!(
            "{name} didn't accept the replacement text (AXError {}).",
            select_error.0
        ));
    }

    let new_cf = CFString::from_str(new_text);
    let new_type: &CFType = &new_cf;
    let selected_error = set_attr(element, "AXSelectedText", new_type);
    if selected_error == AXError::CannotComplete {
        return Err(timeout_error(selected_error));
    }
    // The expected window text after the replacement. Verification re-reads
    // the window at the same start with the expected length (which differs
    // from the snapshot length when the suggestion is longer/shorter).
    let expected_window = replaced_text(&snap.text, &range, new_text);
    let expected_len_utf16 = utf16_len(&expected_window);
    if selected_error != AXError::Success {
        if !covers_whole_field(snap) {
            return Err(format!(
                "{name} didn't accept the replacement text (AXError {}).",
                selected_error.0
            ));
        }
        eprintln!(
            "Writely: setting AXSelectedText failed (AXError {}); replaced the whole field text instead.",
            selected_error.0
        );
        let whole_cf = CFString::from_str(&expected_window);
        let whole_type: &CFType = &whole_cf;
        let value_error = set_attr(element, "AXValue", whole_type);
        if value_error == AXError::CannotComplete {
            return Err(timeout_error(value_error));
        }
        if value_error != AXError::Success {
            return Err(format!(
                "{name} didn't accept the replacement text (AXError {}).",
                value_error.0
            ));
        }
    }

    match read_at(&snap.handle, snap, expected_len_utf16) {
        Ok(actual) if actual == expected_window => Ok(()),
        Ok(_) => Err(format!(
            "{name} didn't accept the replacement text (AXError {}).",
            AXError::Failure.0
        )),
        Err(error) if error.starts_with("The focused app didn't respond") => Err(error),
        Err(error) => Err(format!(
            "{name} didn't accept the replacement text ({error})."
        )),
    }
}

/// Reads `[snap.window_start_utf16, + len_utf16)` via a partial read,
/// falling back to a whole-field read when the snapshot covers the whole
/// field but the app has no partial-read support.
pub(crate) fn read_at(
    handle: &PlatformHandle,
    snap: &FieldSnapshot,
    len_utf16: usize,
) -> Result<String, String> {
    match read_window(handle, snap.window_start_utf16, len_utf16) {
        Ok(actual) => Ok(actual),
        Err(window_error) if covers_whole_field(snap) => match read_text(handle) {
            Ok(actual) => Ok(actual),
            Err(_) => Err(window_error),
        },
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{read_error, timeout_error};
    use objc2_application_services::AXError;

    #[test]
    fn cannot_complete_maps_to_timeout_message() {
        let expected = "The focused app didn't respond to Writely in time (AXError -25204).";
        assert_eq!(read_error(AXError::CannotComplete), expected);
        assert_eq!(timeout_error(AXError::CannotComplete), expected);
    }

    #[test]
    fn other_ax_errors_keep_read_message() {
        assert_eq!(
            read_error(AXError::Failure),
            "Could not read the focused text field: AXError -25200"
        );
    }
}
