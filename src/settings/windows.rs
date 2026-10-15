use std::cell::RefCell;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, GetMonitorInfoW, MonitorFromPoint, SetBkMode, SetTextColor,
    HDC, HFONT, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WindowsProgramming::MulDiv;
use windows::Win32::UI::Controls::EM_SETCUEBANNER;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowTextLengthW,
    GetWindowTextW, IsWindow, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowPos,
    SetWindowTextW, ShowWindow, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_PUSHBUTTON,
    CBN_SELCHANGE, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, ES_PASSWORD, HMENU,
    NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC, WM_DESTROY,
    WM_DPICHANGED, WM_SETFONT, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_OVERLAPPED, WS_SYSMENU,
    WS_VISIBLE, WS_VSCROLL,
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
const ID_AUTOSTART: i32 = 1009;

const PROVIDER_NAMES: [&str; 3] = [
    "Google Gemini",
    "Together AI",
    "Ollama (local, no key needed)",
];
const EFFORT_NAMES: [&str; 5] = ["Default", "Minimal (fastest)", "Low", "Medium", "High"];

// Logical (96 DPI) layout for the Settings window, mirroring the macOS
// top-down row cursor in `macos.rs`: content is 380 wide at x = 20 inside a
// 420-wide client area. `show_url` selects which of the shared key/Ollama
// rows the caller means; both sets share the same rects, so the geometry
// (and the client height) is identical either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl Rect {
    #[cfg(test)]
    fn right(&self) -> i32 {
        self.x + self.width
    }
    fn bottom(&self) -> i32 {
        self.y + self.height
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    show_url: bool,
    client_width: i32,
    client_height: i32,
    provider_label: Rect,
    provider_combo: Rect,
    provider_help: Rect,
    model_label: Rect,
    model_edit: Rect,
    model_help: Rect,
    effort_label: Rect,
    effort_combo: Rect,
    effort_help: Rect,
    key_label: Rect,
    key_edit: Rect,
    key_help: Rect,
    link: Rect,
    url_label: Rect,
    url_edit: Rect,
    url_help: Rect,
    autostart: Rect,
    autostart_help: Rect,
    save: Rect,
    remove: Rect,
    status: Rect,
}

fn layout(show_url: bool) -> Layout {
    let mut cursor = 16;
    let mut row = |height: i32, gap: i32| -> Rect {
        let rect = Rect {
            x: 20,
            y: cursor,
            width: 380,
            height,
        };
        cursor += height + gap;
        rect
    };
    let provider_label = row(18, 4);
    let provider_combo = row(26, 2);
    let provider_help = row(16, 10);
    let model_label = row(18, 4);
    let model_edit = row(24, 2);
    let model_help = row(44, 10);
    let effort_label = row(18, 4);
    let effort_combo = row(26, 2);
    let effort_help = row(34, 10);
    // The key rows and the Ollama-address rows share the same rects.
    let key_label = row(18, 4);
    let key_edit = row(24, 2);
    let key_help = row(34, 2);
    let link = row(18, 10);
    let url_label = key_label;
    let url_edit = key_edit;
    let url_help = key_help;
    let autostart = row(22, 2);
    let autostart_help = row(16, 10);
    let buttons = row(26, 12);
    let save = Rect {
        x: 20,
        y: buttons.y,
        width: 80,
        height: 26,
    };
    let remove = Rect {
        x: 110,
        y: buttons.y,
        width: 110,
        height: 26,
    };
    let status = row(36, 0);
    Layout {
        show_url,
        client_width: 420,
        client_height: status.bottom() + 16,
        provider_label,
        provider_combo,
        provider_help,
        model_label,
        model_edit,
        model_help,
        effort_label,
        effort_combo,
        effort_help,
        key_label,
        key_edit,
        key_help,
        link,
        url_label,
        url_edit,
        url_help,
        autostart,
        autostart_help,
        save,
        remove,
        status,
    }
}

#[cfg(test)]
mod layout_tests {
    use super::layout;

