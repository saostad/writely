use std::cell::RefCell;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{SetBkMode, SetTextColor, HDC, TRANSPARENT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::EM_SETCUEBANNER;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowTextLengthW, GetWindowTextW, IsWindow,
    RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowTextW, ShowWindow, BS_PUSHBUTTON,
    ES_PASSWORD, HMENU, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLORSTATIC, WM_DESTROY, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_OVERLAPPED, WS_SYSMENU,
    WS_VISIBLE,
};

const ID_EDIT: i32 = 1001;
const ID_SAVE: i32 = 1002;
const ID_REMOVE: i32 = 1003;
const ID_LINK: i32 = 1004;

struct Handles {
    window: HWND,
    edit: HWND,
    status: HWND,
    error: bool,
}

thread_local! {
    static STATE: RefCell<Option<Handles>> = const { RefCell::new(None) };
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn set_status(text: &str, is_error: bool) {
    STATE.with(|state| {
        if let Some(handles) = state.borrow_mut().as_mut() {
            handles.error = is_error;
            let value = wide(text);
            // SAFETY: handles.status is a live STATIC while the window is open.
            if let Err(error) = unsafe { SetWindowTextW(handles.status, PCWSTR(value.as_ptr())) } {
                eprintln!("Writely: could not update the Settings status: {error}");
            }
        }
    });
}

fn refresh_cue_banner(edit: HWND) {
    let has_key = match super::has_api_key() {
        Ok(has_key) => has_key,
        Err(error) => {
            set_status(&error, true);
            false
        }
    };
    let cue = wide(if has_key {
        "••••••••"
    } else {
        "Paste your key here"
    });
    // SAFETY: edit is a live EDIT control and cue outlives the call.
    unsafe {
        SendMessageW(
            edit,
            EM_SETCUEBANNER,
            WPARAM(0),
            LPARAM(cue.as_ptr() as isize),
        );
    }
}

fn read_edit_text(edit: HWND) -> String {
    // SAFETY: edit is a live EDIT control while the window is open.
    let length = unsafe { GetWindowTextLengthW(edit) };
    if length <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; (length + 1) as usize];
    // SAFETY: buffer has room for length + 1 code units.
    let copied = unsafe { GetWindowTextW(edit, &mut buffer) };
    if copied <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..copied as usize])
}

