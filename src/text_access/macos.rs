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
    CFRange, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize, Type,
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
    // While cursor reporting was newly enabled for this pid, the caret
    // position may lag behind the focused element (the retry ends as soon as
    // the element appears); the "no text cursor appeared within 12 s" line at
    // the end of the window covers a lasting failure, so don't log here.
    if crate::watcher::macos::no_caret_suppressed(pid) {
        return;
    }
    let mut logged = NO_CARET_LOGGED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if logged.insert(pid) {
        eprintln!("Writely can't find the text cursor in this app, so the icon is hidden.");
    }
}

/// Whether a caret rectangle from `AXBoundsForRange` is a real caret
/// position: it must have a non-zero height and its origin must lie inside
/// the focused element's `AXFrame`, grown by 4 pt on each side. Some apps
/// (e.g. the Claude desktop app) report a bogus {0,0,0,0}-style rect that
/// must not anchor the icon.
pub(crate) fn caret_rect_valid(rect: &CGRect, frame: &CGRect) -> bool {
    if rect.size.height == 0.0 {
        return false;
    }
    const MARGIN: f64 = 4.0;
    rect.origin.x >= frame.origin.x - MARGIN
        && rect.origin.x <= frame.origin.x + frame.size.width + MARGIN
        && rect.origin.y >= frame.origin.y - MARGIN
        && rect.origin.y <= frame.origin.y + frame.size.height + MARGIN
}

/// The focused element's `AXFrame` in top-left screen coordinates.
fn element_frame(element: &AXUIElement) -> Result<CGRect, AXError> {
    let value = copy_ax_value(element, "AXFrame")?;
    ax_rect(&value).ok_or(AXError::Failure)
}

/// Picks the input area for caret validation: the scroll-area parent's frame
/// when the parent role is `AXScrollArea`, else the element's own frame.
/// Pure so it can be unit-tested.
pub(crate) fn choose_input_area(
    element_frame: CGRect,
    parent: Option<(&str, CGRect)>,
) -> CGRect {
    match parent {
        Some(("AXScrollArea", parent_frame)) => parent_frame,
        _ => element_frame,
    }
}

/// The input-area frame for `element`: its scroll-area parent's frame when
/// there is one, else its own frame. `None` when no frame is readable.
fn input_area_frame(element: &AXUIElement) -> Option<CGRect> {
    let own = element_frame(element).ok()?;
    let parent_value = copy_attr(element, "AXParent").ok()?;
    let ptr = CFRetained::as_ptr(&parent_value).cast::<AXUIElement>();
    // SAFETY: AXParent yields an AXUIElement valid while parent_value is alive.
    let parent: &AXUIElement = unsafe { ptr.as_ref() };
    let role = copy_string(parent, "AXRole").ok().flatten();
    let parent_frame = element_frame(parent).ok();
    match (role.as_deref(), parent_frame) {
        (Some(role), Some(frame)) => Some(choose_input_area(own, Some((role, frame)))),
        _ => Some(own),
    }
}