    // The visible rows in top-to-bottom order (the shared key/Ollama rows
    // appear once, since only one set is visible at a time).
    fn visible_rows(show_url: bool) -> Vec<super::Rect> {
        let lay = layout(show_url);
        let shared_label = if show_url {
            lay.url_label
        } else {
            lay.key_label
        };
        let shared_edit = if show_url { lay.url_edit } else { lay.key_edit };
        let shared_help = if show_url { lay.url_help } else { lay.key_help };
        vec![
            lay.provider_label,
            lay.provider_combo,
            lay.provider_help,
            lay.model_label,
            lay.model_edit,
            lay.model_help,
            lay.effort_label,
            lay.effort_combo,
            lay.effort_help,
            shared_label,
            shared_edit,
            shared_help,
            lay.link,
            lay.autostart,
            lay.autostart_help,
            lay.save,
            lay.status,
        ]
    }

    #[test]
    fn rows_never_overlap_and_go_strictly_downwards() {
        for show_url in [false, true] {
            let rows = visible_rows(show_url);
            for pair in rows.windows(2) {
                assert!(
                    pair[1].y >= pair[0].bottom(),
                    "rows overlap: {:?} then {:?}",
                    pair[0],
                    pair[1]
                );
                assert!(
                    pair[1].y > pair[0].y,
                    "rows do not go downwards: {:?} then {:?}",
                    pair[0],
                    pair[1]
                );
            }
        }
    }

    #[test]
    fn every_rect_is_inside_the_client() {
        for show_url in [false, true] {
            let lay = layout(show_url);
            assert_eq!(lay.client_width, 420);
            for rect in visible_rows(show_url) {
                assert!(
                    rect.x >= 0 && rect.right() <= lay.client_width,
                    "rect escapes the client: {rect:?}"
                );
                assert!(
                    rect.y >= 0 && rect.bottom() <= lay.client_height,
                    "rect escapes the client: {rect:?}"
                );
            }
            // Save and Remove key share the buttons row, side by side.
            assert_eq!(lay.save.y, lay.remove.y);
            assert!(lay.remove.x >= lay.save.right());
            assert!(lay.remove.right() <= lay.client_width);
        }
    }

    #[test]
    fn client_height_ends_16_below_the_status() {
        for show_url in [false, true] {
            let lay = layout(show_url);
            assert_eq!(lay.client_height, lay.status.bottom() + 16);
            assert_eq!(lay.key_label, lay.url_label);
            assert_eq!(lay.key_edit, lay.url_edit);
            assert_eq!(lay.key_help, lay.url_help);
        }
    }
}

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
    provider_label: HWND,
    provider_combo: HWND,
    provider_help: HWND,
    model_label: HWND,
    model_edit: HWND,
    model_help: HWND,
    effort_label: HWND,
    effort_combo: HWND,
    effort_help: HWND,
    edit: HWND,
    url_edit: HWND,
    key_label: HWND,
    key_help: HWND,
    link: HWND,
    url_label: HWND,
    url_help: HWND,
    autostart_checkbox: HWND,
    autostart_help: HWND,
    save: HWND,
    remove: HWND,
    status: HWND,
    font: HFONT,
    dpi: u32,
    error: bool,
    settings: AppSettings,
}

thread_local! {
    static STATE: RefCell<Option<Handles>> = const { RefCell::new(None) };
}

// Scales a logical (96 DPI) value to physical pixels at `dpi`.
fn scale(value: i32, dpi: u32) -> i32 {
    // SAFETY: MulDiv is a pure arithmetic function with no pointers.
    unsafe { MulDiv(value, dpi as i32, 96) }
}

fn scaled(rect: Rect, dpi: u32) -> Rect {
    Rect {
        x: scale(rect.x, dpi),
        y: scale(rect.y, dpi),
        width: scale(rect.width, dpi),
        height: scale(rect.height, dpi),
    }
}

// Loads the system message font at `dpi`. This is the only allowed
// fallback: on failure the caller logs and keeps the default font.
fn load_settings_font(dpi: u32) -> Result<HFONT, windows::core::Error> {
    let mut metrics = NONCLIENTMETRICSW {
        cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // SAFETY: metrics is a live NONCLIENTMETRICSW sized by cbSize.
    unsafe {
        SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            metrics.cbSize,
            Some(&mut metrics as *mut NONCLIENTMETRICSW as *mut core::ffi::c_void),
            0,
            dpi,
        )?;
    }
    // SAFETY: lfMessageFont was filled by SystemParametersInfoForDpi above.
    let font = unsafe { CreateFontIndirectW(&metrics.lfMessageFont) };
    if font.0.is_null() {
        return Err(windows::core::Error::from_thread());
    }
    Ok(font)
}

