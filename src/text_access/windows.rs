use std::ops::Range;
use std::sync::Once;

use uiautomation::core::{UIAutomation, UIElement};
use uiautomation::patterns::{UITextPattern, UIValuePattern};
use uiautomation::types::TextPatternRangeEndpoint;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound,
    SafeArrayUnaccessData,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTextPattern2,
    UIA_TextPattern2Id,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, GUITHREADINFO,
};

use super::{replaced_text, CaretInfo, CaretRect, FieldSnapshot};

pub(crate) struct PlatformHandle {
    element: UIElement,
}

static COM_INIT: Once = Once::new();

fn ensure_com() {
    COM_INIT.call_once(|| {
        // SAFETY: one-time COM reservation on this thread, same concurrency
        // model as the uiautomation crate. If the UI stack already owns COM,
        // the calls below still work and surface their own errors.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    });
}

fn read_error(error: impl std::fmt::Display) -> String {
    format!("Could not read the focused text field: {error}")
}

pub(crate) fn check_permission(_prompt: bool) -> Result<(), String> {
    Ok(())
}

fn uia_client() -> Result<IUIAutomation, String> {
    ensure_com();
    // SAFETY: creating the in-process UI Automation client takes no borrowed state.
    unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL) }.map_err(read_error)
}

fn focused_uia_element(
    automation: &IUIAutomation,
) -> Result<Option<(IUIAutomationElement, u32)>, String> {
    // SAFETY: the automation client is a live in-process COM object.
    let element = unsafe { automation.GetFocusedElement() }.map_err(read_error)?;
    // SAFETY: the element is a live COM object from the call above.
    let element_pid = unsafe { element.CurrentProcessId() }.map_err(read_error)?;
    if element_pid < 0 || element_pid as u32 == std::process::id() {
        return Ok(None);
    }
    // SAFETY: the element is a live COM object from the call above.
    match unsafe { element.CurrentIsPassword() } {
        Ok(is_password) if is_password.as_bool() => Ok(None),
        Ok(_) => Ok(Some((element, element_pid as u32))),
        Err(error) => Err(read_error(error)),
    }
}

/// Releases the SAFEARRAY exactly once when the guard drops.
struct SafeArrayGuard(*mut SAFEARRAY);

impl Drop for SafeArrayGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the array came from GetBoundingRectangles and has not
            // been destroyed yet.
            if let Err(error) = unsafe { SafeArrayDestroy(self.0) } {
                eprintln!("Writely: could not release cursor rectangles: {error}");
            }
        }
    }
}

/// Caret rectangle from the UIA caret range (physical pixels), or `Ok(None)`
/// when the focused element has no text caret.
fn uia_caret_rect(element: &IUIAutomationElement, pid: u32) -> Result<Option<CaretInfo>, String> {
    // SAFETY: the element is a live COM object.
    let pattern: IUIAutomationTextPattern2 =
        match unsafe { element.GetCurrentPatternAs(UIA_TextPattern2Id) } {
            Ok(pattern) => pattern,
            // Not a text field (e.g. a focused button): no caret to follow.
            Err(_) => return Ok(None),
        };
    let mut active = BOOL(0);
    // SAFETY: the pattern is a live COM object and active is a writable flag.
    let range = unsafe { pattern.GetCaretRange(&mut active) }.map_err(read_error)?;
    if !active.as_bool() {
        return Ok(None);
    }
    // SAFETY: the range is a live COM object.
    let raw = unsafe { range.GetBoundingRectangles() }.map_err(read_error)?;
    if raw.is_null() {
        return Ok(None);
    }
    let _guard = SafeArrayGuard(raw);
    // SAFETY: raw is a valid SAFEARRAY while the guard is alive.
    let lower = unsafe { SafeArrayGetLBound(raw, 1) }.map_err(read_error)?;
    // SAFETY: raw is a valid SAFEARRAY while the guard is alive.
    let upper = unsafe { SafeArrayGetUBound(raw, 1) }.map_err(read_error)?;
    let count = (upper - lower + 1).max(0) as usize;
    if count < 4 {
        return Ok(None);
    }
    let mut data: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: raw is a valid SAFEARRAY while the guard is alive, and data
    // receives the locked pointer, released below before the guard drops.
    unsafe { SafeArrayAccessData(raw, &mut data) }.map_err(read_error)?;
    // SAFETY: UIA bounding rectangles are doubles; count was checked above.
    let values: &[f64] = unsafe { std::slice::from_raw_parts(data as *const f64, count) };
    let (x, y, width, height) = (values[0], values[1], values[2], values[3]);
    // SAFETY: the array was locked above and is still owned by the guard.
    unsafe { SafeArrayUnaccessData(raw) }.map_err(read_error)?;
    Ok(Some(CaretInfo {
        pid,
        caret_rect: CaretRect {
            x,
            y,
            width: width.max(0.0),
            height: height.max(0.0),
            physical: true,
        },
    }))
}

fn document_text(element: &UIElement) -> Result<String, String> {
    let pattern = element.get_pattern::<UITextPattern>().map_err(read_error)?;
    let document = pattern.get_document_range().map_err(read_error)?;
    document.get_text(-1).map_err(read_error)
}

