use std::cell::RefCell;
use std::sync::Arc;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, GetMonitorInfoW, GetSysColor, GetSysColorBrush,
    MonitorFromPoint, MonitorFromWindow, SetBkMode, SetTextColor, COLOR_GRAYTEXT, COLOR_WINDOW,
    HBRUSH, HDC, HFONT, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WindowsProgramming::MulDiv;
use windows::Win32::UI::Controls::{EM_SETCUEBANNER, NMHDR, NM_CLICK, NM_RETURN};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetKeyState, SetFocus, VK_RETURN, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::Shell::{
    DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetNextDlgTabItem, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, IsWindow, KillTimer, MessageBoxW, RegisterClassW,
    SendMessageW, SetForegroundWindow, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow,
    BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CBN_SELCHANGE,
    CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, DLGC_WANTTAB, ES_AUTOHSCROLL,
    ES_NUMBER, ES_PASSWORD, HMENU, IDYES, MB_ICONQUESTION, MB_YESNO, NONCLIENTMETRICSW,
    SPI_GETNONCLIENTMETRICS, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED,
    WM_GETDLGCODE, WM_KEYDOWN, WM_NOTIFY, WM_SETFONT, WM_TIMER, WNDCLASSW, WS_CAPTION, WS_CHILD,
    WS_EX_CLIENTEDGE, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::config::{AppSettings, Effort, LocalModelId, Provider};
use crate::local::{catalog, download, install};

const ID_EDIT: i32 = 1001;
const ID_SAVE: i32 = 1002;
const ID_REMOVE: i32 = 1003;
const ID_LINK: i32 = 1004;
const ID_PROVIDER: i32 = 1005;
const ID_MODEL: i32 = 1006;
const ID_EFFORT: i32 = 1007;
const ID_URL: i32 = 1008;
const ID_AUTOSTART: i32 = 1009;
const ID_CLIPBOARD: i32 = 1010;
const ID_LOCAL_DOWNLOAD: i32 = 1011;
const ID_LOCAL_CANCEL: i32 = 1012;
const ID_LOCAL_DELETE: i32 = 1013;
const ID_LOCAL_MODEL: i32 = 1014;
const ID_LOCAL_WARM: i32 = 1015;
const ID_LOCAL_ADVANCED: i32 = 1016;
const ID_LOCAL_IDLE: i32 = 1017;
const ID_LOCAL_THREADS: i32 = 1018;
const ID_LOCAL_MAX_REPLY: i32 = 1019;
const ID_LOCAL_CONTEXT: i32 = 1020;
const ID_LOCAL_STARTUP: i32 = 1021;
const ID_LOCAL_REPLY: i32 = 1022;
const ID_LOCAL_TIMER: usize = 2001;
const TAB_SUBCLASS_ID: usize = 1;

const PROVIDER_NAMES: [&str; 4] = [
    "Google Gemini",
    "Together AI",
    "Ollama (local, no key needed)",
    "On this computer",
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
    is_local: bool,
    show_advanced: bool,
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
    local_model_label: Rect,
    local_model_combo: Rect,
    local_model_help: Rect,
    local_status: Rect,
    local_download: Rect,
    local_cancel: Rect,
    local_delete: Rect,
    local_runtime_help: Rect,
    local_warm: Rect,
    local_warm_help: Rect,
    local_idle_label: Rect,
    local_idle_edit: Rect,
    local_idle_help: Rect,
    local_advanced: Rect,
    adv_labels: [Rect; 5],
    adv_edits: [Rect; 5],
    adv_helps: [Rect; 5],
    autostart: Rect,
    autostart_help: Rect,
    clipboard: Rect,
    clipboard_help: Rect,
    save: Rect,
    remove: Rect,
    tip: Rect,
    status: Rect,
}

fn layout(show_url: bool, is_local: bool, show_advanced: bool) -> Layout {
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
    // The shared model rows are hidden for Local; they take no space then.
    let (model_label, model_edit, model_help) = if is_local {
        (provider_help, provider_help, provider_help)
    } else {
        (row(18, 4), row(24, 2), row(44, 10))
    };
    let effort_label = row(18, 4);
    let effort_combo = row(26, 2);
    let effort_help = row(34, 10);
    let placeholder = effort_help;
    let (
        key_label,
        key_edit,
        key_help,
        link,
        local_model_label,
        local_model_combo,
        local_model_help,
        local_status,
        local_download,
        local_cancel,
        local_delete,
        local_runtime_help,
        local_warm,
        local_warm_help,
        local_idle_label,
        local_idle_edit,
        local_idle_help,
        local_advanced,
        adv_labels,
        adv_edits,
        adv_helps,
    ) = if is_local {
        let local_model_label = row(18, 4);
        let local_model_combo = row(26, 2);
        let local_model_help = row(30, 8);
        let local_status = row(36, 4);
        let local_buttons = row(26, 6);
        let local_download = Rect {
            x: 20,
            y: local_buttons.y,
            width: 90,
            height: 26,
        };
        let local_cancel = Rect {
            x: 120,
            y: local_buttons.y,
            width: 80,
            height: 26,
        };
        let local_delete = Rect {
            x: 210,
            y: local_buttons.y,
            width: 110,
            height: 26,
        };
        let local_runtime_help = row(40, 8);
        let local_warm = row(22, 2);
        let local_warm_help = row(30, 8);
        let local_idle_label = row(18, 4);
        let local_idle_edit = row(24, 2);
        let local_idle_help = row(30, 8);
        let local_advanced = row(22, 8);
        let (adv_labels, adv_edits, adv_helps) = if show_advanced {
            let help_heights = [30, 30, 30, 16, 16];
            let mut labels = Vec::new();
            let mut edits = Vec::new();
            let mut helps = Vec::new();
            for help_height in help_heights {
                // Label and edit share a row to keep the window short.
                let adv_row = row(24, 2);
                labels.push(Rect {
                    x: 20,
                    y: adv_row.y + 3,
                    width: 200,
                    height: 18,
                });
                edits.push(Rect {
                    x: 230,
                    y: adv_row.y,
                    width: 170,
                    height: 24,
                });
                helps.push(row(help_height, 8));
            }
            (
                labels.try_into().expect("five advanced labels"),
                edits.try_into().expect("five advanced edits"),
                helps.try_into().expect("five advanced helps"),
            )
        } else {
            ([placeholder; 5], [placeholder; 5], [placeholder; 5])
        };
        (
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            local_model_label,
            local_model_combo,
            local_model_help,
            local_status,
            local_download,
            local_cancel,
            local_delete,
            local_runtime_help,
            local_warm,
            local_warm_help,
            local_idle_label,
            local_idle_edit,
            local_idle_help,
            local_advanced,
            adv_labels,
            adv_edits,
            adv_helps,
        )
    } else {
        // The key rows and the Ollama-address rows share the same rects.
        let key_label = row(18, 4);
        let key_edit = row(24, 2);
        let key_help = row(34, 2);
        let link = row(18, 10);
        (
            key_label,
            key_edit,
            key_help,
            link,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            [placeholder; 5],
            [placeholder; 5],
            [placeholder; 5],
        )
    };
    let url_label = key_label;
    let url_edit = key_edit;
    let url_help = key_help;
    let autostart = row(22, 2);
    let autostart_help = row(16, 10);
    let clipboard = row(22, 2);
    let clipboard_help = row(34, 10);
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
    let tip = row(18, 10);
    let status = row(36, 0);
    Layout {
        show_url,
        is_local,
        show_advanced,
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
        local_model_label,
        local_model_combo,
        local_model_help,
        local_status,
        local_download,
        local_cancel,
        local_delete,
        local_runtime_help,
        local_warm,
        local_warm_help,
        local_idle_label,
        local_idle_edit,
        local_idle_help,
        local_advanced,
        adv_labels,
        adv_edits,
        adv_helps,
        autostart,
        autostart_help,
        clipboard,
        clipboard_help,
        save,
        remove,
        tip,
        status,
    }
}

fn layout_for(handles: &Handles) -> Layout {
    layout(
        handles.settings.provider == Provider::Ollama,
        handles.settings.provider == Provider::Local,
        handles.show_advanced,
    )
}

#[cfg(test)]
mod layout_tests {
    use super::layout;

    const COMBOS: [(bool, bool, bool); 4] = [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (false, true, true),
    ];

    // The visible rows in top-to-bottom order (the shared key/Ollama rows
    // appear once, since only one set is visible at a time). Side-by-side
    // button/advanced rows appear once, by their leftmost member.
    fn visible_rows(show_url: bool, is_local: bool, show_advanced: bool) -> Vec<super::Rect> {
        let lay = layout(show_url, is_local, show_advanced);
        let mut rows = vec![lay.provider_label, lay.provider_combo, lay.provider_help];
        if !is_local {
            rows.extend([lay.model_label, lay.model_edit, lay.model_help]);
        }
        rows.extend([lay.effort_label, lay.effort_combo, lay.effort_help]);
        if is_local {
            rows.extend([
                lay.local_model_label,
                lay.local_model_combo,
                lay.local_model_help,
                lay.local_status,
                lay.local_download,
                lay.local_runtime_help,
                lay.local_warm,
                lay.local_warm_help,
                lay.local_idle_label,
                lay.local_idle_edit,
                lay.local_idle_help,
                lay.local_advanced,
            ]);
            if show_advanced {
                for i in 0..5 {
                    rows.push(lay.adv_edits[i]);
                    rows.push(lay.adv_helps[i]);
                }
            }
        } else {
            let shared_label = if show_url {
                lay.url_label
            } else {
                lay.key_label
            };
            let shared_edit = if show_url { lay.url_edit } else { lay.key_edit };
            let shared_help = if show_url { lay.url_help } else { lay.key_help };
            rows.extend([shared_label, shared_edit, shared_help, lay.link]);
        }
        rows.extend([
            lay.autostart,
            lay.autostart_help,
            lay.clipboard,
            lay.clipboard_help,
            lay.save,
            lay.tip,
            lay.status,
        ]);
        rows
    }

    #[test]
    fn rows_never_overlap_and_go_strictly_downwards() {
        for (show_url, is_local, show_advanced) in COMBOS {
            let rows = visible_rows(show_url, is_local, show_advanced);
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
        for (show_url, is_local, show_advanced) in COMBOS {
            let lay = layout(show_url, is_local, show_advanced);
            assert_eq!(lay.client_width, 420);
            for rect in visible_rows(show_url, is_local, show_advanced) {
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
            if is_local {
                assert_eq!(lay.local_download.y, lay.local_cancel.y);
                assert_eq!(lay.local_cancel.y, lay.local_delete.y);
                assert!(lay.local_delete.right() <= lay.client_width);
                if show_advanced {
                    for i in 0..5 {
                        assert!(lay.adv_labels[i].right() <= lay.adv_edits[i].x);
                        assert!(lay.adv_edits[i].right() <= lay.client_width);
                        assert!(lay.adv_edits[i].bottom() <= lay.client_height);
                    }
                }
            }
        }
    }

    #[test]
    fn client_height_ends_16_below_the_status() {
        for (show_url, is_local, show_advanced) in COMBOS {
            let lay = layout(show_url, is_local, show_advanced);
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
        3 => Provider::Local,
        _ => Provider::Gemini,
    }
}

fn provider_index(provider: Provider) -> usize {
    match provider {
        Provider::Gemini => 0,
        Provider::Together => 1,
        Provider::Ollama => 2,
        Provider::Local => 3,
    }
}

fn local_model_at(index: i32) -> LocalModelId {
    match index {
        1 => LocalModelId::Qwen35_2B,
        _ => LocalModelId::Qwen35_0_8B,
    }
}

fn local_model_index(model: LocalModelId) -> usize {
    match model {
        LocalModelId::Qwen35_0_8B => 0,
        LocalModelId::Qwen35_2B => 1,
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
        Provider::Local => "",
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
    local_model_label: HWND,
    local_model_combo: HWND,
    local_model_help: HWND,
    local_status: HWND,
    local_download: HWND,
    local_cancel: HWND,
    local_delete: HWND,
    local_runtime_help: HWND,
    local_warm_checkbox: HWND,
    local_warm_help: HWND,
    local_idle_label: HWND,
    local_idle_edit: HWND,
    local_idle_help: HWND,
    local_advanced_checkbox: HWND,
    adv_labels: [HWND; 5],
    adv_edits: [HWND; 5],
    adv_helps: [HWND; 5],
    autostart_checkbox: HWND,
    autostart_help: HWND,
    clipboard_checkbox: HWND,
    clipboard_help: HWND,
    save: HWND,
    remove: HWND,
    tip: HWND,
    status: HWND,
    font: HFONT,
    dpi: u32,
    error: bool,
    local_error: bool,
    local_runtime_error: bool,
    local_was_busy: bool,
    show_advanced: bool,
    settings: AppSettings,
    draft_keys: [String; 3],
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
    let lay = layout_for(handles);
    let mut rects = vec![
        (handles.provider_label, lay.provider_label),
        (
            handles.provider_combo,
            Rect {
                height: lay.provider_combo.height + 200,
                ..lay.provider_combo
            },
        ),
        (handles.provider_help, lay.provider_help),
        (handles.model_label, lay.model_label),
        (handles.model_edit, lay.model_edit),
        (handles.model_help, lay.model_help),
        (handles.effort_label, lay.effort_label),
        (
            handles.effort_combo,
            Rect {
                height: lay.effort_combo.height + 200,
                ..lay.effort_combo
            },
        ),
        (handles.effort_help, lay.effort_help),
        (handles.key_label, lay.key_label),
        (handles.edit, lay.key_edit),
        (handles.key_help, lay.key_help),
        (handles.link, lay.link),
        (handles.url_label, lay.url_label),
        (handles.url_edit, lay.url_edit),
        (handles.url_help, lay.url_help),
        (handles.local_model_label, lay.local_model_label),
        (
            handles.local_model_combo,
            Rect {
                height: lay.local_model_combo.height + 200,
                ..lay.local_model_combo
            },
        ),
        (handles.local_model_help, lay.local_model_help),
        (handles.local_status, lay.local_status),
        (handles.local_download, lay.local_download),
        (handles.local_cancel, lay.local_cancel),
        (handles.local_delete, lay.local_delete),
        (handles.local_runtime_help, lay.local_runtime_help),
        (handles.local_warm_checkbox, lay.local_warm),
        (handles.local_warm_help, lay.local_warm_help),
        (handles.local_idle_label, lay.local_idle_label),
        (handles.local_idle_edit, lay.local_idle_edit),
        (handles.local_idle_help, lay.local_idle_help),
        (handles.local_advanced_checkbox, lay.local_advanced),
        (handles.autostart_checkbox, lay.autostart),
        (handles.autostart_help, lay.autostart_help),
        (handles.clipboard_checkbox, lay.clipboard),
        (handles.clipboard_help, lay.clipboard_help),
        (handles.save, lay.save),
        (handles.remove, lay.remove),
        (handles.tip, lay.tip),
        (handles.status, lay.status),
    ];
    for i in 0..5 {
        rects.push((handles.adv_labels[i], lay.adv_labels[i]));
        rects.push((handles.adv_edits[i], lay.adv_edits[i]));
        rects.push((handles.adv_helps[i], lay.adv_helps[i]));
    }
    rects
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

fn current_layout() -> Layout {
    STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(layout_for)
            .unwrap_or_else(|| layout(false, false, false))
    })
}

fn monitor_work_area(monitor: windows::Win32::Graphics::Gdi::HMONITOR) -> Option<RECT> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: info is a live MONITORINFO sized by cbSize.
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        Some(info.rcWork)
    } else {
        None
    }
}

// Clamps a window rectangle's size and position into `work`.
fn clamped_into(work: RECT, x: i32, y: i32, width: i32, height: i32) -> (i32, i32, i32, i32) {
    let width = width.min(work.right - work.left);
    let height = height.min(work.bottom - work.top);
    let x = x.clamp(work.left, (work.right - width).max(work.left));
    let y = y.clamp(work.top, (work.bottom - height).max(work.top));
    (x, y, width, height)
}

// Sizes the window for the scaled client area and centres it on the
// monitor that holds the mouse pointer.
fn size_and_center_window(window: HWND, dpi: u32) {
    let lay = current_layout();
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
    let (x, y, width, height) = if let Some(work) = monitor_work_area(monitor) {
        clamped_into(
            work,
            work.left + (work.right - work.left - width) / 2,
            work.top + (work.bottom - work.top - height) / 2,
            width,
            height,
        )
    } else {
        eprintln!("Writely: could not find the monitor for the Settings window.");
        (100, 100, width, height)
    };
    // SAFETY: window is the live Settings window.
    if let Err(error) = unsafe { SetWindowPos(window, None, x, y, width, height, SWP_NOZORDER) } {
        eprintln!("Writely: could not position the Settings window: {error}");
    }
}

// Re-layouts and resizes the open window for the current layout, keeping
// its top-left corner where it is where possible and staying inside the
// work area of the monitor that holds the window.
fn refit_window() {
    // The snapshot ends before any Win32 call below.
    let snapshot = STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|handles| (handles.window, handles.dpi, layout_for(handles)))
    });
    let Some((window, dpi, lay)) = snapshot else {
        return;
    };
    STATE.with(|state| {
        if let Some(handles) = state.borrow().as_ref() {
            apply_layout(handles, dpi);
        }
    });
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
    let mut current = RECT::default();
    // SAFETY: window is live and current is a live RECT.
    let (x, y) = match unsafe { GetWindowRect(window, &mut current) } {
        Ok(()) => (current.left, current.top),
        Err(error) => {
            eprintln!("Writely: could not read the Settings window position: {error}");
            (100, 100)
        }
    };
    // SAFETY: window is the live Settings window.
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    let (x, y, width, height) = if let Some(work) = monitor_work_area(monitor) {
        clamped_into(work, x, y, width, height)
    } else {
        eprintln!("Writely: could not find the monitor for the Settings window.");
        (x, y, width, height)
    };
    // SAFETY: window is the live Settings window.
    if let Err(error) = unsafe {
        SetWindowPos(
            window,
            None,
            x,
            y,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    } {
        eprintln!("Writely: could not position the Settings window: {error}");
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn set_status(text: &str, is_error: bool) {
    // The borrow ends before SetWindowTextW: it can repaint the label
    // synchronously, and WM_CTLCOLORSTATIC reads STATE again.
    let status = STATE.with(|state| {
        state.borrow_mut().as_mut().map(|handles| {
            handles.error = is_error;
            handles.status
        })
    });
    if let Some(status) = status {
        let value = wide(text);
        // SAFETY: status is a live STATIC while the window is open.
        if let Err(error) = unsafe { SetWindowTextW(status, PCWSTR(value.as_ptr())) } {
            eprintln!("Writely: could not update the Settings status: {error}");
        }
    }
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

fn set_local_status(text: &str, is_error: bool) {
    // The borrow ends before SetWindowTextW (see set_status).
    let status = STATE.with(|state| {
        state.borrow_mut().as_mut().map(|handles| {
            handles.local_error = is_error;
            handles.local_status
        })
    });
    if let Some(status) = status {
        let value = wide(text);
        // SAFETY: status is a live STATIC while the window is open.
        if let Err(error) = unsafe { SetWindowTextW(status, PCWSTR(value.as_ptr())) } {
            eprintln!("Writely: could not update the Settings local status: {error}");
        }
    }
}

/// Re-renders the local-model status, buttons and model-combo state from
/// the current install phase for the selected model (like Linux).
fn render_local() {
    // Snapshots end before the Win32 calls below, which can repaint and
    // re-enter through WM_CTLCOLORSTATIC (see the comment in set_status).
    let snapshot = STATE.with(|state| {
        state.borrow().as_ref().map(|handles| {
            (
                local_model_at(combo_selection(handles.local_model_combo)),
                handles.local_model_combo,
                handles.local_download,
                handles.local_cancel,
                handles.local_delete,
                handles.local_runtime_error,
            )
        })
    });
    let Some((selected, combo, download_button, cancel_button, delete_button, runtime_error)) =
        snapshot
    else {
        return;
    };
    let phase = install::install_phase();
    let ready = install::model_ready(selected);
    let view = super::local_view(&phase, ready, catalog::model_file(selected).label);
    let busy = matches!(
        phase,
        install::InstallPhase::Downloading { .. }
            | install::InstallPhase::Verifying { .. }
            | install::InstallPhase::Unpacking
    );
    let finished = STATE.with(|state| {
        state
            .borrow_mut()
            .as_mut()
            .map(|handles| {
                let finished =
                    handles.local_was_busy && matches!(phase, install::InstallPhase::Idle) && ready;
                handles.local_was_busy = busy;
                finished
            })
            .unwrap_or(false)
    });
    set_local_status(&view.status, view.is_error);
    // SAFETY: all handles are live child windows while Settings is open.
    unsafe {
        let _ = ShowWindow(
            download_button,
            if view.show_download && !runtime_error {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
        let _ = ShowWindow(
            cancel_button,
            if view.show_cancel && !runtime_error {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
        let _ = ShowWindow(
            delete_button,
            if view.show_delete && !runtime_error {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
        let _ = EnableWindow(combo, view.fields_enabled);
    }
    if finished {
        super::notify_changed();
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
        model_label: HWND,
        model_edit: HWND,
        model_help: HWND,
        edit: HWND,
        url_edit: HWND,
        key_label: HWND,
        key_help: HWND,
        link: HWND,
        url_label: HWND,
        url_help: HWND,
        local_rows: Vec<HWND>,
        advanced_rows: Vec<HWND>,
        show_advanced: bool,
        model: String,
    }
    let rows = STATE.with(|state| {
        state.borrow().as_ref().map(|handles| {
            let mut local_rows = vec![
                handles.local_model_label,
                handles.local_model_combo,
                handles.local_model_help,
                handles.local_status,
                handles.local_runtime_help,
                handles.local_warm_checkbox,
                handles.local_warm_help,
                handles.local_idle_label,
                handles.local_idle_edit,
                handles.local_idle_help,
                handles.local_advanced_checkbox,
            ];
            // render_local owns the three buttons' visibility while Local
            // is shown; they are only forced hidden with the section.
            local_rows.extend([
                handles.local_download,
                handles.local_cancel,
                handles.local_delete,
            ]);
            let mut advanced_rows = Vec::new();
            for i in 0..5 {
                advanced_rows.push(handles.adv_labels[i]);
                advanced_rows.push(handles.adv_edits[i]);
                advanced_rows.push(handles.adv_helps[i]);
            }
            Rows {
                provider: provider_at(combo_selection(handles.provider_combo)),
                model_label: handles.model_label,
                model_edit: handles.model_edit,
                model_help: handles.model_help,
                edit: handles.edit,
                url_edit: handles.url_edit,
                key_label: handles.key_label,
                key_help: handles.key_help,
                link: handles.link,
                url_label: handles.url_label,
                url_help: handles.url_help,
                local_rows,
                advanced_rows,
                show_advanced: handles.show_advanced,
                model: handles
                    .settings
                    .model_for(provider_at(combo_selection(handles.provider_combo))),
            }
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
    let is_local = rows.provider == Provider::Local;
    // SAFETY: all handles are live child windows while Settings is open.
    unsafe {
        for handle in [rows.key_label, rows.edit, rows.key_help, rows.link] {
            let _ = ShowWindow(
                handle,
                if is_ollama || is_local {
                    SW_HIDE
                } else {
                    SW_SHOW
                },
            );
        }
        for handle in [rows.url_label, rows.url_edit, rows.url_help] {
            let _ = ShowWindow(handle, if is_ollama { SW_SHOW } else { SW_HIDE });
        }
        for handle in [rows.model_label, rows.model_edit, rows.model_help] {
            let _ = ShowWindow(handle, if is_local { SW_HIDE } else { SW_SHOW });
        }
        // The first 11 local rows always follow the section; the last 3
        // are the buttons, whose visibility render_local sets while the
        // section is shown.
        for (index, handle) in rows.local_rows.iter().enumerate() {
            let show = is_local && index < 11;
            let _ = ShowWindow(*handle, if show { SW_SHOW } else { SW_HIDE });
        }
        for handle in &rows.advanced_rows {
            let _ = ShowWindow(
                *handle,
                if is_local && rows.show_advanced {
                    SW_SHOW
                } else {
                    SW_HIDE
                },
            );
        }
    }
    let key_title = wide(match rows.provider {
        Provider::Gemini => "Google Gemini API key",
        Provider::Together => "Together AI API key",
        Provider::Ollama => "Ollama address",
        Provider::Local => "Ollama address",
    });
    // SAFETY: key_label is a live STATIC while the window is open.
    if let Err(error) = unsafe { SetWindowTextW(rows.key_label, PCWSTR(key_title.as_ptr())) } {
        eprintln!("Writely: could not update the Settings key label: {error}");
    }
    let key_help_text = wide(match rows.provider {
        Provider::Gemini => "Writely sends the sentence you're editing to Google Gemini to check its grammar. Get a free key at ",
        Provider::Together => "Writely sends the sentence you're editing to Together AI to check its grammar. Get a key at ",
        Provider::Ollama => "Where Ollama runs on your computer. No key needed.",
        Provider::Local => "This runs a small AI model on your computer. Download and memory options are being added.",
    });
    // SAFETY: key_help is a live STATIC while the window is open.
    if let Err(error) = unsafe { SetWindowTextW(rows.key_help, PCWSTR(key_help_text.as_ptr())) } {
        eprintln!("Writely: could not update the Settings help text: {error}");
    }
    if let Some(page) = super::api_key_page_for(rows.provider) {
        let link_text = wide(&format!("<a>{page}</a>"));
        // SAFETY: link is a live SysLink while the window is open.
        if let Err(error) = unsafe { SetWindowTextW(rows.link, PCWSTR(link_text.as_ptr())) } {
            eprintln!("Writely: could not update the Settings link: {error}");
        }
    }
    if rows.provider.needs_key() {
        refresh_key_cue(rows.edit, rows.provider);
    }
    refit_window();
    if is_local {
        render_local();
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
            // The model box still shows the draft provider's model: on a
            // provider change the combo already has the new selection.
            let shown = handles.settings.provider;
            handles.settings.set_model(shown, model.trim());
            handles.settings.provider = provider;
            handles.settings.effort = effort;
            handles.settings.launch_at_login = checkbox_checked(handles.autostart_checkbox);
            handles.settings.clipboard_fallback = checkbox_checked(handles.clipboard_checkbox);
            if !url.trim().is_empty() {
                handles.settings.ollama_url = url.trim().to_string();
            }
        }
    });
}

/// Applies a provider change: files the shown key text, stashes the draft,
/// selects `new_provider`, and shows its draft. `CBN_SELCHANGE` and a tray
/// switch both call this (a programmatic `CB_SETCURSEL` emits no change event).
fn apply_provider_change(new_provider: Provider) {
    STATE.with(|state| {
        if let Some(handles) = state.borrow_mut().as_mut() {
            let shown = handles.settings.provider;
            let text = read_edit_text(handles.edit);
            super::file_key_draft(&mut handles.draft_keys, shown, &text);
        }
    });
    stash_draft();
    let combo = STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|handles| handles.provider_combo)
    });
    if let Some(combo) = combo {
        set_combo_selection(combo, provider_index(new_provider));
    }
    STATE.with(|state| {
        if let Some(handles) = state.borrow_mut().as_mut() {
            handles.settings.provider = new_provider;
        }
    });
    // The text is set after the borrow ends: SetWindowTextW can repaint
    // synchronously, and WM_CTLCOLORSTATIC reads STATE again.
    let pending_text = STATE.with(|state| {
        state.borrow().as_ref().map(|handles| {
            (
                handles.edit,
                super::restore_key_draft(&handles.draft_keys, new_provider).to_string(),
            )
        })
    });
    if let Some((edit, draft)) = pending_text {
        set_edit_text(edit, &draft);
    }
    refresh_provider_rows();
}

/// Follows a tray switch in an open Settings window: the same steps as a
/// manual dropdown change. Returns straight away when Settings is not open.
pub fn provider_switched(provider: Provider) {
    let is_open = STATE.with(|state| state.borrow().is_some());
    if !is_open {
        return;
    }
    apply_provider_change(provider);
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

fn focusable_children(handles: &Handles) -> [HWND; 22] {
    [
        handles.provider_combo,
        handles.model_edit,
        handles.effort_combo,
        handles.edit,
        handles.url_edit,
        handles.link,
        handles.local_model_combo,
        handles.local_download,
        handles.local_cancel,
        handles.local_delete,
        handles.local_warm_checkbox,
        handles.local_idle_edit,
        handles.local_advanced_checkbox,
        handles.adv_edits[0],
        handles.adv_edits[1],
        handles.adv_edits[2],
        handles.adv_edits[3],
        handles.adv_edits[4],
        handles.autostart_checkbox,
        handles.clipboard_checkbox,
        handles.save,
        handles.remove,
    ]
}

extern "system" fn settings_child_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    ref_data: usize,
) -> LRESULT {
    if msg == WM_GETDLGCODE {
        // SAFETY: hwnd is a subclassed child of the Settings window.
        let base = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
        return LRESULT(base.0 | DLGC_WANTTAB as isize);
    }
    if msg == WM_KEYDOWN && wparam.0 == VK_TAB.0 as usize {
        let parent = HWND(ref_data as *mut core::ffi::c_void);
        // SAFETY: VK_SHIFT is a valid virtual-key code for the state query.
        let shift_down = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        // SAFETY: parent is the Settings window and hwnd is one of its children.
        match unsafe { GetNextDlgTabItem(parent, Some(hwnd), shift_down) } {
            Ok(next) if !next.is_invalid() => {
                // SAFETY: next is the next tab-stop child of the Settings window.
                let _previous = unsafe { SetFocus(Some(next)) };
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("Writely: Tab won't move between Settings fields: {error}");
            }
        }
        return LRESULT(0);
    }
    if msg == WM_KEYDOWN && wparam.0 == VK_RETURN.0 as usize {
        let is_edit = STATE.with(|state| {
            state
                .borrow()
                .as_ref()
                .map(|handles| {
                    handles.model_edit == hwnd
                        || handles.edit == hwnd
                        || handles.url_edit == hwnd
                        || handles.local_idle_edit == hwnd
                        || handles.adv_edits.contains(&hwnd)
                })
                .unwrap_or(false)
        });
        if is_edit {
            let parent = HWND(ref_data as *mut core::ffi::c_void);
            // SAFETY: parent is the Settings window; this runs Save's WM_COMMAND code.
            let _sent = unsafe {
                SendMessageW(
                    parent,
                    WM_COMMAND,
                    Some(WPARAM(ID_SAVE as usize)),
                    Some(LPARAM(hwnd.0 as isize)),
                )
            };
            return LRESULT(0);
        }
    }
    // SAFETY: all other messages go to the child's original window procedure.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
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
                            handles.clipboard_checkbox,
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
                    clipboard_checkbox,
                    provider,
                )) = snapshot
                {
                    let model = read_edit_text(model_edit);
                    if provider != Provider::Local {
                        if let Err(error) = crate::config::validate_model_name(&model) {
                            set_status(&error, true);
                            return LRESULT(0);
                        }
                    }
                    let effort = effort_at(combo_selection(effort_combo));
                    let url = read_edit_text(url_edit);
                    let launch_at_login = checkbox_checked(autostart_checkbox);
                    let clipboard_fallback = checkbox_checked(clipboard_checkbox);
                    // Every provider's Save keeps the local values (like
                    // Linux): snapshot the fields, apply them to a clone,
                    // and only write them back on success.
                    let local_snapshot = STATE.with(|state| {
                        state.borrow().as_ref().map(|handles| {
                            (
                                local_model_at(combo_selection(handles.local_model_combo)),
                                checkbox_checked(handles.local_warm_checkbox),
                                super::LocalFields {
                                    idle_minutes: read_edit_text(handles.local_idle_edit),
                                    threads: read_edit_text(handles.adv_edits[0]),
                                    max_reply: read_edit_text(handles.adv_edits[1]),
                                    context: read_edit_text(handles.adv_edits[2]),
                                    startup_secs: read_edit_text(handles.adv_edits[3]),
                                    reply_secs: read_edit_text(handles.adv_edits[4]),
                                },
                                handles.settings.clone(),
                            )
                        })
                    });
                    let Some((local_model, local_warm, local_fields, mut local_applied)) =
                        local_snapshot
                    else {
                        return LRESULT(0);
                    };
                    if let Err(error) = super::apply_local_fields(
                        &mut local_applied,
                        local_model,
                        local_warm,
                        &local_fields,
                    ) {
                        set_status(&error, true);
                        return LRESULT(0);
                    }
                    STATE.with(|state| {
                        if let Some(handles) = state.borrow_mut().as_mut() {
                            handles.settings.local_model = local_applied.local_model;
                            handles.settings.local_warm_up = local_applied.local_warm_up;
                            handles.settings.local_idle_minutes = local_applied.local_idle_minutes;
                            handles.settings.local_threads = local_applied.local_threads;
                            handles.settings.local_max_reply_tokens =
                                local_applied.local_max_reply_tokens;
                            handles.settings.local_context_tokens =
                                local_applied.local_context_tokens;
                            handles.settings.local_startup_timeout_secs =
                                local_applied.local_startup_timeout_secs;
                            handles.settings.local_reply_timeout_secs =
                                local_applied.local_reply_timeout_secs;
                            handles.settings.provider = provider;
                            handles.settings.set_model(provider, model.trim());
                            handles.settings.effort = effort;
                            handles.settings.launch_at_login = launch_at_login;
                            handles.settings.clipboard_fallback = clipboard_fallback;
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
                    // settings.json is written: tell the tray once, even if a
                    // key save fails below.
                    super::notify_changed();
                    if let Err(error) = crate::autostart::set_enabled(launch_at_login) {
                        set_status(&error, true);
                        return LRESULT(0);
                    }
                    super::set_autostart_message(None);
                    // File the box text under the current provider, then save
                    // every non-empty draft under its own provider. On the
                    // first failure the remaining drafts are kept, so nothing
                    // is lost.
                    let value = read_edit_text(edit);
                    STATE.with(|state| {
                        if let Some(handles) = state.borrow_mut().as_mut() {
                            super::file_key_draft(&mut handles.draft_keys, provider, &value);
                        }
                    });
                    let pending = STATE.with(|state| {
                        state
                            .borrow()
                            .as_ref()
                            .map(|handles| super::pending_key_drafts(&handles.draft_keys))
                            .unwrap_or_default()
                    });
                    for (draft_provider, draft_key) in &pending {
                        if let Err(error) = super::save_api_key_for(*draft_provider, draft_key) {
                            set_status(
                                &format!(
                                    "Could not save the {} key: {error}",
                                    draft_provider.display_name()
                                ),
                                true,
                            );
                            return LRESULT(0);
                        }
                        STATE.with(|state| {
                            if let Some(handles) = state.borrow_mut().as_mut() {
                                super::clear_key_draft(&mut handles.draft_keys, *draft_provider);
                            }
                        });
                    }
                    set_status("Saved.", false);
                    set_edit_text(edit, "");
                    refresh_key_cue(edit, provider);
                }
            } else if id == ID_REMOVE {
                let edit = STATE.with(|state| state.borrow().as_ref().map(|handles| handles.edit));
                if let Some(edit) = edit {
                    let provider = current_provider();
                    match super::remove_api_key_for(provider) {
                        Ok(()) => {
                            STATE.with(|state| {
                                if let Some(handles) = state.borrow_mut().as_mut() {
                                    super::clear_key_draft(&mut handles.draft_keys, provider);
                                }
                            });
                            set_edit_text(edit, "");
                            refresh_key_cue(edit, provider);
                            set_status("Key removed.", false);
                            super::notify_changed();
                        }
                        Err(error) => set_status(&error, true),
                    }
                }
            } else if id as u32 == ID_PROVIDER as u32 && notification == CBN_SELCHANGE {
                apply_provider_change(current_provider());
            } else if id as u32 == ID_LOCAL_MODEL as u32 && notification == CBN_SELCHANGE {
                render_local();
            } else if id == ID_LOCAL_DOWNLOAD {
                // The snapshot ends before start_install spawns its thread.
                let selected = STATE.with(|state| {
                    state
                        .borrow()
                        .as_ref()
                        .map(|handles| local_model_at(combo_selection(handles.local_model_combo)))
                });
                if let Some(selected) = selected {
                    if let Err(error) = install::start_install(selected, Arc::new(|| {})) {
                        set_status(&error, true);
                    }
                    render_local();
                }
            } else if id == ID_LOCAL_CANCEL {
                install::cancel_install();
                render_local();
            } else if id == ID_LOCAL_DELETE {
                // No STATE borrow is held across MessageBoxW: it runs its
                // own modal loop and can dispatch messages (including the
                // timer) that read STATE again.
                let snapshot = STATE.with(|state| {
                    state.borrow().as_ref().map(|handles| {
                        let selected = local_model_at(combo_selection(handles.local_model_combo));
                        (
                            handles.window,
                            selected,
                            catalog::model_file(selected).label.to_string(),
                        )
                    })
                });
                if let Some((window, selected, label)) = snapshot {
                    let text = wide(&format!("Delete {label}? You can download it again later."));
                    let caption = wide("Writely");
                    // SAFETY: window is live and both strings outlive the call.
                    let answer = unsafe {
                        MessageBoxW(
                            Some(window),
                            PCWSTR(text.as_ptr()),
                            PCWSTR(caption.as_ptr()),
                            MB_YESNO | MB_ICONQUESTION,
                        )
                    };
                    if answer == IDYES {
                        crate::local::server::stop("model deleted");
                        match install::delete_model(selected) {
                            Ok(()) => {
                                set_status("Deleted.", false);
                                super::notify_changed();
                            }
                            Err(error) => set_status(&error, true),
                        }
                        render_local();
                    }
                }
            } else if id == ID_LOCAL_ADVANCED {
                let checked = STATE.with(|state| {
                    state
                        .borrow()
                        .as_ref()
                        .map(|handles| checkbox_checked(handles.local_advanced_checkbox))
                });
                if let Some(checked) = checked {
                    STATE.with(|state| {
                        if let Some(handles) = state.borrow_mut().as_mut() {
                            handles.show_advanced = checked;
                        }
                    });
                    refresh_provider_rows();
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == ID_LOCAL_TIMER {
                render_local();
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            // SAFETY: lParam points to an NMHDR for WM_NOTIFY.
            let header = unsafe { &*(lparam.0 as *const NMHDR) };
            if header.idFrom == ID_LINK as usize
                && (header.code == NM_CLICK || header.code == NM_RETURN)
            {
                open_key_page(current_provider());
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            let target = HWND(lparam.0 as *mut core::ffi::c_void);
            let (is_help, is_status_error) = STATE.with(|state| {
                state
                    .borrow()
                    .as_ref()
                    .map(|handles| {
                        let helps = [
                            handles.provider_help,
                            handles.model_help,
                            handles.effort_help,
                            handles.key_help,
                            handles.url_help,
                            handles.local_model_help,
                            handles.local_runtime_help,
                            handles.local_warm_help,
                            handles.local_idle_help,
                            handles.autostart_help,
                            handles.clipboard_help,
                        ];
                        (
                            helps.contains(&target) || handles.adv_helps.contains(&target),
                            (handles.status == target && handles.error)
                                || (handles.local_status == target && handles.local_error),
                        )
                    })
                    .unwrap_or((false, false))
            });
            // SAFETY: wparam is the STATIC control device context for this message.
            unsafe {
                let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
                if is_status_error {
                    // RGB(196, 43, 28) as a COLORREF (0x00BBGGRR).
                    SetTextColor(hdc, COLORREF(0x001C2BC4));
                } else if is_help {
                    SetTextColor(hdc, COLORREF(GetSysColor(COLOR_GRAYTEXT)));
                }
                SetBkMode(hdc, TRANSPARENT);
            }
            // SAFETY: the COLOR_WINDOW brush matches the window background.
            LRESULT(unsafe { GetSysColorBrush(COLOR_WINDOW) }.0 as isize)
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
            // SAFETY: hwnd is the settings window that SetTimer armed.
            if let Err(error) = unsafe { KillTimer(Some(hwnd), ID_LOCAL_TIMER) } {
                eprintln!("Writely: could not stop the Settings timer: {error}");
            }
            let children = STATE.with(|state| state.borrow().as_ref().map(focusable_children));
            if let Some(children) = children {
                for child in children {
                    // SAFETY: child was subclassed when the window opened.
                    let _removed = unsafe {
                        RemoveWindowSubclass(child, Some(settings_child_proc), TAB_SUBCLASS_ID)
                    };
                }
            }
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
            if let Some(message) = super::provider_message() {
                set_status(&message, true);
                super::set_provider_message(None);
            }
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
        hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as *mut core::ffi::c_void),
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

    let lay = layout(false, false, false);
    // SAFETY: window is the live Settings window just created above.
    let dpi = unsafe { GetDpiForWindow(window) };
    let dpi = if dpi == 0 { 96 } else { dpi };
    let font = settings_font(dpi);
    // SAFETY: parent is the live settings window and instance is the module handle.
    let make_child_ex = |class: &str,
                         text: &str,
                         style: WINDOW_STYLE,
                         ex_style: WINDOW_EX_STYLE,
                         rect: Rect,
                         id: i32|
     -> Result<HWND, String> {
        let class_wide = wide(class);
        let text_wide = wide(text);
        // SAFETY: class/text strings outlive the call and parent is live.
        unsafe {
            CreateWindowExW(
                ex_style,
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

    let make_child = |class: &str,
                      text: &str,
                      style: WINDOW_STYLE,
                      rect: Rect,
                      id: i32|
     -> Result<HWND, String> {
        make_child_ex(class, text, style, WINDOW_EX_STYLE(0), rect, id)
    };

    let combo_style = WINDOW_STYLE((CBS_DROPDOWNLIST as u32) | WS_VSCROLL.0 | WS_TABSTOP.0);

    let provider_label = make_child("STATIC", "Provider", WINDOW_STYLE(0), lay.provider_label, 0)?;
    let provider_combo = make_child(
        "COMBOBOX",
        "",
        combo_style,
        Rect {
            height: lay.provider_combo.height + 200,
            ..lay.provider_combo
        },
        ID_PROVIDER,
    )?;
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
    let model_edit = make_child_ex(
        "EDIT",
        "",
        WINDOW_STYLE((ES_AUTOHSCROLL as u32) | WS_TABSTOP.0),
        WS_EX_CLIENTEDGE,
        lay.model_edit,
        ID_MODEL,
    )?;
    let model_help = make_child("STATIC", "Paste the model name, e.g. gemini-3.5-flash-lite / deepseek-ai/DeepSeek-V4.1-Flash / llama3.2", WINDOW_STYLE(0), lay.model_help, 0)?;
    let effort_label = make_child("STATIC", "Effort", WINDOW_STYLE(0), lay.effort_label, 0)?;
    let effort_combo = make_child(
        "COMBOBOX",
        "",
        combo_style,
        Rect {
            height: lay.effort_combo.height + 200,
            ..lay.effort_combo
        },
        ID_EFFORT,
    )?;
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
    let edit_style = WINDOW_STYLE((ES_PASSWORD as u32) | (ES_AUTOHSCROLL as u32) | WS_TABSTOP.0);
    let edit = make_child_ex(
        "EDIT",
        "",
        edit_style,
        WS_EX_CLIENTEDGE,
        lay.key_edit,
        ID_EDIT,
    )?;
    let key_help = make_child("STATIC", "Writely sends the sentence you're editing to Google Gemini to check its grammar. Get a free key at ", WINDOW_STYLE(0), lay.key_help, 0)?;
    let default_page = super::api_key_page_for(Provider::Gemini).unwrap_or(super::API_KEY_PAGE);
    let link_text = format!("<a>{default_page}</a>");
    let link = make_child(
        "SysLink",
        &link_text,
        WINDOW_STYLE(WS_TABSTOP.0),
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
    let url_edit = make_child_ex(
        "EDIT",
        "",
        WINDOW_STYLE((ES_AUTOHSCROLL as u32) | WS_TABSTOP.0),
        WS_EX_CLIENTEDGE,
        lay.url_edit,
        ID_URL,
    )?;
    let url_help = make_child(
        "STATIC",
        "Where Ollama runs on your computer. No key needed.",
        WINDOW_STYLE(0),
        lay.url_help,
        0,
    )?;
    // The "On this computer" section (round 7), mirroring Linux. Positions
    // are placeholders here; apply_layout places them for the provider.
    let local_model_label =
        make_child("STATIC", "Model", WINDOW_STYLE(0), lay.local_model_label, 0)?;
    let local_model_combo = make_child(
        "COMBOBOX",
        "",
        combo_style,
        Rect {
            height: lay.local_model_combo.height + 200,
            ..lay.local_model_combo
        },
        ID_LOCAL_MODEL,
    )?;
    for (_id, text) in super::local_model_choices() {
        let entry = wide(&text);
        // SAFETY: local_model_combo is a live COMBOBOX and entry outlives the call.
        unsafe {
            SendMessageW(
                local_model_combo,
                CB_ADDSTRING,
                Some(WPARAM(0)),
                Some(LPARAM(entry.as_ptr() as isize)),
            );
        }
    }
    let local_model_help = make_child(
        "STATIC",
        "Smaller is faster; larger makes fewer mistakes. Your text never leaves this computer.",
        WINDOW_STYLE(0),
        lay.local_model_help,
        0,
    )?;
    let local_status = make_child("STATIC", "", WINDOW_STYLE(0), lay.local_status, 0)?;
    let local_download = make_child(
        "BUTTON",
        "Download",
        WINDOW_STYLE((BS_PUSHBUTTON as u32) | WS_TABSTOP.0),
        lay.local_download,
        ID_LOCAL_DOWNLOAD,
    )?;
    let local_cancel = make_child(
        "BUTTON",
        "Cancel",
        WINDOW_STYLE((BS_PUSHBUTTON as u32) | WS_TABSTOP.0),
        lay.local_cancel,
        ID_LOCAL_CANCEL,
    )?;
    let local_delete = make_child(
        "BUTTON",
        "Delete model",
        WINDOW_STYLE((BS_PUSHBUTTON as u32) | WS_TABSTOP.0),
        lay.local_delete,
        ID_LOCAL_DELETE,
    )?;
    let (local_runtime_help_text, local_runtime_error) = match catalog::runtime_file() {
        Ok(runtime) => (
            format!(
                "The first download also gets the llama.cpp helper ({}). Files are saved in {}.",
                download::human_size(runtime.size),
                catalog::local_data_dir().display()
            ),
            false,
        ),
        Err(error) => (error, true),
    };
    let local_runtime_help = make_child(
        "STATIC",
        &local_runtime_help_text,
        WINDOW_STYLE(0),
        lay.local_runtime_help,
        0,
    )?;
    let local_warm_checkbox = make_child(
        "BUTTON",
        "Load the model when Writely starts",
        WINDOW_STYLE((BS_AUTOCHECKBOX as u32) | WS_TABSTOP.0),
        lay.local_warm,
        ID_LOCAL_WARM,
    )?;
    let local_warm_help = make_child(
        "STATIC",
        "Makes the first check fast. While loaded it uses about 1 GB of memory (2 GB for the 2B model).",
        WINDOW_STYLE(0),
        lay.local_warm_help,
        0,
    )?;
    let local_idle_label = make_child(
        "STATIC",
        "Free memory after (minutes)",
        WINDOW_STYLE(0),
        lay.local_idle_label,
        0,
    )?;
    let number_edit_style =
        WINDOW_STYLE((ES_NUMBER as u32) | (ES_AUTOHSCROLL as u32) | WS_TABSTOP.0);
    let local_idle_edit = make_child_ex(
        "EDIT",
        "",
        number_edit_style,
        WS_EX_CLIENTEDGE,
        lay.local_idle_edit,
        ID_LOCAL_IDLE,
    )?;
    let local_idle_help = make_child(
        "STATIC",
        "Unload the model after this many minutes without use. 0 = never. It loads again in about a second when needed.",
        WINDOW_STYLE(0),
        lay.local_idle_help,
        0,
    )?;
    let local_advanced_checkbox = make_child(
        "BUTTON",
        "Show advanced options",
        WINDOW_STYLE((BS_AUTOCHECKBOX as u32) | WS_TABSTOP.0),
        lay.local_advanced,
        ID_LOCAL_ADVANCED,
    )?;
    const ADV_LABELS: [&str; 5] = [
        "Processor threads",
        "Longest reply (tokens)",
        "Context size (tokens)",
        "Startup time limit (seconds)",
        "Reply time limit (seconds)",
    ];
    const ADV_HELPS: [&str; 5] = [
        "How many processor cores the model uses. 4 suits most computers.",
        "Stops a reply that runs too long. A token is about 4 characters.",
        "How much text the model reads at once. Larger uses more memory.",
        "How long to wait for the model to load.",
        "How long to wait for a reply.",
    ];
    const ADV_IDS: [i32; 5] = [
        ID_LOCAL_THREADS,
        ID_LOCAL_MAX_REPLY,
        ID_LOCAL_CONTEXT,
        ID_LOCAL_STARTUP,
        ID_LOCAL_REPLY,
    ];
    let mut adv_label_handles = Vec::new();
    let mut adv_edit_handles = Vec::new();
    let mut adv_help_handles = Vec::new();
    for i in 0..5 {
        adv_label_handles.push(make_child(
            "STATIC",
            ADV_LABELS[i],
            WINDOW_STYLE(0),
            lay.adv_labels[i],
            0,
        )?);
        adv_edit_handles.push(make_child_ex(
            "EDIT",
            "",
            number_edit_style,
            WS_EX_CLIENTEDGE,
            lay.adv_edits[i],
            ADV_IDS[i],
        )?);
        adv_help_handles.push(make_child(
            "STATIC",
            ADV_HELPS[i],
            WINDOW_STYLE(0),
            lay.adv_helps[i],
            0,
        )?);
    }
    let adv_labels: [HWND; 5] = adv_label_handles.try_into().expect("five advanced labels");
    let adv_edits: [HWND; 5] = adv_edit_handles.try_into().expect("five advanced edits");
    let adv_helps: [HWND; 5] = adv_help_handles.try_into().expect("five advanced helps");
    let autostart_checkbox = make_child(
        "BUTTON",
        "Start Writely at login",
        WINDOW_STYLE((BS_AUTOCHECKBOX as u32) | WS_TABSTOP.0),
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
    let clipboard_checkbox = make_child(
        "BUTTON",
        "Use the clipboard when an app doesn't share its text",
        WINDOW_STYLE((BS_AUTOCHECKBOX as u32) | WS_TABSTOP.0),
        lay.clipboard,
        ID_CLIPBOARD,
    )?;
    let clipboard_help = make_child(
        "STATIC",
        "Writely copies your selection and then puts your clipboard back. Turn off to never touch the clipboard.",
        WINDOW_STYLE(0),
        lay.clipboard_help,
        0,
    )?;
    let save = make_child(
        "BUTTON",
        "Save",
        WINDOW_STYLE((BS_DEFPUSHBUTTON as u32) | WS_TABSTOP.0),
        lay.save,
        ID_SAVE,
    )?;
    let remove = make_child(
        "BUTTON",
        "Remove key",
        WINDOW_STYLE((BS_PUSHBUTTON as u32) | WS_TABSTOP.0),
        lay.remove,
        ID_REMOVE,
    )?;
    let tip = make_child(
        "STATIC",
        "Tip: select text in any app and press Ctrl+Alt+G to check it.",
        WINDOW_STYLE(0),
        lay.tip,
        0,
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
            local_model_label,
            local_model_combo,
            local_model_help,
            local_status,
            local_download,
            local_cancel,
            local_delete,
            local_runtime_help,
            local_warm_checkbox,
            local_warm_help,
            local_idle_label,
            local_idle_edit,
            local_idle_help,
            local_advanced_checkbox,
            adv_labels,
            adv_edits,
            adv_helps,
            autostart_checkbox,
            autostart_help,
            clipboard_checkbox,
            clipboard_help,
            save,
            remove,
            tip,
            status,
            font,
            dpi,
            error: false,
            local_error: false,
            local_runtime_error,
            local_was_busy: false,
            show_advanced: false,
            settings,
            draft_keys: [String::new(), String::new(), String::new()],
        });
    });
    // The saved provider, not the combo's (still empty) selection, so a
    // window saved on "On this computer" opens on its section.
    let provider = STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|handles| handles.settings.provider)
            .unwrap_or(Provider::Gemini)
    });
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
    let clipboard_fallback = STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|handles| handles.settings.clipboard_fallback)
            .unwrap_or(true)
    });
    set_checkbox(clipboard_checkbox, clipboard_fallback);
    // Restore the saved local-model values. "Show advanced options" starts
    // unchecked every time the window opens.
    let local_saved = STATE.with(|state| {
        state.borrow().as_ref().map(|handles| {
            let s = &handles.settings;
            (
                s.local_model,
                s.local_warm_up,
                s.local_idle_minutes,
                [
                    s.local_threads,
                    s.local_max_reply_tokens,
                    s.local_context_tokens,
                    s.local_startup_timeout_secs,
                    s.local_reply_timeout_secs,
                ],
            )
        })
    });
    if let Some((local_model, local_warm, local_idle, local_numbers)) = local_saved {
        set_combo_selection(local_model_combo, local_model_index(local_model));
        set_checkbox(local_warm_checkbox, local_warm);
        set_edit_text(local_idle_edit, &local_idle.to_string());
        for (edit, value) in adv_edits.iter().zip(local_numbers.iter()) {
            set_edit_text(*edit, &value.to_string());
        }
    }
    set_checkbox(local_advanced_checkbox, false);
    STATE.with(|state| {
        if let Some(handles) = state.borrow().as_ref() {
            apply_font(handles);
            apply_layout(handles, dpi);
        }
    });
    size_and_center_window(window, dpi);
    // Subclass every focusable child so Tab moves between fields (tao's
    // loop never calls IsDialogMessageW).
    STATE.with(|state| {
        if let Some(handles) = state.borrow().as_ref() {
            for child in focusable_children(handles) {
                // SAFETY: child is a live child and window outlives the subclass.
                if !unsafe {
                    SetWindowSubclass(
                        child,
                        Some(settings_child_proc),
                        TAB_SUBCLASS_ID,
                        window.0 as usize,
                    )
                }
                .as_bool()
                {
                    let error = windows::core::Error::from_thread();
                    eprintln!("Writely: Tab won't move between Settings fields: {error}");
                }
            }
        }
    });
    refresh_provider_rows();
    render_local();
    // SAFETY: window is live; WM_TIMER re-renders the local download state.
    if unsafe { SetTimer(Some(window), ID_LOCAL_TIMER, 250, None) } == 0 {
        let error = windows::core::Error::from_thread();
        eprintln!(
            "Writely: could not start the Settings timer, so download progress will not update: {error}"
        );
    }
    if let Some(error) = initial_error {
        set_status(&error, true);
    } else if let Some(message) = super::provider_message() {
        set_status(&message, true);
        super::set_provider_message(None);
    } else if let Some(error) = super::autostart_message() {
        set_status(&error, true);
    } else if let Some(error) = super::hotkey_message() {
        set_status(&error, true);
    }
    // SAFETY: window is a live top-level window on this thread.
    unsafe {
        let _ = ShowWindow(window, SW_SHOW);
        let _ = SetForegroundWindow(window);
        // SAFETY: provider_combo is a live child of the Settings window.
        let _previous = SetFocus(Some(provider_combo));
    }
    Ok(())
}