fn settings_font(dpi: u32) -> HFONT {
    match load_settings_font(dpi) {
        Ok(font) => font,
        Err(error) => {
            eprintln!("Writely: could not load the Settings font: {error}");
            HFONT::default()
        }
    }
}

fn delete_font(font: HFONT) {
    if font.0.is_null() {
        return;
    }
    // SAFETY: font was created by CreateFontIndirectW and is no longer in use.
    let deleted = unsafe { DeleteObject(HGDIOBJ(font.0)) };
    if !deleted.as_bool() {
        eprintln!("Writely: could not delete the Settings font.");
    }
}

fn child_rects(handles: &Handles) -> Vec<(HWND, Rect)> {
    let lay = layout(handles.settings.provider == Provider::Ollama);
    vec![
        (handles.provider_label, lay.provider_label),
        (handles.provider_combo, lay.provider_combo),
        (handles.provider_help, lay.provider_help),
        (handles.model_label, lay.model_label),
        (handles.model_edit, lay.model_edit),
        (handles.model_help, lay.model_help),
        (handles.effort_label, lay.effort_label),
        (handles.effort_combo, lay.effort_combo),
        (handles.effort_help, lay.effort_help),
        (handles.key_label, lay.key_label),
        (handles.edit, lay.key_edit),
        (handles.key_help, lay.key_help),
        (handles.link, lay.link),
        (handles.url_label, lay.url_label),
        (handles.url_edit, lay.url_edit),
        (handles.url_help, lay.url_help),
        (handles.autostart_checkbox, lay.autostart),
        (handles.autostart_help, lay.autostart_help),
        (handles.save, lay.save),
        (handles.remove, lay.remove),
        (handles.status, lay.status),
    ]
}

