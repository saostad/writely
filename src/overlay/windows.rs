use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use tao::event_loop::EventLoopProxy;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, SetBkMode,
    SetTextColor, DT_CENTER, DT_SINGLELINE, DT_VCENTER, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, IsWindow, RegisterClassW, SetWindowPos, ShowWindow,
    HWND_TOPMOST, MA_NOACTIVATE, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, WM_DESTROY,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_PAINT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::text_access::CaretRect;
use crate::UserEvent;

const ICON_SIZE: i32 = 20;

static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();
static REGISTERED: AtomicBool = AtomicBool::new(false);

struct IconState {
    window: HWND,
    x: i32,
    y: i32,
    visible: bool,
}

thread_local! {
    static STATE: RefCell<Option<IconState>> = const { RefCell::new(None) };
}

pub fn init(proxy: EventLoopProxy<UserEvent>) {
    if PROXY.set(proxy).is_err() {
        // Keep the first proxy installed for this process.
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn paint_icon(window: HWND) {
    // SAFETY: window is a live icon window on this thread.
    unsafe {
        let mut paint = std::mem::zeroed::<PAINTSTRUCT>();
        let dc = BeginPaint(window, &mut paint);
        if dc.is_invalid() {
            return;
        }
        let mut area = RECT {
            left: 0,
            top: 0,
            right: ICON_SIZE,
            bottom: ICON_SIZE,
        };
        let brush = CreateSolidBrush(COLORREF(0x0026_2626));
        if !brush.is_invalid() {
            FillRect(dc, &area, brush);
            // SAFETY: the brush was just created and is deleted here.
            if !DeleteObject(brush.into()).as_bool() {
                eprintln!("Writely: could not release the icon brush.");
            }
        }
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(0x00FF_FFFF));
        let mut glyph: Vec<u16> = "W".encode_utf16().collect();
        DrawTextW(
            dc,
            &mut glyph,
            &mut area,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        if !EndPaint(window, &paint).as_bool() {
            eprintln!("Writely: could not finish painting the icon.");
        }
    }
}

// SAFETY: the window and message parameters are provided by the system for
// the duration of each call; the handler only paints or forwards the click.
unsafe extern "system" fn icon_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONUP => {
            if let Some(proxy) = PROXY.get() {
                if let Err(error) = proxy.send_event(UserEvent::IconClicked) {
                    eprintln!("Writely: could not deliver the icon click event: {error}");
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            paint_icon(window);
            LRESULT(0)
        }
        WM_DESTROY => {
            STATE.with(|state| {
                *state.borrow_mut() = None;
            });
            // SAFETY: default handling for window teardown.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn ensure_window() -> Option<HWND> {
    if let Some(window) = STATE.with(|state| state.borrow().as_ref().map(|icon| icon.window)) {
        // SAFETY: window came from STATE.
        if unsafe { IsWindow(Some(window)) }.as_bool() {
            return Some(window);
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
                eprintln!("Writely: could not create the icon window: {error}");
                REGISTERED.store(false, Ordering::SeqCst);
                return None;
            }
        };
        let class_name = wide("WritelyIcon");
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(icon_proc),
            hInstance: HINSTANCE(module.0),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        // SAFETY: window_class points at a live WNDCLASSW with a valid proc.
        if unsafe { RegisterClassW(&window_class) } == 0 {
            eprintln!("Writely: could not create the icon window: RegisterClassW failed.");
            REGISTERED.store(false, Ordering::SeqCst);
            return None;
        }
    }
    // SAFETY: no instance handle is needed to get the current module handle.
    let module = match unsafe { GetModuleHandleW(PCWSTR::null()) } {
        Ok(module) => module,
        Err(error) => {
            eprintln!("Writely: could not create the icon window: {error}");
            return None;
        }
    };
    let class_name = wide("WritelyIcon");
    let title = wide("Writely");
    // SAFETY: the class was registered above and all strings outlive the call.
    // The window is created hidden and shown without activation on demand.
    match unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            ICON_SIZE,
            ICON_SIZE,
            None,
            None,
            Some(HINSTANCE(module.0)),
            None,
        )
    } {
        Ok(window) => {
            STATE.with(|state| {
                *state.borrow_mut() = Some(IconState {
                    window,
                    x: 0,
                    y: 0,
                    visible: false,
                });
            });
            Some(window)
        }
        Err(error) => {
            eprintln!("Writely: could not create the icon window: {error}");
            None
        }
    }
}

pub fn show_at(rect: &CaretRect) {
    // Caret coordinates are physical pixels; the window position is too.
    let x = (rect.x + rect.width + 4.0) as i32;
    let y = (rect.y + rect.height / 2.0 - ICON_SIZE as f64 / 2.0) as i32;
    let Some(window) = ensure_window() else {
        return;
    };
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(icon) = state.as_mut() else {
            return;
        };
        if icon.visible && icon.x == x && icon.y == y {
            return;
        }
        // SAFETY: window is a live icon window on this thread.
        if unsafe {
            SetWindowPos(
                window,
                Some(HWND_TOPMOST),
                x,
                y,
                ICON_SIZE,
                ICON_SIZE,
                SWP_NOACTIVATE,
            )
        }
        .is_err()
        {
            eprintln!("Writely: could not move the icon.");
            return;
        }
        if !icon.visible {
            // SAFETY: window is a live icon window on this thread.
            // SW_SHOWNOACTIVATE shows the icon without activating Writely.
            let _previously_visible = unsafe { ShowWindow(window, SW_SHOWNOACTIVATE) };
            icon.visible = true;
        }
        icon.x = x;
        icon.y = y;
    });
}

pub fn hide() {
    STATE.with(|state| {
        if let Some(icon) = state.borrow_mut().as_mut() {
            if icon.visible {
                // SAFETY: window is a live icon window on this thread.
                // ShowWindow reports the previous visibility, not success.
                let _previously_visible = unsafe { ShowWindow(icon.window, SW_HIDE) };
                icon.visible = false;
            }
        }
    });
}
