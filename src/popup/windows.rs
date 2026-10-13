use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use tao::event_loop::EventLoopProxy;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    DrawTextW, GetDC, GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SetBkMode, SetTextColor,
    DT_CALCRECT, DT_LEFT, DT_WORDBREAK, HDC, MONITORINFO, MONITOR_DEFAULTTONEAREST, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, IsWindow, RegisterClassW, SetWindowPos, SetWindowTextW,
    ShowWindow, BN_CLICKED, BS_PUSHBUTTON, HMENU, HWND_TOPMOST, MA_NOACTIVATE, SWP_NOACTIVATE,
    SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COMMAND,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_MOUSEACTIVATE, WNDCLASSW, WS_CHILD, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::text_access::CaretRect;
use crate::UserEvent;

const ID_REPLACE: i32 = 2001;
const MAX_TEXT_WIDTH: i32 = 360;
const PADDING: i32 = 10;
const GAP: i32 = 8;
const BUTTON_WIDTH: i32 = 90;
const BUTTON_HEIGHT: i32 = 24;
const ICON_SIZE: i32 = 20;

static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();
static REGISTERED: AtomicBool = AtomicBool::new(false);

struct PopupState {
    window: HWND,
    label: HWND,
    button: HWND,
    error: bool,
    visible: bool,
}

thread_local! {
    static STATE: RefCell<Option<PopupState>> = const { RefCell::new(None) };
}

pub fn init(proxy: EventLoopProxy<UserEvent>) {
    if PROXY.set(proxy).is_err() {
        // Keep the first proxy installed for this process.
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

extern "system" fn popup_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        // Never take activation or focus from the user's text field.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as i32;
            let code = ((wparam.0 >> 16) & 0xffff) as u32;
            if id == ID_REPLACE && code == BN_CLICKED {
                if let Some(proxy) = PROXY.get() {
                    if let Err(error) = proxy.send_event(UserEvent::ReplaceClicked) {
                        eprintln!("Writely: could not deliver the replace event: {error}");
                    }
                }
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            let target = HWND(lparam.0 as *mut core::ffi::c_void);
            let is_error_label = STATE.with(|state| {
                state
                    .borrow()
                    .as_ref()
                    .map(|popup| popup.label == target && popup.error)
                    .unwrap_or(false)
            });
            if is_error_label {
                // SAFETY: wparam is the STATIC control device context for this message.
                unsafe {
                    SetTextColor(
                        HDC(wparam.0 as *mut core::ffi::c_void),
                        COLORREF(0x000000FF),
                    );
                    SetBkMode(HDC(wparam.0 as *mut core::ffi::c_void), TRANSPARENT);
                }
            }
            // SAFETY: falling through to the default procedure is valid for this message.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        WM_DESTROY => {
            STATE.with(|state| {
                *state.borrow_mut() = None;
            });
            // SAFETY: default handling for window teardown.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        // SAFETY: all other messages use the default window procedure.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

/// Height of the wrapping label for `text`, measured with the label's own
/// device context so long suggestions wrap instead of clipping.
fn measure_text(label: HWND, text: &str) -> i32 {
    // SAFETY: label is a live child window on this thread.
    let dc = unsafe { GetDC(Some(label)) };
    if dc.is_invalid() {
        return 40;
    }
    let mut units: Vec<u16> = text.encode_utf16().collect();
    let mut area = RECT {
        left: 0,
        top: 0,
        right: MAX_TEXT_WIDTH,
        bottom: 0,
    };
    // SAFETY: dc is valid and area is writable for the CALCRECT measurement.
    let height = unsafe {
        DrawTextW(
            dc,
            &mut units,
            &mut area as *mut RECT,
            DT_LEFT | DT_WORDBREAK | DT_CALCRECT,
        )
    };
    // SAFETY: dc came from GetDC above and is released exactly once here.
    unsafe {
        ReleaseDC(Some(label), dc);
    }
    if height <= 0 {
        40
    } else {
        area.bottom - area.top
    }
}

fn ensure_window() -> Option<(HWND, HWND, HWND)> {
    if let Some((window, label, button)) = STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|popup| (popup.window, popup.label, popup.button))
    }) {
        // SAFETY: window came from STATE.
        if unsafe { IsWindow(Some(window)) }.as_bool() {
            return Some((window, label, button));
        }
        STATE.with(|state| {
            *state.borrow_mut() = None;
        });
    }
    if !REGISTERED.swap(true, Ordering::SeqCst) {
        // SAFETY: no instance handle is needed to get the current module handle.
        let module = match unsafe { GetModuleHandleW(PCWSTR::null()) } {
            Ok(module) => module,
            Err(error) => {
                eprintln!("Writely: could not create the suggestion popup: {error}");
                REGISTERED.store(false, Ordering::SeqCst);
                return None;
            }
        };
        let class_name = wide("WritelyPopup");
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(popup_proc),
            hInstance: HINSTANCE(module.0),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        // SAFETY: window_class points at a live WNDCLASSW with a valid proc.
        if unsafe { RegisterClassW(&window_class) } == 0 {
            eprintln!("Writely: could not create the suggestion popup: RegisterClassW failed.");
            REGISTERED.store(false, Ordering::SeqCst);
            return None;
        }
    }
    // SAFETY: no instance handle is needed to get the current module handle.
    let module = match unsafe { GetModuleHandleW(PCWSTR::null()) } {
        Ok(module) => module,
        Err(error) => {
            eprintln!("Writely: could not create the suggestion popup: {error}");
            return None;
        }
    };
    let instance = HINSTANCE(module.0);
    let class_name = wide("WritelyPopup");
    let title = wide("Writely");
    // SAFETY: the class was registered above and all strings outlive the call.
    // The window is created hidden and shown without activation on demand.
    let window = match unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            MAX_TEXT_WIDTH + 2 * PADDING,
            100,
            None,
            None,
            Some(instance),
            None,
        )
    } {
        Ok(window) => window,
        Err(error) => {
            eprintln!("Writely: could not create the suggestion popup: {error}");
            return None;
        }
    };
    let make_child = |class: &str, text: &str, style: WINDOW_STYLE, id: i32| -> Option<HWND> {
        let class_wide = wide(class);
        let text_wide = wide(text);
        // SAFETY: class/text strings outlive the call and the parent is live.
        match unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class_wide.as_ptr()),
                PCWSTR(text_wide.as_ptr()),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                10,
                10,
                Some(window),
                Some(HMENU(id as *mut core::ffi::c_void)),
                Some(instance),
                None,
            )
        } {
            Ok(child) => Some(child),
            Err(error) => {
                eprintln!("Writely: could not create the suggestion popup: {error}");
                None
            }
        }
    };
    // A plain STATIC wraps at word boundaries by default.
    let label = make_child("STATIC", "", WINDOW_STYLE(0), 0)?;
    let button = make_child(
        "BUTTON",
        "Replace",
        WINDOW_STYLE(BS_PUSHBUTTON as u32),
        ID_REPLACE,
    )?;
    STATE.with(|state| {
        *state.borrow_mut() = Some(PopupState {
            window,
            label,
            button,
            error: false,
            visible: false,
        });
    });
    Some((window, label, button))
}