fn open_api_key_page() {
    let operation = wide("open");
    let file = wide(super::API_KEY_PAGE);
    // SAFETY: both strings outlive the call and no window handle is required.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOW,
        )
    };
    if result.0 as isize <= 32 {
        set_status(
            "Could not open the API key page in your browser: ShellExecuteW could not open the API key page",
            true,
        );
    }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as i32;
            if id == ID_SAVE {
                let edit = STATE.with(|state| state.borrow().as_ref().map(|handles| handles.edit));
                if let Some(edit) = edit {
                    let value = read_edit_text(edit);
                    match super::save_api_key(&value) {
                        Ok(()) => {
                            let empty = wide("");
                            // SAFETY: edit is a live EDIT control.
                            if let Err(error) =
                                unsafe { SetWindowTextW(edit, PCWSTR(empty.as_ptr())) }
                            {
                                eprintln!(
                                    "Writely: could not clear the Settings key field: {error}"
                                );
                            }
                            refresh_cue_banner(edit);
                            set_status("Saved.", false);
                        }
                        Err(error) => set_status(&error, true),
                    }
                }
            } else if id == ID_REMOVE {
                let edit = STATE.with(|state| state.borrow().as_ref().map(|handles| handles.edit));
                if let Some(edit) = edit {
                    match super::remove_api_key() {
                        Ok(()) => {
                            let empty = wide("");
                            // SAFETY: edit is a live EDIT control.
                            if let Err(error) =
                                unsafe { SetWindowTextW(edit, PCWSTR(empty.as_ptr())) }
                            {
                                eprintln!(
                                    "Writely: could not clear the Settings key field: {error}"
                                );
                            }
                            refresh_cue_banner(edit);
                            set_status("Key removed.", false);
                        }
                        Err(error) => set_status(&error, true),
                    }
                }
            } else if id == ID_LINK {
                open_api_key_page();
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            let target = HWND(lparam.0 as *mut core::ffi::c_void);
            let is_status = STATE.with(|state| {
                state
                    .borrow()
                    .as_ref()
                    .map(|handles| handles.status == target && handles.error)
                    .unwrap_or(false)
            });
            if is_status {
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
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_CLOSE => {
            // SAFETY: hwnd is the settings window created by show_settings.
            if let Err(error) = unsafe { DestroyWindow(hwnd) } {
                eprintln!("Writely: could not close the settings window: {error}");
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            STATE.with(|state| {
                *state.borrow_mut() = None;
            });
            LRESULT(0)
        }
        // SAFETY: all other messages use the default window procedure.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub fn show_settings() -> Result<(), String> {
    let existing = STATE.with(|state| state.borrow().as_ref().map(|handles| handles.window));
    if let Some(window) = existing {
        // SAFETY: window came from STATE and is checked with IsWindow.
        let alive = unsafe { IsWindow(Some(window)).as_bool() };
        if alive {
            // SAFETY: window is a live top-level window on this thread.
            unsafe {
                ShowWindow(window, SW_SHOW);
                SetForegroundWindow(window);
            }
            return Ok(());
        }
        STATE.with(|state| {
            *state.borrow_mut() = None;
        });
    }

    // SAFETY: no instance handle is needed to get the current module handle.
    let module = unsafe { GetModuleHandleW(PCWSTR::null()) }
        .map_err(|error| format!("Could not open the settings window: {error}"))?;
    let instance = HINSTANCE(module.0);
    let class_name = wide("WritelySettings");
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: instance,
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    // SAFETY: window_class points at a live WNDCLASSW with a valid proc.
    unsafe { RegisterClassW(&window_class) };

    let title = wide("Writely Settings");
    // SAFETY: the class was registered above and all strings outlive the call.
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
            100,
            100,
            420,
            240,
            None,
            None,
            Some(instance),
            None,
        )
    }
    .map_err(|error| format!("Could not open the settings window: {error}"))?;

    // SAFETY: parent is the live settings window and instance is the module handle.
    let make_child = |class: &str,
                      text: &str,
                      style: WINDOW_STYLE,
                      x: i32,
                      y: i32,
                      width: i32,
                      height: i32,
                      id: i32|
     -> Result<HWND, String> {
        let class_wide = wide(class);
        let text_wide = wide(text);
        // SAFETY: class/text strings outlive the call and parent is live.
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class_wide.as_ptr()),
                PCWSTR(text_wide.as_ptr()),
                WS_CHILD | WS_VISIBLE | style,
                x,
                y,
                width,
                height,
                Some(window),
                Some(HMENU(id as *mut core::ffi::c_void)),
                Some(instance),
                None,
            )
        }
        .map_err(|error| format!("Could not open the settings window: {error}"))
    };

    let _label = make_child(
        "STATIC",
        "Google Gemini API key",
        WINDOW_STYLE(0),
        20,
        16,
        380,
        18,
        0,
    )?;
    let edit_style = WINDOW_STYLE(ES_PASSWORD as u32);
    let edit = make_child("EDIT", "", edit_style, 20, 40, 380, 24, ID_EDIT)?;
    let _help = make_child(
        "STATIC",
        "Writely sends the sentence you're editing to Google Gemini to check its grammar. Get a free key at ",
        WINDOW_STYLE(0),
        20,
        74,
        380,
        32,
        0,
    )?;
    let _link = make_child(
        "BUTTON",
        super::API_KEY_PAGE,
        WINDOW_STYLE(BS_PUSHBUTTON as u32),
        20,
        108,
        300,
        22,
        ID_LINK,
    )?;
    let _save = make_child(
        "BUTTON",
        "Save",
        WINDOW_STYLE(BS_PUSHBUTTON as u32),
        20,
        142,
        80,
        26,
        ID_SAVE,
    )?;
    let _remove = make_child(
        "BUTTON",
        "Remove key",
        WINDOW_STYLE(BS_PUSHBUTTON as u32),
        110,
        142,
        110,
        26,
        ID_REMOVE,
    )?;
    let status = make_child("STATIC", "", WINDOW_STYLE(0), 20, 178, 380, 18, 0)?;

    STATE.with(|state| {
        *state.borrow_mut() = Some(Handles {
            window,
            edit,
            status,
            error: false,
        });
    });
    refresh_cue_banner(edit);
    if let Err(error) = super::has_api_key() {
        set_status(&error, true);
    }
    // SAFETY: window is a live top-level window on this thread.
    unsafe {
        ShowWindow(window, SW_SHOW);
        SetForegroundWindow(window);
    }
    Ok(())
}