fn apply_font(handles: &Handles) {
    if handles.font.0.is_null() {
        return;
    }
    for (child, _) in child_rects(handles) {
        // SAFETY: child is a live child window and the font outlives the call.
        unsafe {
            SendMessageW(
                child,
                WM_SETFONT,
                Some(WPARAM(handles.font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
    }
}

fn apply_layout(handles: &Handles, dpi: u32) {
    for (child, rect) in child_rects(handles) {
        let rect = scaled(rect, dpi);
        // SAFETY: child is a live child window of the Settings window.
        if let Err(error) = unsafe {
            SetWindowPos(
                child,
                None,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        } {
            eprintln!("Writely: could not position a Settings control: {error}");
        }
    }
}

// Sizes the window for the scaled client area and centres it on the
// monitor that holds the mouse pointer.
fn size_and_center_window(window: HWND, dpi: u32) {
    let lay = layout(false);
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: scale(lay.client_width, dpi),
        bottom: scale(lay.client_height, dpi),
    };
    // SAFETY: rect is a live RECT and the style matches the created window.
    if let Err(error) = unsafe {
        AdjustWindowRectExForDpi(
            &mut rect,
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            false,
            WINDOW_EX_STYLE(0),
            dpi,
        )
    } {
        eprintln!("Writely: could not size the Settings window: {error}");
        return;
    }
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let mut point = POINT::default();
    // SAFETY: point is a live POINT for the cursor position.
    if let Err(error) = unsafe { GetCursorPos(&mut point) } {
        eprintln!("Writely: could not find the pointer for the Settings window: {error}");
        return;
    }
    // SAFETY: point was filled by GetCursorPos above.
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: info is a live MONITORINFO sized by cbSize.
    let have_info = unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool();
    let (x, y) = if have_info {
        (
            info.rcWork.left + (info.rcWork.right - info.rcWork.left - width) / 2,
            info.rcWork.top + (info.rcWork.bottom - info.rcWork.top - height) / 2,
        )
    } else {
        eprintln!("Writely: could not find the monitor for the Settings window.");
        (100, 100)
    };
    // SAFETY: window is the live Settings window.
    if let Err(error) = unsafe { SetWindowPos(window, None, x, y, width, height, SWP_NOZORDER) } {
        eprintln!("Writely: could not position the Settings window: {error}");
    }
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

fn checkbox_checked(checkbox: HWND) -> bool {
    // SAFETY: checkbox is a live BUTTON while the window is open.
    unsafe { SendMessageW(checkbox, BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0))) }.0 == 1
}

fn set_checkbox(checkbox: HWND, checked: bool) {
    // SAFETY: checkbox is a live BUTTON while the window is open.
    unsafe {
        SendMessageW(
            checkbox,
            BM_SETCHECK,
            Some(WPARAM(if checked { 1 } else { 0 })),
            Some(LPARAM(0)),
        );
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
            let _ = ShowWindow(handle, if is_ollama { SW_HIDE } else { SW_SHOW });
        }
        for handle in [rows.url_label, rows.url_edit, rows.url_help] {
            let _ = ShowWindow(handle, if is_ollama { SW_SHOW } else { SW_HIDE });
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
            handles.settings.launch_at_login = checkbox_checked(handles.autostart_checkbox);
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
                            handles.autostart_checkbox,
                            provider_at(combo_selection(handles.provider_combo)),
                        )
                    })
                });
                if let Some((
                    edit,
                    model_edit,
                    effort_combo,
                    url_edit,
                    autostart_checkbox,
                    provider,
                )) = snapshot
                {
                    let model = read_edit_text(model_edit);
                    if let Err(error) = crate::config::validate_model_name(&model) {
                        set_status(&error, true);
                        return LRESULT(0);
                    }
                    let effort = effort_at(combo_selection(effort_combo));
                    let url = read_edit_text(url_edit);
                    let launch_at_login = checkbox_checked(autostart_checkbox);
                    STATE.with(|state| {
                        if let Some(handles) = state.borrow_mut().as_mut() {
                            handles.settings.provider = provider;
                            handles.settings.set_model(provider, model.trim());
                            handles.settings.effort = effort;
                            handles.settings.launch_at_login = launch_at_login;
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
                    if let Err(error) = crate::autostart::set_enabled(launch_at_login) {
                        set_status(&error, true);
                        return LRESULT(0);
                    }
                    super::set_autostart_message(None);
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
        WM_DPICHANGED => {
            let dpi = (wparam.0 & 0xffff) as u32;
            // SAFETY: lParam points to the suggested RECT for WM_DPICHANGED.
            let suggested = unsafe { *(lparam.0 as *const RECT) };
            let new_font = settings_font(dpi);
            let snapshot = STATE.with(|state| {
                state.borrow_mut().as_mut().map(|handles| {
                    let old_font = std::mem::replace(&mut handles.font, new_font);
                    handles.dpi = dpi;
                    (handles.window, old_font)
                })
            });
            if let Some((window, old_font)) = snapshot {
                delete_font(old_font);
                STATE.with(|state| {
                    if let Some(handles) = state.borrow().as_ref() {
                        apply_font(handles);
                        apply_layout(handles, dpi);
                    }
                });
                // SAFETY: window is the live Settings window and suggested is the rect from lParam.
                if let Err(error) = unsafe {
                    SetWindowPos(
                        window,
                        None,
                        suggested.left,
                        suggested.top,
                        suggested.right - suggested.left,
                        suggested.bottom - suggested.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                } {
                    eprintln!(
                        "Writely: could not resize the Settings window for the new scale: {error}"
                    );
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let font = STATE.with(|state| state.borrow().as_ref().map(|handles| handles.font));
            if let Some(font) = font {
                delete_font(font);
            }
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
                let _ = ShowWindow(window, SW_SHOW);
                let _ = SetForegroundWindow(window);
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
            620,
            None,
            None,
            Some(instance),
            None,
        )
    }
    .map_err(|error| format!("Could not open the settings window: {error}"))?;

    let lay = layout(false);
    // SAFETY: window is the live Settings window just created above.
    let dpi = unsafe { GetDpiForWindow(window) };
    let dpi = if dpi == 0 { 96 } else { dpi };
    let font = settings_font(dpi);
    // SAFETY: parent is the live settings window and instance is the module handle.
    let make_child = |class: &str,
                      text: &str,
                      style: WINDOW_STYLE,
                      rect: Rect,
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
                scale(rect.x, dpi),
                scale(rect.y, dpi),
                scale(rect.width, dpi),
                scale(rect.height, dpi),
                Some(window),
                Some(HMENU(id as *mut core::ffi::c_void)),
                Some(instance),
                None,
            )
        }
        .map_err(|error| format!("Could not open the settings window: {error}"))
    };

    let combo_style = WINDOW_STYLE((CBS_DROPDOWNLIST as u32) | WS_VSCROLL.0);

    let provider_label = make_child("STATIC", "Provider", WINDOW_STYLE(0), lay.provider_label, 0)?;
    let provider_combo = make_child("COMBOBOX", "", combo_style, lay.provider_combo, ID_PROVIDER)?;
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
    let provider_help = make_child(
        "STATIC",
        "Who checks your grammar.",
        WINDOW_STYLE(0),
        lay.provider_help,
        0,
    )?;
    let model_label = make_child("STATIC", "Model", WINDOW_STYLE(0), lay.model_label, 0)?;
    let model_edit = make_child("EDIT", "", WINDOW_STYLE(0), lay.model_edit, ID_MODEL)?;
    let model_help = make_child("STATIC", "Paste the model name, e.g. gemini-3.5-flash-lite / deepseek-ai/DeepSeek-V4.1-Flash / llama3.2", WINDOW_STYLE(0), lay.model_help, 0)?;
    let effort_label = make_child("STATIC", "Effort", WINDOW_STYLE(0), lay.effort_label, 0)?;
    let effort_combo = make_child("COMBOBOX", "", combo_style, lay.effort_combo, ID_EFFORT)?;
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
    let effort_help = make_child("STATIC", "How hard the model thinks. Not every model supports every level; you'll see the model's error if not.", WINDOW_STYLE(0), lay.effort_help, 0)?;
    let key_label = make_child(
        "STATIC",
        "Google Gemini API key",
        WINDOW_STYLE(0),
        lay.key_label,
        0,
    )?;
    let edit_style = WINDOW_STYLE(ES_PASSWORD as u32);
    let edit = make_child("EDIT", "", edit_style, lay.key_edit, ID_EDIT)?;
    let key_help = make_child("STATIC", "Writely sends the sentence you're editing to Google Gemini to check its grammar. Get a free key at ", WINDOW_STYLE(0), lay.key_help, 0)?;
    let default_page = super::api_key_page_for(Provider::Gemini).unwrap_or(super::API_KEY_PAGE);
    let link = make_child(
        "BUTTON",
        default_page,
        WINDOW_STYLE(BS_PUSHBUTTON as u32),
        lay.link,
        ID_LINK,
    )?;
    // The Ollama-address rows share the key rows' space; only one set shows.
    let url_label = make_child(
        "STATIC",
        "Ollama address",
        WINDOW_STYLE(0),
        lay.url_label,
        0,
    )?;
    let url_edit = make_child("EDIT", "", WINDOW_STYLE(0), lay.url_edit, ID_URL)?;
    let url_help = make_child(
        "STATIC",
        "Where Ollama runs on your computer. No key needed.",
        WINDOW_STYLE(0),
        lay.url_help,
        0,
    )?;
    let autostart_checkbox = make_child(
        "BUTTON",
        "Start Writely at login",
        WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
        lay.autostart,
        ID_AUTOSTART,
    )?;
    let autostart_help = make_child(
        "STATIC",
        "Writely starts automatically when you log in.",
        WINDOW_STYLE(0),
        lay.autostart_help,
        0,
    )?;
    let save = make_child(
        "BUTTON",
        "Save",
        WINDOW_STYLE(BS_PUSHBUTTON as u32),
        lay.save,
        ID_SAVE,
    )?;
    let remove = make_child(
        "BUTTON",
        "Remove key",
        WINDOW_STYLE(BS_PUSHBUTTON as u32),
        lay.remove,
        ID_REMOVE,
    )?;
    let status = make_child("STATIC", "", WINDOW_STYLE(0), lay.status, 0)?;

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
            provider_label,
            provider_combo,
            provider_help,
            model_label,
            model_edit,
            model_help,
            effort_label,
            effort_combo,
            effort_help,
            edit,
            url_edit,
            key_label,
            key_help,
            link,
            url_label,
            url_help,
            autostart_checkbox,
            autostart_help,
            save,
            remove,
            status,
            font,
            dpi,
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
    let launch_at_login = STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|handles| handles.settings.launch_at_login)
            .unwrap_or(true)
    });
    set_checkbox(autostart_checkbox, launch_at_login);
    STATE.with(|state| {
        if let Some(handles) = state.borrow().as_ref() {
            apply_font(handles);
            apply_layout(handles, dpi);
        }
    });
    size_and_center_window(window, dpi);
    refresh_provider_rows();
    if let Some(error) = initial_error {
        set_status(&error, true);
    } else if let Some(error) = super::autostart_message() {
        set_status(&error, true);
    }
    // SAFETY: window is a live top-level window on this thread.
    unsafe {
        let _ = ShowWindow(window, SW_SHOW);
        let _ = SetForegroundWindow(window);
    }
    Ok(())
}
