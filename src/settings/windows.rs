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
    CBN_SELCHANGE, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, ES_PASSWORD, HMENU,
    SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC,
    WM_DESTROY, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_OVERLAPPED, WS_SYSMENU, WS_VISIBLE, WS_VSCROLL,
};

use crate::config::{AppSettings, Effort, Provider};

const ID_EDIT: i32 = 1001;
const ID_SAVE: i32 = 1002;
const ID_REMOVE: i32 = 1003;
const ID_LINK: i32 = 1004;
const ID_PROVIDER: i32 = 1005;
const ID_MODEL: i32 = 1006;
const ID_EFFORT: i32 = 1007;
const ID_URL: i32 = 1008;

const PROVIDER_NAMES: [&str; 3] = [
    "Google Gemini",
    "Together AI",
    "Ollama (local, no key needed)",
];
const EFFORT_NAMES: [&str; 5] = ["Default", "Minimal (fastest)", "Low", "Medium", "High"];

fn provider_at(index: i32) -> Provider {
    match index {
        1 => Provider::Together,
        2 => Provider::Ollama,
        _ => Provider::Gemini,
    }
}

fn provider_index(provider: Provider) -> usize {
    match provider {
        Provider::Gemini => 0,
        Provider::Together => 1,
        Provider::Ollama => 2,
    }
}

fn effort_at(index: i32) -> Effort {
    match index {
        1 => Effort::Minimal,
        2 => Effort::Low,
        3 => Effort::Medium,
        4 => Effort::High,
        _ => Effort::Default,
    }
}

fn effort_index(effort: Effort) -> usize {
    match effort {
        Effort::Default => 0,
        Effort::Minimal => 1,
        Effort::Low => 2,
        Effort::Medium => 3,
        Effort::High => 4,
    }
}

fn model_cue_for(provider: Provider) -> &'static str {
    match provider {
        Provider::Gemini => "gemini-3.5-flash-lite",
        Provider::Together => "deepseek-ai/DeepSeek-V4.1-Flash",
        Provider::Ollama => "llama3.2",
    }
}

struct Handles {
    window: HWND,
    provider_combo: HWND,
    model_edit: HWND,
    effort_combo: HWND,
    edit: HWND,
    url_edit: HWND,
    key_label: HWND,
    key_help: HWND,
    link: HWND,
    url_label: HWND,
    url_help: HWND,
    status: HWND,
    error: bool,
    settings: AppSettings,
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

fn combo_selection(combo: HWND) -> i32 {
    // SAFETY: combo is a live COMBOBOX while the window is open.
    unsafe { SendMessageW(combo, CB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))) }.0 as i32
}

fn set_combo_selection(combo: HWND, index: usize) {
    // SAFETY: combo is a live COMBOBOX while the window is open.
    unsafe {
        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(index)), Some(LPARAM(0)));
    }
}

fn set_edit_text(edit: HWND, text: &str) {
    let value = wide(text);
    // SAFETY: edit is a live EDIT control while the window is open.
    if let Err(error) = unsafe { SetWindowTextW(edit, PCWSTR(value.as_ptr())) } {
        eprintln!("Writely: could not update the Settings field: {error}");
    }
}

fn refresh_key_cue(edit: HWND, provider: Provider) {
    let has_key = match super::has_api_key_for(provider) {
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
            Some(WPARAM(0)),
            Some(LPARAM(cue.as_ptr() as isize)),
        );
    }
}

