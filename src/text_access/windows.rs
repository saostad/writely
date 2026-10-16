use std::ops::Range;
use std::sync::Once;

use uiautomation::core::{UIAutomation, UIElement};
use uiautomation::patterns::{UITextPattern, UIValuePattern};
use uiautomation::types::{TextPatternRangeEndpoint, TextUnit};
use windows::core::{Interface, BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, POINT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound,
    SafeArrayUnaccessData,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Accessibility::{
    AccessibleObjectFromWindow, CUIAutomation, IAccessible, IUIAutomation, IUIAutomationElement,
    IUIAutomationTextPattern, IUIAutomationTextPattern2, IUIAutomationTextRange,
    TextPatternRangeEndpoint_Start, TextUnit_Character, UIA_TextPattern2Id, UIA_TextPatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, CHILDID_SELF, GUITHREADINFO,
    OBJID_CARET,
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

/// Whether an MSAA caret rectangle is usable: it must have extent and
/// must not be the (0, 0) placeholder some apps report when there is no
/// caret. A zero width is fine (Chrome reports a 0-wide caret); a caret
/// on a monitor left of the primary one has a negative x and is usable.
fn usable_msaa_caret(x: i32, y: i32, width: i32, height: i32) -> bool {
    width + height > 0 && !(x == 0 && y == 0)
}

/// Whether `exe` is a terminal emulator, where a mouse selection is a
/// screen selection and Replace cannot work. Case-insensitive on the
/// exe file name. Pure for testing.
pub(crate) fn is_terminal_exe(exe: &str) -> bool {
    let name = exe.rsplit(['\\', '/']).next().unwrap_or(exe);
    [
        "windowsterminal.exe",
        "openconsole.exe",
        "conhost.exe",
        "cmd.exe",
        "powershell.exe",
        "pwsh.exe",
        "warp.exe",
        "wezterm-gui.exe",
        "alacritty.exe",
        "mintty.exe",
        "hyper.exe",
        "tabby.exe",
        "rio.exe",
        "ghostty.exe",
    ]
    .iter()
    .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// Whether UIA text/selection for this app is unreliable, so the caller
/// should skip accessibility and go straight to copy-and-paste: the
/// listed editors/office apps, every terminal, or any Chromium
/// EditContext editor (`native-edit-context`). Case-insensitive on the
/// exe file name. Pure for testing.
fn is_unreliable_uia(exe: &str, class_name: &str) -> bool {
    if class_name.eq_ignore_ascii_case("native-edit-context") {
        return true;
    }
    if is_terminal_exe(exe) {
        return true;
    }
    let name = exe.rsplit(['\\', '/']).next().unwrap_or(exe);
    [
        "code.exe",
        "code - insiders.exe",
        "cursor.exe",
        "windsurf.exe",
        "sublime_text.exe",
        "firefox.exe",
        "winword.exe",
        "powerpnt.exe",
        "idea64.exe",
        "pycharm64.exe",
        "webstorm64.exe",
        "rider64.exe",
        "clion64.exe",
        "goland64.exe",
        "phpstorm64.exe",
        "rubymine64.exe",
        "datagrip64.exe",
        "studio64.exe",
    ]
    .iter()
    .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// The exe file name (not the full path) of process `pid`.
pub(crate) fn process_exe_name(pid: u32) -> Result<String, String> {
    // SAFETY: opening the process with query-only access takes a plain pid.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|error| format!("Could not read the app name for process {pid}: {error}"))?;
    let mut buffer = vec![0u16; 1024];
    let mut size = buffer.len() as u32;
    // SAFETY: handle is live, buffer is writable, and size is in/out.
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };
    // SAFETY: handle was opened above and is closed exactly once here.
    if let Err(error) = unsafe { CloseHandle(handle) } {
        eprintln!("Writely: could not close a process handle: {error}");
    }
    result.map_err(|error| format!("Could not read the app name for process {pid}: {error}"))?;
    buffer.truncate(size as usize);
    let path = String::from_utf16_lossy(&buffer);
    let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
    if name.is_empty() {
        Err(format!(
            "Could not read the app name for process {pid}: the name was empty"
        ))
    } else {
        Ok(name)
    }
}

/// Whether process `pid` is a terminal app. A failed exe lookup is
/// logged and treated as not a terminal.
pub(crate) fn is_terminal_app(pid: u32) -> bool {
    match process_exe_name(pid) {
        Ok(exe) => is_terminal_exe(&exe),
        Err(error) => {
            eprintln!("Writely: {error}");
            false
        }
    }
}

/// Caret rectangle from MSAA `OBJID_CARET` (screen physical pixels), or
/// `Ok(None)` when the accessible object reports no usable caret.
/// Callers treat an `Err` as "try the next step", not as fatal.
fn msaa_caret_rect(hwnd: HWND, pid: u32) -> Result<Option<CaretInfo>, String> {
    ensure_com();
    let mut raw: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: hwnd is a live window, the IID identifies IAccessible, and
    // raw receives the owned interface pointer on success.
    unsafe { AccessibleObjectFromWindow(hwnd, OBJID_CARET.0 as u32, &IAccessible::IID, &mut raw) }
        .map_err(read_error)?;
    if raw.is_null() {
        return Ok(None);
    }
    // SAFETY: raw is the owned IAccessible pointer from the call above.
    let accessible: IAccessible = unsafe { IAccessible::from_raw(raw) };
    let mut child = VARIANT::default();
    // SAFETY: child is a zeroed VARIANT; setting its tag and i32 payload
    // to VT_I4 / CHILDID_SELF matches what accLocation expects.
    unsafe {
        (*child.Anonymous.Anonymous).vt = VT_I4;
        (*child.Anonymous.Anonymous).Anonymous.lVal = CHILDID_SELF as i32;
    }
    let (mut left, mut top, mut width, mut height) = (0i32, 0i32, 0i32, 0i32);
    // SAFETY: accessible is a live COM object, the four out-parameters are
    // writable, and child is the VARIANT built above.
    unsafe { accessible.accLocation(&mut left, &mut top, &mut width, &mut height, &child) }
        .map_err(read_error)?;
    if !usable_msaa_caret(left, top, width, height) {
        return Ok(None);
    }
    Ok(Some(CaretInfo {
        pid,
        caret_rect: CaretRect {
            x: left as f64,
            y: top as f64,
            width: width.max(0) as f64,
            height: height.max(0) as f64,
            physical: true,
        },
    }))
}

/// Bounding rectangles of a text range as flat `[x, y, width, height, ...]`
/// groups (empty when the range has no rectangle, e.g. a collapsed caret).
fn range_rects(range: &IUIAutomationTextRange) -> Result<Vec<f64>, String> {
    // SAFETY: the range is a live COM object.
    let raw = unsafe { range.GetBoundingRectangles() }.map_err(read_error)?;
    if raw.is_null() {
        return Ok(Vec::new());
    }
    let _guard = SafeArrayGuard(raw);
    // SAFETY: raw is a valid SAFEARRAY while the guard is alive.
    let lower = unsafe { SafeArrayGetLBound(raw, 1) }.map_err(read_error)?;
    // SAFETY: raw is a valid SAFEARRAY while the guard is alive.
    let upper = unsafe { SafeArrayGetUBound(raw, 1) }.map_err(read_error)?;
    let count = (upper - lower + 1).max(0) as usize;
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut data: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: raw is a valid SAFEARRAY while the guard is alive, and data
    // receives the locked pointer, released below before the guard drops.
    unsafe { SafeArrayAccessData(raw, &mut data) }.map_err(read_error)?;
    // SAFETY: UIA bounding rectangles are doubles; count is the element count.
    let values: &[f64] = unsafe { std::slice::from_raw_parts(data as *const f64, count) };
    let result = values.to_vec();
    // SAFETY: the array was locked above and is still owned by the guard.
    unsafe { SafeArrayUnaccessData(raw) }.map_err(read_error)?;
    Ok(result)
}

/// Caret at the right edge of the last rectangle in `values` (flat
/// `[x, y, width, height, ...]` groups), 1 px wide. `None` when empty.
fn caret_at_right_edge(values: &[f64], pid: u32) -> Option<CaretInfo> {
    if values.len() < 4 {
        return None;
    }
    let last = values.len() - 4;
    let (x, y, width, height) = (
        values[last],
        values[last + 1],
        values[last + 2],
        values[last + 3],
    );
    Some(CaretInfo {
        pid,
        caret_rect: CaretRect {
            x: x + width.max(0.0),
            y,
            width: 1.0,
            height: height.max(0.0),
            physical: true,
        },
    })
}

/// Caret from UIA TextPattern (v1) selection, for apps without
/// TextPattern2. The caret sits at the end of the selection; a collapsed
/// selection has no rectangle, so the character before it is measured
/// and its right edge is used instead.
fn text_pattern_caret(
    element: &IUIAutomationElement,
    pid: u32,
) -> Result<Option<CaretInfo>, String> {
    // SAFETY: the element is a live COM object.
    let pattern: IUIAutomationTextPattern =
        match unsafe { element.GetCurrentPatternAs(UIA_TextPatternId) } {
            Ok(pattern) => pattern,
            // Not a text field (e.g. a focused button): no caret to follow.
            Err(_) => return Ok(None),
        };
    // SAFETY: the pattern is a live COM object.
    let selections = unsafe { pattern.GetSelection() }.map_err(read_error)?;
    // SAFETY: the selection array is a live COM object.
    let length = unsafe { selections.Length() }.map_err(read_error)?;
    if length <= 0 {
        return Ok(None);
    }
    // SAFETY: the selection array is live and index 0 is within its length.
    let range = unsafe { selections.GetElement(0) }.map_err(read_error)?;
    let values = range_rects(&range)?;
    if let Some(info) = caret_at_right_edge(&values, pid) {
        return Ok(Some(info));
    }
    // Empty rectangles: measure the character before the caret.
    // SAFETY: the range is a live COM object.
    let previous = unsafe { range.Clone() }.map_err(read_error)?;
    // SAFETY: the cloned range is a live COM object.
    unsafe { previous.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -1) }
        .map_err(read_error)?;
    let values = range_rects(&previous)?;
    Ok(caret_at_right_edge(&values, pid))
}

/// Caret rectangle from the UIA caret range (physical pixels), or `Ok(None)`
/// when the focused element has no text caret.
fn uia_caret_rect(element: &IUIAutomationElement, pid: u32) -> Result<Option<CaretInfo>, String> {
    // SAFETY: the element is a live COM object.
    if let Ok(pattern) =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPattern2Id) }
    {
        let mut active = BOOL(0);
        // SAFETY: the pattern is a live COM object and active is a writable flag.
        if let Ok(range) = unsafe { pattern.GetCaretRange(&mut active) } {
            if active.as_bool() {
                let values = range_rects(&range)?;
                if values.len() >= 4 {
                    let (x, y, width, height) = (values[0], values[1], values[2], values[3]);
                    return Ok(Some(CaretInfo {
                        pid,
                        caret_rect: CaretRect {
                            x,
                            y,
                            width: width.max(0.0),
                            height: height.max(0.0),
                            physical: true,
                        },
                    }));
                }
            }
        }
    }
    // No TextPattern2 caret (missing pattern, inactive, or no rectangle):
    // fall back to the TextPattern (v1) selection as the last step.
    text_pattern_caret(element, pid)
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

    // Apps whose UIA text/selection isn't the real editor content go
    // straight to the copy-and-paste fallback (`Ok(None)` here).
    let class_name = match element.get_classname() {
        Ok(class_name) => class_name,
        Err(error) => {
            eprintln!("Writely: {}", read_error(error));
            String::new()
        }
    };
    match process_exe_name(pid) {
        Ok(exe) if is_unreliable_uia(&exe, &class_name) => return Ok(None),
        Ok(_) => {}
        Err(error) => eprintln!("Writely: {error}"),
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
    let selections = pattern.get_selection().map_err(read_error)?;
    let (caret, selection) = match selections.into_iter().next() {
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
    let gui_ok = unsafe { GetGUIThreadInfo(foreground_thread, &mut thread_info) }.is_ok();
    if gui_ok
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
    // MSAA caret: Chrome and VS Code expose no system or UIA caret, but
    // their caret is reachable as OBJID_CARET. A failure here only means
    // "try UIA next"; the UIA result (or its error) decides the outcome.
    let msaa_hwnd = if gui_ok && !thread_info.hwndFocus.is_invalid() {
        thread_info.hwndFocus
    } else {
        foreground
    };
    if let Ok(Some(info)) = msaa_caret_rect(msaa_hwnd, pid) {
        return Ok(Some(info));
    }
    uia_caret_rect(&element, pid)
}

/// Whether the currently focused element is a secure (password) field.
/// Unknown or unreadable focus counts as secure: the copy-and-paste fallback
/// must never run for a password field.
pub(crate) fn focused_is_secure() -> bool {
    let automation = match uia_client() {
        Ok(automation) => automation,
        Err(error) => {
            eprintln!("Writely: {error}");
            return true;
        }
    };
    // SAFETY: the automation client is a live in-process COM object.
    let element = match unsafe { automation.GetFocusedElement() } {
        Ok(element) => element,
        Err(error) => {
            eprintln!("Writely: {}", read_error(error));
            return true;
        }
    };
    // SAFETY: the element is a live COM object from the call above.
    match unsafe { element.CurrentIsPassword() } {
        Ok(is_password) => is_password.as_bool(),
        Err(error) => {
            eprintln!("Writely: {}", read_error(error));
            true
        }
    }
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

/// Selects `range` (char indices into `snap.text`) in the field through
/// UI Automation and verifies the selection reads back as the snapshot
/// text for that range, so a paste replaces exactly the checked text.
pub(crate) fn select_range(snap: &FieldSnapshot, range: Range<usize>) -> Result<(), String> {
    let char_count = snap.text.chars().count();
    let expected = selected_text_for_range(&snap.text, &range)?;
    let pattern = snap
        .handle
        .element
        .get_pattern::<UITextPattern>()
        .map_err(|error| format!("Could not select the text to replace: {error}"))?;
    let document = pattern
        .get_document_range()
        .map_err(|error| format!("Could not select the text to replace: {error}"))?;
    let target = document.clone();
    target
        .move_endpoint_by_unit(
            TextPatternRangeEndpoint::Start,
            TextUnit::Character,
            range.start as i32,
        )
        .map_err(|error| format!("Could not select the text to replace: {error}"))?;
    let after_end = char_count - range.end;
    target
        .move_endpoint_by_unit(
            TextPatternRangeEndpoint::End,
            TextUnit::Character,
            -(after_end as i32),
        )
        .map_err(|error| format!("Could not select the text to replace: {error}"))?;
    target
        .select()
        .map_err(|error| format!("Could not select the text to replace: {error}"))?;
    let selections = pattern
        .get_selection()
        .map_err(|error| format!("Could not select the text to replace: {error}"))?;
    let selected = match selections.into_iter().next() {
        Some(selection) => selection
            .get_text(-1)
            .map_err(|error| format!("Could not select the text to replace: {error}"))?,
        None => {
            return Err(
                "Could not select the text to replace: the app reported no selection.".to_string(),
            )
        }
    };
    if selected != expected {
        return Err(
            "Could not select the text to replace: the selected text doesn't match the checked text."
                .to_string(),
        );
    }
    Ok(())
}

/// The snapshot text for `range` (char indices), or an error when the range
/// is out of bounds. Pure so the char-index (not byte-index) slicing and
/// the bounds check can be tested without a live field.
fn selected_text_for_range(text: &str, range: &Range<usize>) -> Result<String, String> {
    let char_count = text.chars().count();
    if range.start > range.end || range.end > char_count {
        return Err(
            "Could not select the text to replace: the range is out of bounds.".to_string(),
        );
    }
    Ok(text
        .chars()
        .skip(range.start)
        .take(range.end - range.start)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{is_terminal_exe, is_unreliable_uia, selected_text_for_range, usable_msaa_caret};

    #[test]
    fn msaa_caret_accepts_only_rects_with_extent_off_the_origin() {
        // The two live probes from Chrome and VS Code.
        assert!(usable_msaa_caret(390, 224, 0, 27));
        assert!(usable_msaa_caret(767, 252, 1, 35));
        assert!(!usable_msaa_caret(0, 0, 0, 0));
        assert!(!usable_msaa_caret(0, 0, 1, 20));
        // A caret on a monitor left of the primary one.
        assert!(usable_msaa_caret(-1200, 300, 1, 20));
    }

    #[test]
    fn unreliable_uia_matches_listed_exes_class_and_terminals() {
        assert!(is_unreliable_uia("Code.exe", ""));
        assert!(is_unreliable_uia("CODE.EXE", ""));
        assert!(!is_unreliable_uia("notepad.exe", "RichEditD2DPT"));
        assert!(is_unreliable_uia("chrome.exe", "native-edit-context"));
        assert!(!is_unreliable_uia("chrome.exe", ""));
        assert!(is_unreliable_uia("warp.exe", ""));
    }

    #[test]
    fn terminal_exes_are_case_insensitive() {
        assert!(is_terminal_exe("Warp.exe"));
        assert!(is_terminal_exe("WindowsTerminal.exe"));
        assert!(!is_terminal_exe("Code.exe"));
    }

    #[test]
    fn selected_text_uses_char_indices_not_bytes() {
        // The emoji is one char but four bytes: a byte slice would split it.
        let text = "a\u{1F600} bad day.";
        assert_eq!(selected_text_for_range(text, &(2..6)).unwrap(), " bad");
        assert_eq!(
            selected_text_for_range(text, &(0..2)).unwrap(),
            "a\u{1F600}"
        );
    }

    #[test]
    fn selected_text_rejects_out_of_bounds_ranges() {
        let text = "short";
        assert!(selected_text_for_range(text, &(0..6)).is_err());
        assert!(selected_text_for_range(text, &std::ops::Range { start: 3, end: 2 }).is_err());
        assert_eq!(selected_text_for_range(text, &(5..5)).unwrap(), "");
    }
}