pub fn show_message(text: &str, is_error: bool, show_button: bool, caret: &CaretRect) {
    let Some((window, label, button)) = ensure_window() else {
        return;
    };
    let text_height = measure_text(label, text).max(17);
    let width = MAX_TEXT_WIDTH + 2 * PADDING;
    let mut height = PADDING + text_height + PADDING;
    if show_button {
        height += GAP + BUTTON_HEIGHT;
    }
    let value = wide(text);
    // SAFETY: label is a live STATIC child window.
    if let Err(error) = unsafe { SetWindowTextW(label, PCWSTR(value.as_ptr())) } {
        eprintln!("Writely: could not update the suggestion popup: {error}");
        return;
    }
    // SAFETY: all handles are live child windows on this thread.
    unsafe {
        // SetWindowPos on a child moves it without activating anything.
        let _ = SetWindowPos(
            label,
            None,
            PADDING,
            PADDING,
            MAX_TEXT_WIDTH,
            text_height,
            SWP_NOACTIVATE,
        );
        let _ = SetWindowPos(
            button,
            None,
            width - PADDING - BUTTON_WIDTH,
            PADDING + text_height + GAP,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
            SWP_NOACTIVATE,
        );
        let _previously_visible = ShowWindow(button, if show_button { SW_SHOW } else { SW_HIDE });
    }
    // 4 pt below the icon; the icon sits 4 pt right of the caret.
    // Caret coordinates are physical pixels; the window position is too.
    let icon_top = caret.y + caret.height / 2.0 - ICON_SIZE as f64 / 2.0;
    let mut x = (caret.x + caret.width + 4.0) as i32;
    let mut y = (icon_top + ICON_SIZE as f64 + 4.0) as i32;
    // SAFETY: POINT is plain integers for the monitor lookup below.
    let point = POINT { x, y };
    // SAFETY: point is valid and info is writable with cbSize set.
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let work = unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            RECT {
                left: x,
                top: y,
                right: x + width,
                bottom: y + height,
            }
        }
    };
    x = x.clamp(work.left, (work.right - width).max(work.left));
    if y + height > work.bottom {
        // No room below: 4 pt above the caret line.
        y = (caret.y - 4.0) as i32 - height;
    }
    y = y.clamp(work.top, (work.bottom - height).max(work.top));
    // SAFETY: window is a live popup window on this thread.
    if unsafe {
        SetWindowPos(
            window,
            Some(HWND_TOPMOST),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE,
        )
    }
    .is_err()
    {
        eprintln!("Writely: could not move the suggestion popup.");
        return;
    }
    STATE.with(|state| {
        if let Some(popup) = state.borrow_mut().as_mut() {
            // The window and its controls are kept alive and reused; they are
            // never re-created.
            popup.error = is_error;
            if !popup.visible {
                // SAFETY: window is a live popup window on this thread.
                // SW_SHOWNOACTIVATE shows it without activating Writely.
                let _previously_visible = unsafe { ShowWindow(window, SW_SHOWNOACTIVATE) };
                popup.visible = true;
            }
        }
    });
}

pub fn hide() {
    STATE.with(|state| {
        if let Some(popup) = state.borrow_mut().as_mut() {
            if popup.visible {
                // SAFETY: window is a live popup window on this thread.
                // ShowWindow reports the previous visibility, not success.
                let _previously_visible = unsafe { ShowWindow(popup.window, SW_HIDE) };
                popup.visible = false;
            }
        }
    });
}

pub fn is_visible() -> bool {
    STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|popup| popup.visible)
            .unwrap_or(false)
    })
}