/// Shows the key rows (label/edit/help/link) or the Ollama-address rows for
/// the selected provider, and refreshes the model cue and key cue banner.
fn refresh_provider_rows() {
    struct Rows {
        provider: Provider,
        model_edit: HWND,
        edit: HWND,
        url_edit: HWND,
        key_label: HWND,
        key_help: HWND,
        link: HWND,
        url_label: HWND,
        url_help: HWND,
        model: String,
    }
    let rows = STATE.with(|state| {
        state.borrow().as_ref().map(|handles| Rows {
            provider: provider_at(combo_selection(handles.provider_combo)),
            model_edit: handles.model_edit,
            edit: handles.edit,
            url_edit: handles.url_edit,
            key_label: handles.key_label,
            key_help: handles.key_help,
            link: handles.link,
            url_label: handles.url_label,
            url_help: handles.url_help,
            model: handles
                .settings
                .model_for(provider_at(combo_selection(handles.provider_combo))),
        })
    });
    let Some(rows) = rows else { return };
    set_edit_text(rows.model_edit, &rows.model);
    let model_cue = wide(model_cue_for(rows.provider));
    // SAFETY: model_edit is a live EDIT control and the cue outlives the call.
    unsafe {
        SendMessageW(
            rows.model_edit,
            EM_SETCUEBANNER,
            Some(WPARAM(0)),
            Some(LPARAM(model_cue.as_ptr() as isize)),
        );
    }
    let is_ollama = rows.provider == Provider::Ollama;
    // SAFETY: all handles are live child windows while Settings is open.
    unsafe {
        for handle in [rows.key_label, rows.edit, rows.key_help, rows.link] {
            ShowWindow(handle, if is_ollama { SW_HIDE } else { SW_SHOW });
        }
        for handle in [rows.url_label, rows.url_edit, rows.url_help] {
            ShowWindow(handle, if is_ollama { SW_SHOW } else { SW_HIDE });
        }
    }
    let key_title = wide(match rows.provider {
        Provider::Gemini => "Google Gemini API key",
        Provider::Together => "Together AI API key",
        Provider::Ollama => "Ollama address",
    });
    // SAFETY: key_label is a live STATIC while the window is open.
    if let Err(error) = unsafe { SetWindowTextW(rows.key_label, PCWSTR(key_title.as_ptr())) } {
        eprintln!("Writely: could not update the Settings key label: {error}");
    }
    let key_help_text = wide(match rows.provider {
        Provider::Gemini => "Writely sends the sentence you're editing to Google Gemini to check its grammar. Get a free key at ",
        Provider::Together => "Writely sends the sentence you're editing to Together AI to check its grammar. Get a key at ",
        Provider::Ollama => "Where Ollama runs on your computer. No key needed.",
    });
    // SAFETY: key_help is a live STATIC while the window is open.
    if let Err(error) = unsafe { SetWindowTextW(rows.key_help, PCWSTR(key_help_text.as_ptr())) } {
        eprintln!("Writely: could not update the Settings help text: {error}");
    }
    if let Some(page) = super::api_key_page_for(rows.provider) {
        let link_text = wide(page);
        // SAFETY: link is a live BUTTON while the window is open.
        if let Err(error) = unsafe { SetWindowTextW(rows.link, PCWSTR(link_text.as_ptr())) } {
            eprintln!("Writely: could not update the Settings link: {error}");
        }
    }
    if !is_ollama {
        refresh_key_cue(rows.edit, rows.provider);
    }
}

/// Stores the model/effort/URL control values into the draft settings.
fn stash_draft() {
    STATE.with(|state| {
        if let Some(handles) = state.borrow_mut().as_mut() {
            let provider = provider_at(combo_selection(handles.provider_combo));
            let model = read_edit_text(handles.model_edit);
            let effort = effort_at(combo_selection(handles.effort_combo));
            let url = read_edit_text(handles.url_edit);
            handles.settings.provider = provider;
            handles.settings.set_model(provider, model.trim());
            handles.settings.effort = effort;
            if !url.trim().is_empty() {
                handles.settings.ollama_url = url.trim().to_string();
            }
        }
    });
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

fn current_provider() -> Provider {
    STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|handles| provider_at(combo_selection(handles.provider_combo)))
            .unwrap_or(Provider::Gemini)
    })
}