pub(crate) fn focused_field() -> Result<Option<FieldSnapshot>, String> {
    let automation = UIAutomation::new().map_err(read_error)?;
    let element = automation.get_focused_element().map_err(read_error)?;

    let pid = element.get_process_id().map_err(read_error)?;
    if pid == std::process::id() {
        return Ok(None);
    }
    match element.is_password() {
        Ok(true) => return Ok(None),
        Ok(false) => {}
        Err(error) => return Err(read_error(error)),
    }

    let pattern = match element.get_pattern::<UITextPattern>() {
        Ok(pattern) => pattern,
        Err(_) => return Ok(None),
    };
    let document = pattern.get_document_range().map_err(read_error)?;
    let text = document.get_text(-1).map_err(read_error)?;

    // The first selection range gives both the caret (its start) and the
    // selection (start..end). A failure to read it is an error for the
    // popup, not a silent "no selection".
    let (caret, selection) = match pattern.get_selection().map_err(read_error)? {
        selections => match selections.into_iter().next() {
            Some(selection) => {
                let before_start = document.clone();
                before_start
                    .move_endpoint_by_range(
                        TextPatternRangeEndpoint::End,
                        &selection,
                        TextPatternRangeEndpoint::Start,
                    )
                    .map_err(read_error)?;
                let start_text = before_start.get_text(-1).map_err(read_error)?;
                let start = start_text.chars().count();
                let before_end = document.clone();
                before_end
                    .move_endpoint_by_range(
                        TextPatternRangeEndpoint::End,
                        &selection,
                        TextPatternRangeEndpoint::End,
                    )
                    .map_err(read_error)?;
                let end_text = before_end.get_text(-1).map_err(read_error)?;
                let end = end_text.chars().count();
                let char_count = text.chars().count();
                let start = start.min(char_count);
                let end = end.min(char_count);
                let range = if end > start { Some(start..end) } else { None };
                (start, range)
            }
            None => (0, None),
        },
    };

    // The caret rectangle comes from the foreground thread / UIA caret range
    // (no element-rectangle approximation). Unknown position means the icon
    // has nothing to anchor to.
    let caret_rect = match caret_info()? {
        Some(info) => info.caret_rect,
        None => return Ok(None),
    };

    // Bounded reads are a later round on Windows; the snapshot covers the
    // whole field, so the whole-field write fallback stays allowed.
    let field_len_utf16 = super::utf16_len(&text);
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

/// Light caret query for keystroke bursts: no document text is read here.
/// Coordinates are physical pixels (`physical: true`).
pub(crate) fn caret_info() -> Result<Option<CaretInfo>, String> {
    // SAFETY: querying the foreground window takes no borrowed state.
    let foreground: HWND = unsafe { GetForegroundWindow() };
    if foreground.is_invalid() {
        return Ok(None);
    }
    let mut foreground_pid: u32 = 0;
    // SAFETY: foreground is a live window and foreground_pid is writable.
    let foreground_thread =
        unsafe { GetWindowThreadProcessId(foreground, Some(&mut foreground_pid)) };
    if foreground_pid == std::process::id() {
        return Ok(None);
    }

    let automation = uia_client()?;
    let Some((element, pid)) = focused_uia_element(&automation)? else {
        return Ok(None);
    };

    // SAFETY: GUITHREADINFO is plain integers; cbSize is set before the call.
    let mut thread_info: GUITHREADINFO = unsafe { std::mem::zeroed() };
    thread_info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
    // SAFETY: thread_info is writable with cbSize set.
    if unsafe { GetGUIThreadInfo(foreground_thread, &mut thread_info) }.is_ok()
        && !thread_info.hwndCaret.is_invalid()
        && (thread_info.rcCaret.right > thread_info.rcCaret.left
            || thread_info.rcCaret.bottom > thread_info.rcCaret.top)
    {
        let mut top_left = POINT {
            x: thread_info.rcCaret.left,
            y: thread_info.rcCaret.top,
        };
        let mut bottom_right = POINT {
            x: thread_info.rcCaret.right,
            y: thread_info.rcCaret.bottom,
        };
        // SAFETY: both points are writable and hwndCaret is a live window.
        let mapped_top = unsafe { ClientToScreen(thread_info.hwndCaret, &mut top_left) }.as_bool();
        // SAFETY: both points are writable and hwndCaret is a live window.
        let mapped_bottom =
            unsafe { ClientToScreen(thread_info.hwndCaret, &mut bottom_right) }.as_bool();
        if mapped_top && mapped_bottom {
            return Ok(Some(CaretInfo {
                pid,
                caret_rect: CaretRect {
                    x: top_left.x as f64,
                    y: top_left.y as f64,
                    width: (bottom_right.x - top_left.x).max(0) as f64,
                    height: (bottom_right.y - top_left.y).max(0) as f64,
                    physical: true,
                },
            }));
        }
    }
    uia_caret_rect(&element, pid)
}

pub(crate) fn read_text(handle: &PlatformHandle) -> Result<String, String> {
    document_text(&handle.element)
}

pub(crate) fn replace_range(
    snap: &FieldSnapshot,
    range: Range<usize>,
    new_text: &str,
) -> Result<(), String> {
    let pattern = match snap.handle.element.get_pattern::<UIValuePattern>() {
        Ok(pattern) => pattern,
        Err(_) => {
            return Err("This app's text field doesn't let Writely replace text.".to_string());
        }
    };
    match pattern.is_readonly() {
        Ok(true) => {
            return Err("This app's text field doesn't let Writely replace text.".to_string());
        }
        Ok(false) => {}
        Err(error) => return Err(format!("Could not replace the text: {error}")),
    }
    let expected = replaced_text(&snap.text, &range, new_text);
    pattern
        .set_value(&expected)
        .map_err(|error| format!("Could not replace the text: {error}"))?;
    let actual = read_text(&snap.handle)?;
    if actual != expected {
        return Err("The app didn't accept the replacement text.".to_string());
    }
    Ok(())
}