/// The caret rectangle via text markers (`AXSelectedTextMarkerRange` →
/// `AXBoundsForTextMarkerRange`), which works for Safari/Chrome/Firefox/
/// Electron web content. The marker range is an opaque `CFType` passed
/// straight back as the parameter.
fn marker_bounds(element: &AXUIElement) -> Result<CGRect, AXError> {
    let marker = copy_attr(element, "AXSelectedTextMarkerRange")?;
    let marker_type: &CFType = &marker;
    let attr = CFString::from_str("AXBoundsForTextMarkerRange");
    let mut raw: *const CFType = std::ptr::null();
    let out = NonNull::from(&mut raw);
    // SAFETY: element and attr are valid and out points at a live local slot.
    let error = unsafe { element.copy_parameterized_attribute_value(&attr, marker_type, out) };
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
/// Whether setting `AXSelectedText` was silently ignored: the verification
/// read still shows the snapshot text instead of the expected text.
pub(crate) fn selected_write_ignored(actual: &str, snapshot_text: &str, expected: &str) -> bool {
    actual == snapshot_text && actual != expected
}

/// Roles that hold editable text, for the text-box icon anchor.
pub(crate) fn is_text_role(role: &str) -> bool {
    matches!(
        role,
        "AXTextArea" | "AXTextField" | "AXComboBox" | "AXSearchField"
    )
}

/// Whether an element frame can anchor the icon: non-zero, and no larger
/// than the biggest screen (an absurd frame means "no anchor").
pub(crate) fn frame_usable(frame: &CGRect, max_screen: (f64, f64)) -> bool {
    frame.size.width > 0.0
        && frame.size.height > 0.0
        && frame.size.width <= max_screen.0
        && frame.size.height <= max_screen.1
}

/// Size of the icon panel, kept in sync with `overlay/macos.rs`.
const ICON_SIZE: f64 = 20.0;
/// Inset of the icon inside the text-box frame.
const BOX_INSET: f64 = 4.0;

/// A zero-size rect that makes `overlay::show_at` (which puts the icon 4 pt
/// right of the rect and centres it on the rect's middle) land the 20x20
/// icon just inside the frame's top-right corner, inset 4 pt.
pub(crate) fn box_anchor_rect(frame: &CGRect) -> CaretRect {
    CaretRect {
        x: frame.origin.x + frame.size.width - ICON_SIZE - BOX_INSET - 4.0,
        y: frame.origin.y + BOX_INSET + ICON_SIZE / 2.0,
        width: 0.0,
        height: 0.0,
        physical: false,
    }
}

/// Pids already told the icon is at the text box, so the message below logs
/// once per pid instead of on every keystroke.
static BOX_ANCHOR_LOGGED: LazyLock<Mutex<HashSet<u32>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn log_box_anchor_once(pid: u32, app: &str) {
    let mut logged = BOX_ANCHOR_LOGGED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if logged.insert(pid) {
        eprintln!("Writely: {app}: caret position unavailable; showing the icon at the text box.");
    }
}

/// Largest screen size in points, for the absurd-frame check. `None` when it
/// can't be read (the size check is then skipped).
fn max_screen_size() -> Option<(f64, f64)> {
    let mtm = objc2_foundation::MainThreadMarker::new()?;
    let screens = objc2_app_kit::NSScreen::screens(mtm);
    if screens.count() == 0 {
        return None;
    }
    let mut max: (f64, f64) = (0.0, 0.0);
    for index in 0..screens.count() {
        let size = screens.objectAtIndex(index).frame().size;
        max = (max.0.max(size.width), max.1.max(size.height));
    }
    if max.0 <= 0.0 || max.1 <= 0.0 {
        None
    } else {
        Some(max)
    }
}

/// The text-box anchor for `element` when there is no valid caret rect:
/// the top-right corner of its input area (the scroll-area parent's frame
/// when there is one, else its own `AXFrame`), or `None` when the element
/// isn't a text element, is secure, or has no usable frame.
fn box_anchor_for(element: &AXUIElement, pid: u32) -> Option<CaretRect> {
    match copy_string(element, "AXSubrole") {
        Ok(Some(subrole)) if subrole == "AXSecureTextField" => return None,
        _ => {}
    }
    let is_text = match copy_string(element, "AXRole") {
        Ok(Some(role)) => is_text_role(&role),
        // Without a readable role, an AXValue that is a settable string
        // still counts as a text element.
        _ => match copy_string(element, "AXValue") {
            Ok(Some(_)) => element_is_editable(element),
            _ => false,
        },
    };
    if !is_text {
        return None;
    }
    let frame = input_area_frame(element)?;
    let usable = match max_screen_size() {
        Some(max) => frame_usable(&frame, max),
        None => frame.size.width > 0.0 && frame.size.height > 0.0,
    };
    if !usable {
        return None;
    }
    let name = app_name(pid);
    log_box_anchor_once(pid, &name);
    Some(box_anchor_rect(&frame))
}

/// Pids already told the icon is at the app window, so the message below
/// logs once per pid instead of on every keystroke.
static WINDOW_ANCHOR_LOGGED: LazyLock<Mutex<HashSet<u32>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn log_window_anchor_once(pid: u32, app: &str) {
    let mut logged = WINDOW_ANCHOR_LOGGED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if logged.insert(pid) {
        eprintln!(
            "Writely: {app}: caret position unavailable; showing the icon at the app window."
        );
    }
}

/// Last-resort icon anchor for apps whose element has no usable frame: the
/// top-right corner of the focused window's frame, or `None` when that is
/// missing or absurd too.
fn window_anchor_for(element: &AXUIElement, pid: u32) -> Option<CaretRect> {
    let window_value = copy_attr(element, "AXWindow").ok()?;
    let ptr = CFRetained::as_ptr(&window_value).cast::<AXUIElement>();
    // SAFETY: AXWindow yields an AXUIElement valid while window_value is alive.
    let window: &AXUIElement = unsafe { ptr.as_ref() };
    let frame = element_frame(window).ok()?;
    let usable = match max_screen_size() {
        Some(max) => frame_usable(&frame, max),
        None => frame.size.width > 0.0 && frame.size.height > 0.0,
    };
    if !usable {
        return None;
    }
    let name = app_name(pid);
    log_window_anchor_once(pid, &name);
    Some(box_anchor_rect(&frame))
}

/// The caret rectangle, falling back to the text-box anchor and then the app
/// window anchor when there is no valid caret position. Returns `None` only
/// when none exists (the caller then logs the no-caret message and hides the
/// icon).
fn caret_or_box_anchor(element: &AXUIElement, pid: u32, caret_utf16: usize) -> Option<CaretRect> {
    if let Some(rect) = caret_rect(element, caret_utf16) {
        return Some(rect);
    }
    if let Some(anchor) = box_anchor_for(element, pid) {
        return Some(anchor);
    }
    window_anchor_for(element, pid)
}

/// Whether the currently focused element is a secure (password) field.
/// Unknown or unreadable focus counts as secure: the copy-and-paste fallback
/// must never run for a password field.
pub(crate) fn focused_is_secure() -> bool {
    // SAFETY: creating the system-wide element is a plain FFI call with no arguments.
    let system = unsafe { AXUIElement::new_system_wide() };
    set_messaging_timeout(&system);
    let focused = match copy_attr(&system, "AXFocusedUIElement") {
        Ok(value) => value,
        // Unknown focus: assume secure so the fallback never runs.
        Err(_) => return true,
    };
    let ptr = CFRetained::as_ptr(&focused).cast::<AXUIElement>();
    // SAFETY: AXFocusedUIElement yields an AXUIElement valid while focused is alive.
    let element: &AXUIElement = unsafe { ptr.as_ref() };
    match copy_string(element, "AXSubrole") {
        Ok(Some(subrole)) => subrole == "AXSecureTextField",
        // Unreadable subrole: assume secure so the fallback never runs.
        _ => true,
    }
}

/// Whether the two verification reads show the `AXSelectedText` write was
/// silently ignored: the expected-length read isn't the expected text, but a
/// re-read at the snapshot length still shows the snapshot text. The second
/// read matters when old and new lengths differ: a truncated read of an
/// unchanged field then equals neither text.
pub(crate) fn selected_write_ignored_reread(
    expected_read: &str,
    snapshot_read: &str,
    snapshot_text: &str,
    expected: &str,
) -> bool {
    expected_read != expected && selected_write_ignored(snapshot_read, snapshot_text, expected)
}

/// Moves the caret to `offset_utf16` without failing the replace: a failure
/// is logged but isn't an error for the user.
fn move_caret_to(element: &AXUIElement, offset_utf16: usize) {
    let Some(range_value) = make_ax_range(offset_utf16, 0) else {
        eprintln!(
            "Writely: could not move the caret after replacing text (AXError {}).",
            AXError::Failure.0
        );
        return;
    };
    let range_type: &CFType = &range_value;
    let error = set_attr(element, "AXSelectedTextRange", range_type);
    if error != AXError::Success {
        eprintln!(
            "Writely: could not move the caret after replacing text (AXError {}).",
            error.0
        );
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
        // No caret offset (e.g. Firefox): fall back to a whole-field
        // snapshot when the text itself is readable.
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {
            return snapshot_without_caret(element, pid as u32);
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
            let Some(caret_rect) = caret_or_box_anchor(&element, pid_u32, sel_start_clamped) else {
                log_no_caret_once(pid_u32);
                return Ok(None);
            };
            Ok(Some(FieldSnapshot {
                pid: pid_u32,
                text: window,
                caret,
                caret_known: true,
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

/// Pids already told the whole field is checked because the caret offset is
/// unavailable, so the message below logs once per pid instead of on click.
static WHOLE_FIELD_LOGGED: LazyLock<Mutex<HashSet<u32>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn log_whole_field_once(pid: u32, app: &str) {
    let mut logged = WHOLE_FIELD_LOGGED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if logged.insert(pid) {
        eprintln!("Writely: {app}: caret offset unavailable; checking the whole field.");
    }
}

/// Snapshot for apps where the caret offset can't be read (Firefox reports
/// `NoValue` for `AXSelectedTextRange`) but the text itself is readable: the
/// whole field becomes the snapshot and the click checks it as a whole.
/// Returns `Ok(None)` when no text is readable (the clipboard fallback or
/// the can't-read message then applies).
fn snapshot_without_caret(
    element: CFRetained<AXUIElement>,
    pid: u32,
) -> Result<Option<FieldSnapshot>, String> {
    let text = match copy_string(&element, "AXValue") {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(None),
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {
            return Ok(None);
        }
        Err(error) => return Err(read_error(error)),
    };
    if utf16_len(&text) > WHOLE_READ_LIMIT_UTF16 {
        return Err("This text field is too large for Writely.".to_string());
    }
    let Some(caret_rect) =
        box_anchor_for(&element, pid).or_else(|| window_anchor_for(&element, pid))
    else {
        log_no_caret_once(pid);
        return Ok(None);
    };
    let name = app_name(pid);
    log_whole_field_once(pid, &name);
    let field_len_utf16 = utf16_len(&text);
    Ok(Some(FieldSnapshot {
        pid,
        text,
        caret: 0,
        caret_known: false,
        caret_rect,
        selection: None,
        window_start_utf16: 0,
        field_len_utf16,
        handle: PlatformHandle { element },
    }))
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
    let Some(caret_rect) = caret_or_box_anchor(&element, pid, caret_utf16) else {
        log_no_caret_once(pid);
        return Ok(None);
    };
    let field_len_utf16 = utf16_len(&text);
    Ok(Some(FieldSnapshot {
        pid,
        text,
        caret,
        caret_known: true,
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
            // No caret offset (e.g. Firefox): the icon can still anchor to
            // the text box when the element has a usable frame.
            Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {
                return Ok(box_anchor_for(&element, pid)
                    .or_else(|| window_anchor_for(&element, pid))
                    .map(|caret_rect| CaretInfo { pid, caret_rect }));
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
        let Some(rect) = caret_or_box_anchor(&element, pid, caret_utf16) else {
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
        // No caret offset (e.g. Firefox): the icon can still anchor to the
        // text box when the element has a usable frame.
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {
            return Ok(box_anchor_for(&element, pid as u32)
                .or_else(|| window_anchor_for(&element, pid as u32))
                .map(|caret_rect| CaretInfo {
                    pid: pid as u32,
                    caret_rect,
                }));
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
    let Some(rect) = caret_or_box_anchor(&element, pid as u32, caret_utf16) else {
        log_no_caret_once(pid as u32);
        return Ok(None);
    };
    Ok(Some(CaretInfo {
        pid: pid as u32,
        caret_rect: rect,
    }))
}

/// Picks the first valid caret rectangle from the zero-length lookup at
/// the caret and the right edge of the previous character: each candidate is
/// validated with `caret_rect_valid`, so an out-of-the-box zero-length rect
/// (e.g. TextEdit's line above the frame at end of text) falls back to the
/// previous character instead of hiding the icon. Returns `None` when both
/// fail. Without a readable frame the first non-empty candidate is trusted.
pub(crate) fn pick_valid_caret_rect(
    zero: Option<CGRect>,
    prev_raw: Option<CGRect>,
    frame: Option<CGRect>,
) -> Option<CGRect> {
    let mut candidates: Vec<CGRect> = Vec::with_capacity(2);
    if let Some(rect) = zero {
        if rect.size.width != 0.0 || rect.size.height != 0.0 {
            candidates.push(rect);
        }
    }
    if let Some(raw) = prev_raw {
        if raw.size.width != 0.0 || raw.size.height != 0.0 {
            candidates.push(CGRect {
                origin: CGPoint {
                    x: raw.origin.x + raw.size.width,
                    y: raw.origin.y,
                },
                size: CGSize {
                    width: 0.0,
                    height: raw.size.height,
                },
            });
        }
    }
    for rect in candidates {
        match frame {
            Some(frame) => {
                if caret_rect_valid(&rect, &frame) {
                    return Some(rect);
                }
            }
            None => return Some(rect),
        }
    }
    None
}

/// The caret screen rectangle, in this order (Input Source Pro):
/// 1. text markers (`AXSelectedTextMarkerRange` → `AXBoundsForTextMarkerRange`,
///    works for Safari/Chrome/Firefox/Electron web content);
/// 2. `AXBoundsForRange` (zero length, then the previous-char fallback);
/// 3. validated against the input area (the scroll-area parent's frame when
///    there is one, else the element's frame);
/// 4. otherwise `None` (the caller falls back to the container anchor).
///
/// A rect outside the input area (or with zero height) is bogus and ignored;
/// when no input-area frame is readable, today's behaviour is kept.
fn caret_rect(element: &AXUIElement, caret_utf16: usize) -> Option<CaretRect> {
    let frame = input_area_frame(element);
    if let Ok(marker_rect) = marker_bounds(element) {
        let valid = match frame {
            Some(frame) => caret_rect_valid(&marker_rect, &frame),
            None => marker_rect.size.height != 0.0,
        };
        if valid {
            return Some(CaretRect {
                x: marker_rect.origin.x,
                y: marker_rect.origin.y,
                width: marker_rect.size.width,
                height: marker_rect.size.height,
                physical: false,
            });
        }
    }
    let zero = bounds_for_range(element, caret_utf16, 0).ok();
    let prev_raw = if caret_utf16 > 0 {
        bounds_for_range(element, caret_utf16 - 1, 1).ok()
    } else {
        None
    };
    let rect = pick_valid_caret_rect(zero, prev_raw, frame)?;
    Some(CaretRect {
        x: rect.origin.x,
        y: rect.origin.y,
        width: rect.size.width,
        height: rect.size.height,
        physical: false,
    })
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

/// Writes the whole field via `AXValue` and verifies the result against
/// `expected_window`.
fn write_whole_field(
    snap: &FieldSnapshot,
    name: &str,
    expected_window: &str,
    expected_len_utf16: usize,
) -> Result<(), String> {
    let element: &AXUIElement = &snap.handle.element;
    let whole_cf = CFString::from_str(expected_window);
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

pub(crate) fn replace_range(
    snap: &FieldSnapshot,
    range: Range<usize>,
    new_text: &str,
) -> Result<(), String> {
    let element: &AXUIElement = &snap.handle.element;
    set_messaging_timeout(element);
    let name = app_name(snap.pid);
    // Without a known caret (e.g. Firefox) the selected-range machinery
    // can't be trusted — setting the range fails silently there — so the
    // whole field is written directly. The snapshot always covers the whole
    // field on this path, which is what allows the `AXValue` write.
    if !snap.caret_known {
        if !covers_whole_field(snap) {
            return Err(format!(
                "{name} didn't accept the replacement text. Select a shorter part of the text, or copy the suggestion by hand."
            ));
        }
        let expected_window = replaced_text(&snap.text, &range, new_text);
        let expected_len_utf16 = utf16_len(&expected_window);
        eprintln!("Writely: {name}: caret offset unavailable; replaced the whole field text.");
        return write_whole_field(snap, &name, &expected_window, expected_len_utf16);
    }
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
    let new_len_utf16 = utf16_len(new_text);
    let caret_target_utf16 = start.saturating_add(new_len_utf16);
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
        move_caret_to(element, caret_target_utf16);
    } else {
        // The set reported Success but some apps (e.g. Chrome) ignore it:
        // verify, and fall back to the whole-field write when the text is
        // unchanged and the snapshot covers the whole field. When old and new
        // lengths differ, the expected-length read of an unchanged field is
        // truncated to neither text, so re-read at the snapshot length
        // before deciding.
        match read_at(&snap.handle, snap, expected_len_utf16) {
            Ok(actual) if actual == expected_window => return Ok(()),
            Ok(expected_read) => {
                let snapshot_len_utf16 = utf16_len(&snap.text);
                let ignored = match read_at(&snap.handle, snap, snapshot_len_utf16) {
                    Ok(snapshot_read) => selected_write_ignored_reread(
                        &expected_read,
                        &snapshot_read,
                        &snap.text,
                        &expected_window,
                    ),
                    Err(_) => selected_write_ignored(&expected_read, &snap.text, &expected_window),
                };
                if !ignored {
                    return Err(format!(
                        "{name} didn't accept the replacement text (AXError {}).",
                        AXError::Failure.0
                    ));
                }
            }
            Err(error) if error.starts_with("The focused app didn't respond") => {
                return Err(error);
            }
            Err(error) => {
                return Err(format!(
                    "{name} didn't accept the replacement text ({error})."
                ));
            }
        }
        if !covers_whole_field(snap) {
            return Err(format!(
                "{name} didn't accept the replacement text. Select a shorter part of the text, or copy the suggestion by hand."
            ));
        }
        eprintln!(
            "Writely: setting AXSelectedText was ignored (the text is unchanged); replaced the whole field text instead."
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
        move_caret_to(element, caret_target_utf16);
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
    use super::{
        caret_rect_valid, choose_input_area, pick_valid_caret_rect, read_error,
        selected_write_ignored, selected_write_ignored_reread, timeout_error,
    };
    use objc2_application_services::AXError;
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};

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

    #[test]
    fn selected_write_outcome_distinguishes_ignored_from_mismatch() {
        // Unchanged text that isn't the expected text: the app ignored it.
        assert!(selected_write_ignored("old", "old", "new"));
        // Accepted: actual already equals the expected text.
        assert!(!selected_write_ignored("new", "old", "new"));
        // Changed to something else: a mismatch, not a silent ignore.
        assert!(!selected_write_ignored("other", "old", "new"));
        // Identical snapshot and expectation: nothing to distinguish.
        assert!(!selected_write_ignored("same", "same", "same"));
    }

    #[test]
    fn bogus_caret_rect_outside_frame_is_rejected() {
        let frame = CGRect {
            origin: CGPoint { x: 100.0, y: 100.0 },
            size: CGSize {
                width: 400.0,
                height: 200.0,
            },
        };
        // Claude's bogus rect: zero size, far outside the frame.
        let bogus = CGRect {
            origin: CGPoint { x: 0.0, y: 1117.0 },
            size: CGSize {
                width: 0.0,
                height: 0.0,
            },
        };
        assert!(!caret_rect_valid(&bogus, &frame));
        // Zero height alone is rejected even inside the frame.
        let flat = CGRect {
            origin: CGPoint { x: 150.0, y: 150.0 },
            size: CGSize {
                width: 2.0,
                height: 0.0,
            },
        };
        assert!(!caret_rect_valid(&flat, &frame));
        // A real caret rect inside the frame (and within the 4 pt margin)
        // is accepted.
        let good = CGRect {
            origin: CGPoint { x: 180.0, y: 165.0 },
            size: CGSize {
                width: 0.0,
                height: 16.0,
            },
        };
        assert!(caret_rect_valid(&good, &frame));
        let on_edge = CGRect {
            origin: CGPoint { x: 100.0, y: 100.0 },
            size: CGSize {
                width: 1.0,
                height: 16.0,
            },
        };
        assert!(caret_rect_valid(&on_edge, &frame));
        // Outside the frame plus the 4 pt margin is rejected.
        let far = CGRect {
            origin: CGPoint { x: 505.0, y: 150.0 },
            size: CGSize {
                width: 1.0,
                height: 16.0,
            },
        };
        assert!(!caret_rect_valid(&far, &frame));
    }

    #[test]
    fn text_box_anchor_helpers() {
        use super::{box_anchor_rect, frame_usable, is_text_role};
        assert!(is_text_role("AXTextArea"));
        assert!(is_text_role("AXTextField"));
        assert!(is_text_role("AXComboBox"));
        assert!(is_text_role("AXSearchField"));
        assert!(!is_text_role("AXStaticText"));
        assert!(!is_text_role("AXButton"));
        // Absurd frames (zero size or larger than the screen) are no anchor.
        let frame = CGRect {
            origin: CGPoint { x: 100.0, y: 100.0 },
            size: CGSize {
                width: 400.0,
                height: 200.0,
            },
        };
        assert!(frame_usable(&frame, (1920.0, 1080.0)));
        let zero = CGRect {
            origin: CGPoint { x: 100.0, y: 100.0 },
            size: CGSize {
                width: 0.0,
                height: 0.0,
            },
        };
        assert!(!frame_usable(&zero, (1920.0, 1080.0)));
        let huge = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: 4000.0,
                height: 200.0,
            },
        };
        assert!(!frame_usable(&huge, (1920.0, 1080.0)));
        // The anchor lands the 20x20 icon inside the frame's top-right
        // corner, inset 4 pt (overlay adds +4 x and centres vertically).
        let anchor = box_anchor_rect(&frame);
        assert_eq!(anchor.x + anchor.width + 4.0, 100.0 + 400.0 - 20.0 - 4.0);
        assert_eq!(anchor.y + anchor.height / 2.0 - 10.0, 100.0 + 4.0);
        assert!(!anchor.physical);
    }

    #[test]
    fn falls_back_to_previous_character_when_caret_rect_is_out_of_the_box() {
        // TextEdit's AXFrame and the two bounds lookups from the report.
        let frame = CGRect {
            origin: CGPoint { x: 214.0, y: 212.0 },
            size: CGSize {
                width: 586.0,
                height: 382.0,
            },
        };
        // Zero-length rect at end of text: a line above the frame, rejected.
        let zero = CGRect {
            origin: CGPoint { x: 336.9, y: 198.0 },
            size: CGSize {
                width: 0.0,
                height: 14.0,
            },
        };
        assert!(!caret_rect_valid(&zero, &frame));
        // Previous-character rect: valid, so its right edge wins.
        let prev_raw = CGRect {
            origin: CGPoint { x: 333.5, y: 212.0 },
            size: CGSize {
                width: 3.3,
                height: 14.0,
            },
        };
        let picked =
            pick_valid_caret_rect(Some(zero), Some(prev_raw), Some(frame)).expect("fallback rect");
        assert!((picked.origin.x - 336.8).abs() < 0.01);
        assert_eq!(picked.origin.y, 212.0);
        assert_eq!(picked.size.width, 0.0);
        assert_eq!(picked.size.height, 14.0);
        // A valid zero-length rect still wins over the previous character.
        let good_zero = CGRect {
            origin: CGPoint { x: 300.0, y: 250.0 },
            size: CGSize {
                width: 0.0,
                height: 14.0,
            },
        };
        let picked_first =
            pick_valid_caret_rect(Some(good_zero), Some(prev_raw), Some(frame)).expect("zero rect");
        assert_eq!(picked_first.origin.x, 300.0);
        // Both failing means no position.
        let bogus = CGRect {
            origin: CGPoint { x: 0.0, y: 1117.0 },
            size: CGSize {
                width: 0.0,
                height: 0.0,
            },
        };
        assert!(pick_valid_caret_rect(Some(bogus), None, Some(frame)).is_none());
    }

    #[test]
    fn ignored_write_detected_when_lengths_differ() {
        let snapshot = "Today are a good day.";
        let expected = "Today is a good day.";
        // Expected-length (20 units) read of the unchanged 21-unit field:
        // truncated to neither text, so the snapshot-length re-read decides.
        assert!(selected_write_ignored_reread(
            "Today are a good day",
            snapshot,
            snapshot,
            expected
        ));
        // A field changed to something else is a mismatch, not ignored.
        assert!(!selected_write_ignored_reread(
            "Today was a good day",
            "Today was a good day.",
            snapshot,
            expected
        ));
        // An accepted write is never "ignored".
        assert!(!selected_write_ignored_reread(
            expected, snapshot, snapshot, expected
        ));
    }

    #[test]
    fn input_area_prefers_the_scroll_area_parent() {
        let element = CGRect {
            origin: CGPoint { x: 200.0, y: 200.0 },
            size: CGSize {
                width: 300.0,
                height: 100.0,
            },
        };
        let parent = CGRect {
            origin: CGPoint { x: 100.0, y: 100.0 },
            size: CGSize {
                width: 500.0,
                height: 400.0,
            },
        };
        // A scroll-area parent wins over the element frame.
        let picked = choose_input_area(element, Some(("AXScrollArea", parent)));
        assert_eq!(picked.origin.x, 100.0);
        assert_eq!(picked.size.width, 500.0);
        // Any other parent role keeps the element frame.
        let kept = choose_input_area(element, Some(("AXWindow", parent)));
        assert_eq!(kept.origin.x, 200.0);
        // No parent keeps the element frame.
        let none = choose_input_area(element, None);
        assert_eq!(none.origin.x, 200.0);
        // Validation runs against the input area: a marker inside the
        // scroll area is valid even when outside the element frame.
        let marker = CGRect {
            origin: CGPoint { x: 150.0, y: 150.0 },
            size: CGSize {
                width: 1.0,
                height: 14.0,
            },
        };
        assert!(!caret_rect_valid(&marker, &element));
        assert!(caret_rect_valid(&marker, &parent));
    }
}