fn open_key_page(provider: Provider) {
    let Some(page) = super::api_key_page_for(provider) else {
        set_status("Ollama runs on your computer and needs no API key.", true);
        return;
    };
    let operation = wide("open");
    let file = wide(page);
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
            let notification = ((wparam.0 >> 16) & 0xffff) as u32;
            if id == ID_SAVE {
                let snapshot = STATE.with(|state| {
                    state.borrow().as_ref().map(|handles| {
                        (
                            handles.edit,
                            handles.model_edit,
                            handles.effort_combo,
                            handles.url_edit,
                            provider_at(combo_selection(handles.provider_combo)),
                        )
                    })
                });
                if let Some((edit, model_edit, effort_combo, url_edit, provider)) = snapshot {
                    let model = read_edit_text(model_edit);
                    if let Err(error) = crate::config::validate_model_name(&model) {
                        set_status(&error, true);
                        return LRESULT(0);
                    }
                    let effort = effort_at(combo_selection(effort_combo));
                    let url = read_edit_text(url_edit);
                    STATE.with(|state| {
                        if let Some(handles) = state.borrow_mut().as_mut() {
                            handles.settings.provider = provider;
                            handles.settings.set_model(provider, model.trim());
                            handles.settings.effort = effort;
                            if !url.trim().is_empty() {
                                handles.settings.ollama_url = url.trim().to_string();
                            }
                        }
                    });
                    let settings = STATE.with(|state| {
                        state
                            .borrow()
                            .as_ref()
                            .map(|handles| handles.settings.clone())
                    });
                    match settings.map(|settings| crate::config::save_settings(&settings)) {
                        Some(Ok(())) => {}
                        Some(Err(error)) => {
                            set_status(&error, true);
                            return LRESULT(0);
                        }
                        None => return LRESULT(0),
                    }
                    let value = read_edit_text(edit);
                    if value.trim().is_empty() {
                        set_status("Saved.", false);
                    } else {
                        match super::save_api_key_for(provider, &value) {
                            Ok(()) => set_status("Saved.", false),
                            Err(error) => {
                                set_status(&error, true);
                                return LRESULT(0);
                            }
                        }
                    }
                    set_edit_text(edit, "");
                    refresh_key_cue(edit, provider);
                }
            } else if id == ID_REMOVE {
                let edit = STATE.with(|state| state.borrow().as_ref().map(|handles| handles.edit));
                if let Some(edit) = edit {
                    let provider = current_provider();
                    match super::remove_api_key_for(provider) {
                        Ok(()) => {
                            set_edit_text(edit, "");
                            refresh_key_cue(edit, provider);
                            set_status("Key removed.", false);
                        }
                        Err(error) => set_status(&error, true),
                    }
                }
            } else if id == ID_LINK {
                open_key_page(current_provider());
            } else if id as u32 == ID_PROVIDER as u32 && notification == CBN_SELCHANGE {
                // Stash the model text for the previous provider first, so
                // switching providers keeps each one's model.
                stash_draft();
                refresh_provider_rows();
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
            460,
            560,
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

    let combo_style = WINDOW_STYLE((CBS_DROPDOWNLIST as u32) | WS_VSCROLL.0);

    let _provider_label = make_child("STATIC", "Provider", WINDOW_STYLE(0), 20, 16, 400, 18, 0)?;
    let provider_combo = make_child("COMBOBOX", "", combo_style, 20, 38, 400, 26, ID_PROVIDER)?;
    for name in PROVIDER_NAMES {
        let entry = wide(name);
        // SAFETY: provider_combo is a live COMBOBOX and entry outlives the call.
        unsafe {
            SendMessageW(
                provider_combo,
                CB_ADDSTRING,
                Some(WPARAM(0)),
                Some(LPARAM(entry.as_ptr() as isize)),
            );
        }
    }
    let _provider_help = make_child(
        "STATIC",
        "Who checks your grammar.",
        WINDOW_STYLE(0),
        20,
        68,
        400,
        16,
        0,
    )?;
    let _model_label = make_child("STATIC", "Model", WINDOW_STYLE(0), 20, 88, 400, 18, 0)?;
    let model_edit = make_child("EDIT", "", WINDOW_STYLE(0), 20, 110, 400, 24, ID_MODEL)?;
    let _model_help = make_child(
        "STATIC",
        "Paste the model name, e.g. gemini-3.5-flash-lite / deepseek-ai/DeepSeek-V4.1-Flash / llama3.2",
        WINDOW_STYLE(0),
        20,
        138,
        400,
        36,
        0,
    )?;
    let _effort_label = make_child("STATIC", "Effort", WINDOW_STYLE(0), 20, 178, 400, 18, 0)?;
    let effort_combo = make_child("COMBOBOX", "", combo_style, 20, 200, 400, 26, ID_EFFORT)?;
    for name in EFFORT_NAMES {
        let entry = wide(name);
        // SAFETY: effort_combo is a live COMBOBOX and entry outlives the call.
        unsafe {
            SendMessageW(
                effort_combo,
                CB_ADDSTRING,
                Some(WPARAM(0)),
                Some(LPARAM(entry.as_ptr() as isize)),
            );
        }
    }
    let _effort_help = make_child(
        "STATIC",
        "How hard the model thinks. Not every model supports every level; you'll see the model's error if not.",
        WINDOW_STYLE(0),
        20,
        230,
        400,
        36,
        0,
    )?;
    let key_label = make_child(
        "STATIC",
        "Google Gemini API key",
        WINDOW_STYLE(0),
        20,
        270,
        400,
        18,
        0,
    )?;
    let edit_style = WINDOW_STYLE(ES_PASSWORD as u32);
    let edit = make_child("EDIT", "", edit_style, 20, 292, 400, 24, ID_EDIT)?;
    let key_help = make_child(
        "STATIC",
        "Writely sends the sentence you're editing to Google Gemini to check its grammar. Get a free key at ",
        WINDOW_STYLE(0),
        20,
        320,
        400,
        36,
        0,
    )?;
    let default_page = super::api_key_page_for(Provider::Gemini).unwrap_or(super::API_KEY_PAGE);
    let link = make_child(
        "BUTTON",
        default_page,
        WINDOW_STYLE(BS_PUSHBUTTON as u32),
        20,
        360,
        320,
        24,
        ID_LINK,
    )?;
    // The Ollama-address rows share the key rows' space; only one set shows.
    let url_label = make_child(
        "STATIC",
        "Ollama address",
        WINDOW_STYLE(0),
        20,
        270,
        400,
        18,
        0,
    )?;
    let url_edit = make_child("EDIT", "", WINDOW_STYLE(0), 20, 292, 400, 24, ID_URL)?;
    let url_help = make_child(
        "STATIC",
        "Where Ollama runs on your computer. No key needed.",
        WINDOW_STYLE(0),
        20,
        320,
        400,
        36,
        0,
    )?;
    let _save = make_child(
        "BUTTON",
        "Save",
        WINDOW_STYLE(BS_PUSHBUTTON as u32),
        20,
        394,
        80,
        26,
        ID_SAVE,
    )?;
    let _remove = make_child(
        "BUTTON",
        "Remove key",
        WINDOW_STYLE(BS_PUSHBUTTON as u32),
        110,
        394,
        110,
        26,
        ID_REMOVE,
    )?;
    let status = make_child("STATIC", "", WINDOW_STYLE(0), 20, 428, 400, 36, 0)?;

    // Load the draft: a missing file means defaults; an unreadable or invalid
    // file shows its error and starts from defaults.
    let mut initial_error: Option<String> = None;
    let settings = match crate::config::load_settings() {
        Ok(settings) => settings,
        Err(error) => {
            initial_error = Some(error);
            AppSettings::default()
        }
    };

    STATE.with(|state| {
        *state.borrow_mut() = Some(Handles {
            window,
            provider_combo,
            model_edit,
            effort_combo,
            edit,
            url_edit,
            key_label,
            key_help,
            link,
            url_label,
            url_help,
            status,
            error: false,
            settings,
        });
    });
    let provider = current_provider();
    set_combo_selection(provider_combo, provider_index(provider));
    // Restore the saved effort/model/URL into the controls.
    let (effort, model, url) = STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|handles| {
                (
                    handles.settings.effort,
                    handles.settings.model_for(handles.settings.provider),
                    handles.settings.ollama_url.clone(),
                )
            })
            .unwrap_or((Effort::Default, String::new(), String::new()))
    });
    set_combo_selection(effort_combo, effort_index(effort));
    set_edit_text(model_edit, &model);
    set_edit_text(url_edit, &url);
    refresh_provider_rows();
    if let Some(error) = initial_error {
        set_status(&error, true);
    }
    // SAFETY: window is a live top-level window on this thread.
    unsafe {
        ShowWindow(window, SW_SHOW);
        SetForegroundWindow(window);
    }
    Ok(())
}
