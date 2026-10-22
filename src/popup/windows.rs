use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::{shows_copy_button, InputKind, PopupTab, TabContent};
use crate::diff::DiffOp;
use tao::event_loop::EventLoopProxy;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    Arc, BeginPaint, CreateFontIndirectW, CreateFontW, CreatePen, DeleteObject, DrawTextW,
    EndPaint, FillRect, GetDC, GetMonitorInfoW, GetStockObject, GetSysColor, GetSysColorBrush,
    InvalidateRect, LineTo, MonitorFromPoint, MoveToEx, Rectangle, ReleaseDC, SelectObject,
    SetBkMode, SetTextColor, CLIP_DEFAULT_PRECIS, COLOR_3DSHADOW, COLOR_BTNFACE, COLOR_GRAYTEXT,
    COLOR_HIGHLIGHT, COLOR_WINDOW, COLOR_WINDOWTEXT, DEFAULT_CHARSET, DEFAULT_QUALITY, DT_CENTER,
    DT_SINGLELINE, DT_VCENTER, FW_BOLD, FW_NORMAL, HDC, HFONT, HGDIOBJ, HOLLOW_BRUSH, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};
use windows::Win32::System::WindowsProgramming::MulDiv;
use windows::Win32::UI::Controls::RichEdit::{
    CFE_BOLD, CFE_STRIKEOUT, CFM_BOLD, CFM_COLOR, CFM_STRIKEOUT, CHARFORMAT2W, CHARRANGE,
    ECOOP_SET, ECO_READONLY, EM_EXSETSEL, EM_FORMATRANGE, EM_SETCHARFORMAT, EM_SETEVENTMASK,
    EM_SETOPTIONS, ENM_REQUESTRESIZE, FORMATRANGE, SCF_SELECTION,
};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, EM_SETSEL, ODS_SELECTED, TOOLTIPS_CLASSW, TTF_IDISHWND, TTF_SUBCLASS,
    TTM_ADDTOOLW, TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP, TTTOOLINFOW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, SystemParametersInfoForDpi, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_CONTROL, VK_ESCAPE, VK_RETURN, VK_SHIFT,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, GetClientRect,
    GetForegroundWindow, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsWindow, KillTimer,
    RegisterClassW, SendMessageW, SetForegroundWindow, SetTimer, SetWindowPos, SetWindowTextW,
    ShowWindow, TrackPopupMenuEx, BN_CLICKED, BS_DEFPUSHBUTTON, BS_OWNERDRAW, BS_PUSHBUTTON,
    CS_DROPSHADOW, CW_USEDEFAULT, ES_AUTOVSCROLL, ES_MULTILINE, HMENU, HWND_TOPMOST, MA_NOACTIVATE,
    MENU_ITEM_FLAGS, MF_CHECKED, MF_DISABLED, MF_SEPARATOR, NONCLIENTMETRICSW,
    SPI_GETNONCLIENTMETRICS, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, TPM_LEFTALIGN,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_TOPALIGN, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CHAR, WM_COMMAND,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM, WM_KEYDOWN, WM_MOUSEACTIVATE, WM_NOTIFY, WM_PAINT,
    WM_SETFONT, WM_TIMER, WNDCLASSW, WS_BORDER, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, WS_VSCROLL,
};

use crate::providers::RewriteStyle;
use crate::text_access::CaretRect;
use crate::UserEvent;

// Logical (96 DPI) card geometry, mirroring `macos.rs:29-39`.
const CARD_WIDTH: i32 = 440;
const SIDE_PAD: i32 = 12;
const HEADER_HEIGHT: i32 = 28;
const FOOTER_HEIGHT: i32 = 40;
const BODY_MIN: i32 = 40;
const BODY_MAX: i32 = 340;
const BUTTON_WIDTH: i32 = 90;
const BUTTON_HEIGHT: i32 = 24;
const COPY_BUTTON_WIDTH: i32 = 28;
const STYLE_BUTTON_WIDTH: i32 = 160;
const ICON_SIZE: i32 = 20;
/// Height of the editable input field, mirroring macOS: about 3 lines.
const INPUT_FIELD_HEIGHT: i32 = 56;

const ID_CLOSE: i32 = 2001;
const ID_CANCEL: i32 = 2002;
const ID_REPLACE: i32 = 2003;
const ID_STYLE: i32 = 2004;
const ID_COPY: i32 = 2005;
const ID_RETRY: i32 = 2006;
const ID_SUBMIT: i32 = 2007;
const SPINNER_TIMER_ID: usize = 1;
const SPINNER_SIZE: i32 = 16;

// RichEdit styles: ES_MULTILINE | ES_READONLY | WS_VSCROLL.
const BODY_STYLE: u32 = 0x0004 | 0x0800 | 0x0020_0000;

static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();
static REGISTERED: AtomicBool = AtomicBool::new(false);
static ROUNDED_LOGGED: AtomicBool = AtomicBool::new(false);

struct PopupState {
    window: HWND,
    header: HWND,
    close: HWND,
    body: HWND,
    loading: bool,
    spinner_step: i32,
    spinner_rect: RECT,
    style: HWND,
    cancel: HWND,
    replace: HWND,
    retry: HWND,
    copy: HWND,
    // Input-box controls: the red error note, the gray hint (shown when
    // there is no note), the editable field, and the Write/Rewrite button.
    note: HWND,
    hint: HWND,
    edit: HWND,
    submit: HWND,
    tooltip: HWND,
    selected: PopupTab,
    font: HFONT,
    bold_font: HFONT,
    copy_font: HFONT,
    dpi: u32,
    error: bool,
    visible: bool,
    // Physical y of the footer separator, or -1 when there is no footer.
    footer_y: i32,
}

thread_local! {
    static STATE: RefCell<Option<PopupState>> = const { RefCell::new(None) };
    static FOCUS_HANDOFF: RefCell<Option<(Instant, bool)>> = const { RefCell::new(None) };
    static PRIMARY_LABEL: RefCell<String> = const { RefCell::new(String::new()) };
    // The foreground window before the input box took focus, for the
    // give-back in every other layout and `hide`.
    static INPUT_FOCUS: RefCell<Option<HWND>> = const { RefCell::new(None) };
    static DRAFT_MODE: RefCell<bool> = const { RefCell::new(false) };
}

// Pure 500 ms window check for a focus handoff at `handoff` seen at `now`.
fn focus_handoff_is_recent(handoff: Instant, now: Instant) -> bool {
    now.checked_duration_since(handoff)
        .map(|elapsed| elapsed < Duration::from_millis(500))
        .unwrap_or(false)
}

fn record_focus_handoff() {
    FOCUS_HANDOFF.with(|handoff| {
        *handoff.borrow_mut() = Some((Instant::now(), false));
    });
}

pub fn focus_handoff_recent() -> bool {
    FOCUS_HANDOFF.with(|handoff| {
        let mut handoff = handoff.borrow_mut();
        let Some((at, logged)) = *handoff else {
            return false;
        };
        if !focus_handoff_is_recent(at, Instant::now()) {
            return false;
        }
        if !logged {
            *handoff = Some((at, true));
            eprintln!("Writely: ignored the focus change from the style menu.");
        }
        true
    })
}

/// Whether the popup currently holds keyboard focus: true while the user
/// types in the input box, so caret events from our own focus switch don't
/// dismiss it.
pub fn input_has_focus() -> bool {
    STATE.with(|state| {
        let popup = state.borrow();
        match popup.as_ref() {
            Some(popup) => {
                // SAFETY: querying the foreground window takes no borrowed state.
                let foreground = unsafe { GetForegroundWindow() };
                foreground == popup.window
            }
            None => false,
        }
    })
}

// Remembers the foreground window before the input box takes focus. When
// the popup itself is in front (an input re-show) the earlier window is
// kept, so Cancel still gives focus back to the user's app.
fn remember_input_focus(window: HWND) {
    // SAFETY: querying the foreground window takes no borrowed state.
    let previous = unsafe { GetForegroundWindow() };
    if previous.is_invalid() || previous == window {
        return;
    }
    INPUT_FOCUS.with(|focus| {
        *focus.borrow_mut() = Some(previous);
    });
}

// Gives focus back to the window that was in front before the input box,
// the way `show_style_menu` does — but only while the popup is still in
// front, so an already-moved focus is never stolen. Always clears the
// remembered window.
fn give_back_input_focus(window: HWND) {
    let previous = INPUT_FOCUS.with(|focus| focus.borrow_mut().take());
    let Some(previous) = previous else {
        return;
    };
    // SAFETY: querying the foreground window takes no borrowed state.
    let foreground = unsafe { GetForegroundWindow() };
    if foreground != window {
        return;
    }
    // SAFETY: previous was the foreground window before the input box.
    if !unsafe { SetForegroundWindow(previous) }.as_bool() {
        let error = windows::core::Error::from_thread();
        eprintln!("Writely: could not give focus back to the previous app: {error}");
    }
    record_focus_handoff();
}

fn draft_mode() -> bool {
    DRAFT_MODE.with(|mode| *mode.borrow())
}

pub fn set_primary_label(label: &str) {
    PRIMARY_LABEL.with(|stored| {
        *stored.borrow_mut() = label.to_string();
    });
    let replace = STATE.with(|state| state.borrow().as_ref().map(|popup| popup.replace));
    if let Some(replace) = replace {
        apply_primary_label(replace);
    }
}

// The stored primary label ("Copy" in terminals, "Replace" elsewhere),
// defaulting to "Replace" like the button's initial text.
fn current_primary_label() -> String {
    PRIMARY_LABEL.with(|stored| {
        let stored = stored.borrow();
        if stored.is_empty() {
            "Replace".to_string()
        } else {
            stored.clone()
        }
    })
}

// The primary label with draft mode applied: "Insert" for a composed
// draft, unless the stored label is already "Copy".
fn effective_primary_label() -> String {
    let stored = current_primary_label();
    if draft_mode() && stored != "Copy" {
        "Insert".to_string()
    } else {
        stored
    }
}

// Applies the primary label to the Replace button, logging a failure.
fn apply_primary_label(replace: HWND) {
    let label = effective_primary_label();
    let value = wide(&label);
    // SAFETY: replace is a live child window on this thread.
    if let Err(error) = unsafe { SetWindowTextW(replace, PCWSTR(value.as_ptr())) } {
        eprintln!("Writely: could not update the suggestion popup: {error}");
    }
}

/// Swaps the copy button between the copy icon and the checkmark, updating
/// its tooltip too. Without the icon font it keeps text labels so the button
/// stays readable.
pub fn set_copy_feedback(copied: bool) {
    STATE.with(|state| {
        let popup = state.borrow();
        let Some(popup) = popup.as_ref() else {
            return;
        };
        let icon = if copied { "\u{E73E}" } else { "\u{E8C8}" };
        let label = if popup.copy_font.0.is_null() {
            if copied {
                "Copied"
            } else {
                "Copy"
            }
        } else {
            icon
        };
        let value = wide(label);
        // SAFETY: copy is a live child window on this thread.
        if let Err(error) = unsafe { SetWindowTextW(popup.copy, PCWSTR(value.as_ptr())) } {
            eprintln!("Writely: could not update the suggestion popup: {error}");
        }
        if popup.tooltip.0.is_null() {
            return;
        }
        let tip = wide(if copied { "Copied" } else { "Copy suggestion" });
        let tool = TTTOOLINFOW {
            cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_IDISHWND | TTF_SUBCLASS,
            hwnd: popup.window,
            uId: popup.copy.0 as usize,
            lpszText: PWSTR(tip.as_ptr() as *mut u16),
            ..Default::default()
        };
        // SAFETY: tooltip is a live tooltip window and tool outlives the call.
        unsafe {
            SendMessageW(
                popup.tooltip,
                TTM_UPDATETIPTEXTW,
                Some(WPARAM(0)),
                Some(LPARAM(&tool as *const TTTOOLINFOW as isize)),
            );
        }
    });
}

pub fn init(proxy: EventLoopProxy<UserEvent>) {
    if PROXY.set(proxy).is_err() {
        // Keep the first proxy installed for this process.
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

// Scales a logical (96 DPI) value to physical pixels at `dpi`.
fn scale(value: i32, dpi: u32) -> i32 {
    // SAFETY: MulDiv is a pure arithmetic function with no pointers.
    unsafe { MulDiv(value, dpi as i32, 96) }
}

fn send_dismiss() {
    if let Some(proxy) = PROXY.get() {
        if let Err(error) = proxy.send_event(UserEvent::PopupDismissed) {
            eprintln!("Writely: could not deliver the dismiss event: {error}");
        }
    }
}

// Sends the input box's text, from Enter in the field or the Write/Rewrite
// button. An empty or whitespace-only field sends nothing.
fn send_input() {
    let edit = STATE.with(|state| state.borrow().as_ref().map(|popup| popup.edit));
    let Some(edit) = edit else {
        return;
    };
    // SAFETY: edit is a live EDIT control on this thread.
    let length = unsafe { GetWindowTextLengthW(edit) };
    if length <= 0 {
        return;
    }
    let mut buffer = vec![0u16; length as usize + 1];
    // SAFETY: edit is a live EDIT control and buffer outlives the call.
    let copied = unsafe { GetWindowTextW(edit, &mut buffer) };
    if copied <= 0 {
        return;
    }
    let text = String::from_utf16_lossy(&buffer[..copied as usize]);
    if text.trim().is_empty() {
        return;
    }
    if let Some(proxy) = PROXY.get() {
        if let Err(error) = proxy.send_event(UserEvent::InputSubmitted(text)) {
            eprintln!("Writely: could not deliver the input event: {error}");
        }
    }
}

fn send_replace() {
    if let Some(proxy) = PROXY.get() {
        if let Err(error) = proxy.send_event(UserEvent::ReplaceClicked) {
            eprintln!("Writely: could not deliver the replace event: {error}");
        }
    }
}

fn send_copy() {
    if let Some(proxy) = PROXY.get() {
        if let Err(error) = proxy.send_event(UserEvent::CopyClicked) {
            eprintln!("Writely: could not deliver the copy event: {error}");
        }
    }
}

fn send_retry() {
    if let Some(proxy) = PROXY.get() {
        if let Err(error) = proxy.send_event(UserEvent::RetryClicked) {
            eprintln!("Writely: could not deliver the retry event: {error}");
        }
    }
}

fn send_tab(tab: PopupTab) {
    if let Some(proxy) = PROXY.get() {
        if let Err(error) = proxy.send_event(UserEvent::TabClicked(tab)) {
            eprintln!("Writely: could not deliver the tab event: {error}");
        }
    }
}

fn tab_label(tab: PopupTab) -> &'static str {
    match tab {
        // Draft mode retitles the Correct tab; the style button and the
        // menu item both read through here.
        PopupTab::Correct if draft_mode() => "Draft",
        PopupTab::Correct => "Fix grammar",
        PopupTab::Rewrite(style) => style.label(),
        PopupTab::Custom => "Describe a change…",
    }
}

/// Menu id for "Describe a change…", after the five rewrite styles.
const CUSTOM_MENU_ID: u32 = 7;

// Menu ids 1..=7: 1 is `Correct`; 2..=6 follow `RewriteStyle::ALL`; 7 is
// `Custom`. Id 0 is reserved for separators, headers and "no selection".
fn menu_id_for_tab(tab: PopupTab) -> u32 {
    match tab {
        PopupTab::Correct => 1,
        PopupTab::Rewrite(style) => RewriteStyle::ALL
            .iter()
            .position(|candidate| *candidate == style)
            .map(|index| index as u32 + 2)
            .unwrap_or(1),
        PopupTab::Custom => CUSTOM_MENU_ID,
    }
}

fn tab_for_menu_id(id: u32) -> Option<PopupTab> {
    match id {
        1 => Some(PopupTab::Correct),
        2..=6 => RewriteStyle::ALL
            .get((id - 2) as usize)
            .map(|style| PopupTab::Rewrite(*style)),
        CUSTOM_MENU_ID => Some(PopupTab::Custom),
        _ => None,
    }
}

// Shows the style menu below the style button, like `build_style_menu` on
// macOS. The popup must be in front for the menu, so focus always goes
// back to the app that was in front before Replace can paste into it.
fn show_style_menu(window: HWND) {
    let Some((selected, style)) = STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|popup| (popup.selected, popup.style))
    }) else {
        return;
    };
    let mut button_rect = RECT::default();
    // SAFETY: style is a live child window and button_rect is writable.
    if let Err(error) = unsafe { GetWindowRect(style, &mut button_rect) } {
        eprintln!("Writely: could not show the style menu: {error}");
        return;
    }
    // SAFETY: querying the foreground window takes no borrowed state.
    let previous = unsafe { GetForegroundWindow() };
    // SAFETY: window is a live popup window on this thread.
    if !unsafe { SetForegroundWindow(window) }.as_bool() {
        eprintln!("Writely: could not bring the popup to the front for the style menu.");
    }
    let chosen = show_style_menu_inner(window, selected, button_rect);
    // Always give focus back, even when the menu failed or was dismissed.
    if !previous.is_invalid() {
        // SAFETY: previous was the foreground window before the menu.
        if !unsafe { SetForegroundWindow(previous) }.as_bool() {
            let error = windows::core::Error::from_thread();
            eprintln!("Writely: could not give focus back to the previous app: {error}");
        }
    }
    record_focus_handoff();
    if let Some(tab) = chosen {
        // Custom always starts another refinement, even when already
        // selected; the other tabs only fire on change.
        if tab != selected || tab == PopupTab::Custom {
            send_tab(tab);
        }
    }
}

fn show_style_menu_inner(window: HWND, selected: PopupTab, button_rect: RECT) -> Option<PopupTab> {
    // SAFETY: CreatePopupMenu takes no borrowed state.
    let menu = match unsafe { CreatePopupMenu() } {
        Ok(menu) => menu,
        Err(error) => {
            eprintln!("Writely: could not show the style menu: {error}");
            return None;
        }
    };
    let append =
        |flags: MENU_ITEM_FLAGS, id: u32, text: &str| -> Result<(), windows::core::Error> {
            let text_wide = wide(text);
            // SAFETY: menu is a live popup menu and the text outlives the call.
            unsafe { AppendMenuW(menu, flags, id as usize, PCWSTR(text_wide.as_ptr())) }
        };
    let checked = |tab: PopupTab| -> MENU_ITEM_FLAGS {
        if tab == selected {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        }
    };
    let builds = [
        append(
            MENU_ITEM_FLAGS(0) | checked(PopupTab::Correct),
            1,
            tab_label(PopupTab::Correct),
        ),
        append(MF_SEPARATOR, 0, ""),
        append(MF_DISABLED, 0, "Change tone"),
        append(
            MENU_ITEM_FLAGS(0) | checked(PopupTab::Rewrite(RewriteStyle::Friendly)),
            menu_id_for_tab(PopupTab::Rewrite(RewriteStyle::Friendly)),
            RewriteStyle::Friendly.label(),
        ),
        append(
            MENU_ITEM_FLAGS(0) | checked(PopupTab::Rewrite(RewriteStyle::Formal)),
            menu_id_for_tab(PopupTab::Rewrite(RewriteStyle::Formal)),
            RewriteStyle::Formal.label(),
        ),
        append(
            MENU_ITEM_FLAGS(0) | checked(PopupTab::Rewrite(RewriteStyle::Professional)),
            menu_id_for_tab(PopupTab::Rewrite(RewriteStyle::Professional)),
            RewriteStyle::Professional.label(),
        ),
        append(MF_SEPARATOR, 0, ""),
        append(MF_DISABLED, 0, "Change length"),
        append(
            MENU_ITEM_FLAGS(0) | checked(PopupTab::Rewrite(RewriteStyle::Shorter)),
            menu_id_for_tab(PopupTab::Rewrite(RewriteStyle::Shorter)),
            RewriteStyle::Shorter.label(),
        ),
        append(
            MENU_ITEM_FLAGS(0) | checked(PopupTab::Rewrite(RewriteStyle::Longer)),
            menu_id_for_tab(PopupTab::Rewrite(RewriteStyle::Longer)),
            RewriteStyle::Longer.label(),
        ),
        append(MF_SEPARATOR, 0, ""),
        append(
            MENU_ITEM_FLAGS(0) | checked(PopupTab::Custom),
            menu_id_for_tab(PopupTab::Custom),
            tab_label(PopupTab::Custom),
        ),
    ];
    let mut chosen = None;
    if let Err(error) = builds
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map(|_| ())
    {
        eprintln!("Writely: could not build the style menu: {error}");
    } else {
        // SAFETY: menu is a live popup menu and window is its live owner.
        let picked = unsafe {
            TrackPopupMenuEx(
                menu,
                (TPM_RETURNCMD | TPM_NONOTIFY | TPM_LEFTALIGN | TPM_TOPALIGN).0,
                button_rect.left,
                button_rect.bottom,
                window,
                None,
            )
        };
        chosen = tab_for_menu_id(picked.0 as u32);
    }
    // SAFETY: menu was created above and is destroyed exactly once here.
    if let Err(error) = unsafe { DestroyMenu(menu) } {
        eprintln!("Writely: could not close the style menu: {error}");
    }
    chosen
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
            if code == BN_CLICKED {
                match id {
                    ID_CLOSE | ID_CANCEL => send_dismiss(),
                    ID_REPLACE => send_replace(),
                    ID_COPY => send_copy(),
                    ID_RETRY => send_retry(),
                    ID_SUBMIT => send_input(),
                    ID_STYLE => show_style_menu(window),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            // EN_REQUESTRESIZE only tells us the body wants a new height;
            // sizing is measured with EM_FORMATRANGE on show, so there is
            // nothing to do here beyond acknowledging it.
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            let target = HWND(lparam.0 as *mut core::ffi::c_void);
            let colour = STATE.with(|state| {
                state.borrow().as_ref().and_then(|popup| {
                    if target == popup.header || target == popup.hint {
                        // SAFETY: COLOR_GRAYTEXT is a valid system colour index.
                        Some(unsafe { GetSysColor(COLOR_GRAYTEXT) })
                    } else if target == popup.note {
                        // The error red, like the body error colour.
                        Some(0x001C2BC4)
                    } else {
                        None
                    }
                })
            });
            if let Some(colour) = colour {
                // SAFETY: wparam is the STATIC control device context for this message.
                unsafe {
                    SetTextColor(HDC(wparam.0 as *mut core::ffi::c_void), COLORREF(colour));
                    SetBkMode(HDC(wparam.0 as *mut core::ffi::c_void), TRANSPARENT);
                    // SAFETY: COLOR_WINDOW is a valid system colour index.
                    return LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize);
                }
            }
            // SAFETY: falling through to the default procedure is valid for this message.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        WM_PAINT => {
            // SAFETY: this window is being painted and ps is writable.
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(window, &mut ps);
                if !dc.is_invalid() {
                    let mut client = RECT::default();
                    let _ = GetClientRect(window, &mut client);
                    // 1 px border in COLOR_3DSHADOW around the card.
                    // SAFETY: COLOR_3DSHADOW is a valid system colour index.
                    let pen = CreatePen(PS_SOLID, 1, COLORREF(GetSysColor(COLOR_3DSHADOW)));
                    let old_pen = SelectObject(dc, HGDIOBJ(pen.0));
                    // SAFETY: HOLLOW_BRUSH is a stock object that must not be deleted.
                    let old_brush = SelectObject(dc, GetStockObject(HOLLOW_BRUSH));
                    let _ = Rectangle(dc, client.left, client.top, client.right, client.bottom);
                    let footer_y = STATE.with(|state| {
                        state
                            .borrow()
                            .as_ref()
                            .map(|popup| popup.footer_y)
                            .unwrap_or(-1)
                    });
                    if footer_y >= 0 {
                        // 1 px separator above the footer.
                        let _ = MoveToEx(dc, client.left, footer_y, None);
                        let _ = LineTo(dc, client.right, footer_y);
                    }
                    let spinner = STATE.with(|state| {
                        state.borrow().as_ref().map(|popup| {
                            (
                                popup.loading,
                                popup.spinner_step,
                                popup.spinner_rect,
                                popup.dpi,
                            )
                        })
                    });
                    if let Some((true, step, rect, dpi)) = spinner {
                        // 270° ring in COLOR_HIGHLIGHT, rotated by `step`.
                        // SAFETY: COLOR_HIGHLIGHT is a valid system colour index.
                        let spinner_pen = CreatePen(
                            PS_SOLID,
                            scale(2, dpi),
                            COLORREF(GetSysColor(COLOR_HIGHLIGHT)),
                        );
                        let old_spinner_pen = SelectObject(dc, HGDIOBJ(spinner_pen.0));
                        let cx = (rect.left + rect.right) as f64 / 2.0;
                        let cy = (rect.top + rect.bottom) as f64 / 2.0;
                        let radius = ((rect.right - rect.left).min(rect.bottom - rect.top) as f64)
                            / 2.0
                            - 1.0;
                        let point = |degrees: i32| -> (i32, i32) {
                            let radians = (degrees as f64).to_radians();
                            (
                                (cx + radius * radians.cos()) as i32,
                                (cy - radius * radians.sin()) as i32,
                            )
                        };
                        let (x1, y1) = point(step);
                        let (x2, y2) = point(step + 270);
                        let _ = Arc(
                            dc,
                            rect.left,
                            rect.top,
                            rect.right,
                            rect.bottom,
                            x1,
                            y1,
                            x2,
                            y2,
                        );
                        let _ = SelectObject(dc, old_spinner_pen);
                        let _ = DeleteObject(HGDIOBJ(spinner_pen.0));
                    }
                    let _ = SelectObject(dc, old_pen);
                    let _ = SelectObject(dc, old_brush);
                    let _ = DeleteObject(HGDIOBJ(pen.0));
                }
                let _ = EndPaint(window, &ps);
            }
            LRESULT(0)
        }
        WM_DRAWITEM => {
            if (wparam.0 & 0xffff) as i32 == ID_CLOSE {
                // SAFETY: lParam points to a DRAWITEMSTRUCT for WM_DRAWITEM.
                let item = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
                let font = STATE.with(|state| {
                    state
                        .borrow()
                        .as_ref()
                        .map(|popup| popup.font)
                        .unwrap_or_default()
                });
                // SAFETY: item.hDC and item.rcItem come from the owner-draw message.
                unsafe {
                    let brush = if item.itemState.0 & ODS_SELECTED.0 != 0 {
                        GetSysColorBrush(COLOR_BTNFACE)
                    } else {
                        GetSysColorBrush(COLOR_WINDOW)
                    };
                    FillRect(item.hDC, &item.rcItem, brush);
                    let old_font = if font.0.is_null() {
                        HGDIOBJ::default()
                    } else {
                        SelectObject(item.hDC, HGDIOBJ(font.0))
                    };
                    SetTextColor(item.hDC, COLORREF(GetSysColor(COLOR_GRAYTEXT)));
                    SetBkMode(item.hDC, TRANSPARENT);
                    let mut text = wide("\u{2715}");
                    let mut rect = item.rcItem;
                    DrawTextW(
                        item.hDC,
                        &mut text,
                        &mut rect,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                    );
                    if !font.0.is_null() {
                        SelectObject(item.hDC, old_font);
                    }
                }
                return LRESULT(1);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == SPINNER_TIMER_ID {
                let rect = STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    let popup = state.as_mut()?;
                    if !popup.loading || !popup.visible {
                        return None;
                    }
                    popup.spinner_step = (popup.spinner_step + 30) % 360;
                    Some(popup.spinner_rect)
                });
                if let Some(rect) = rect {
                    // SAFETY: window is a live popup window and rect is its spinner area.
                    unsafe {
                        let _ = InvalidateRect(Some(window), Some(&rect), false);
                    }
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: window is a live popup window on this thread.
            let _stopped = unsafe { KillTimer(Some(window), SPINNER_TIMER_ID) };
            let fonts = STATE.with(|state| {
                state
                    .borrow_mut()
                    .take()
                    .map(|popup| (popup.font, popup.bold_font, popup.copy_font))
            });
            if let Some((font, bold_font, copy_font)) = fonts {
                delete_font(font);
                delete_font(bold_font);
                delete_font(copy_font);
            }
            // SAFETY: default handling for window teardown.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        // SAFETY: all other messages use the default window procedure.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

// Keys in the input box's EDIT: Enter submits, Shift+Enter inserts a line
// break (the default), Esc dismisses, Ctrl+A selects all.
unsafe extern "system" fn edit_subclass_proc(
    edit: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if message == WM_KEYDOWN {
        let key = wparam.0 as u16;
        if key == VK_RETURN.0 {
            // SAFETY: GetKeyState takes a virtual-key code and no pointers.
            let shift_down = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            if !shift_down {
                send_input();
                return LRESULT(0);
            }
        } else if key == VK_ESCAPE.0 {
            send_dismiss();
            return LRESULT(0);
        } else if key == 0x41 {
            // SAFETY: GetKeyState takes a virtual-key code and no pointers.
            let ctrl_down = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            if ctrl_down {
                // SAFETY: edit is a live EDIT control on this thread.
                unsafe {
                    SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
                }
                return LRESULT(0);
            }
        }
    }
    if message == WM_CHAR {
        // Swallow the character for each key handled above, so the EDIT
        // doesn't insert a stray line break or beep. Shift+Enter keeps its
        // default line break.
        let ch = wparam.0 as u32;
        if ch == '\r' as u32 {
            // SAFETY: GetKeyState takes a virtual-key code and no pointers.
            let shift_down = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            if !shift_down {
                return LRESULT(0);
            }
        } else if ch == 0x1B || ch == 0x01 {
            return LRESULT(0);
        }
    }
    // SAFETY: edit, wparam and lparam come from the subclass message.
    unsafe { DefSubclassProc(edit, message, wparam, lparam) }
}

fn delete_font(font: HFONT) {
    if font.0.is_null() {
        return;
    }
    // SAFETY: font was created by CreateFontIndirectW and is no longer in use.
    let deleted = unsafe { DeleteObject(HGDIOBJ(font.0)) };
    if !deleted.as_bool() {
        eprintln!("Writely: could not delete the popup font.");
    }
}

// Loads the system message font at `dpi`, bold when asked.
fn load_font(dpi: u32, bold: bool) -> Result<HFONT, windows::core::Error> {
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
    if bold {
        metrics.lfMessageFont.lfWeight = FW_BOLD.0 as i32;
    }
    // SAFETY: lfMessageFont was filled by SystemParametersInfoForDpi above.
    let font = unsafe { CreateFontIndirectW(&metrics.lfMessageFont) };
    if font.0.is_null() {
        return Err(windows::core::Error::from_thread());
    }
    Ok(font)
}

fn popup_font(dpi: u32, bold: bool) -> HFONT {
    match load_font(dpi, bold) {
        Ok(font) => font,
        Err(error) => {
            eprintln!("Writely: could not load the popup font: {error}");
            HFONT::default()
        }
    }
}

// Loads the Segoe MDL2 Assets icon font at `dpi`, sized like the popup font.
fn load_copy_font(dpi: u32) -> Result<HFONT, windows::core::Error> {
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
    let face = wide("Segoe MDL2 Assets");
    // SAFETY: the face name outlives the call; the rest are plain values.
    let font = unsafe {
        CreateFontW(
            metrics.lfMessageFont.lfHeight,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            DEFAULT_QUALITY,
            0,
            PCWSTR(face.as_ptr()),
        )
    };
    if font.0.is_null() {
        return Err(windows::core::Error::from_thread());
    }
    Ok(font)
}

// Recreates the fonts when the DPI changes, freeing the old ones, and
// sends WM_SETFONT to every child.
fn refresh_fonts(dpi: u32) {
    let needs = STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|popup| popup.dpi != dpi || popup.font.0.is_null() || popup.copy_font.0.is_null())
            .unwrap_or(false)
    });
    if !needs {
        return;
    }
    let font = popup_font(dpi, false);
    let bold_font = popup_font(dpi, true);
    let copy_font = match load_copy_font(dpi) {
        Ok(font) => font,
        Err(error) => {
            eprintln!("Writely: could not load the copy icon font: {error}");
            HFONT::default()
        }
    };
    let old = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let popup = state.as_mut()?;
        let old = (popup.font, popup.bold_font, popup.copy_font);
        popup.font = font;
        popup.bold_font = bold_font;
        popup.copy_font = copy_font;
        popup.dpi = dpi;
        let children = [
            (popup.header, bold_font),
            (popup.close, font),
            (popup.body, font),
            (popup.style, font),
            (popup.cancel, font),
            (popup.replace, font),
            (popup.retry, font),
            (popup.copy, copy_font),
            (popup.note, font),
            (popup.hint, font),
            (popup.edit, font),
            (popup.submit, font),
        ];
        for (child, child_font) in children {
            if !child_font.0.is_null() {
                // SAFETY: child is a live child window and the font outlives the call.
                unsafe {
                    SendMessageW(
                        child,
                        WM_SETFONT,
                        Some(WPARAM(child_font.0 as usize)),
                        Some(LPARAM(1)),
                    );
                }
            }
        }
        Some(old)
    });
    if let Some((old_font, old_bold, old_copy)) = old {
        if old_font != font {
            delete_font(old_font);
        }
        if old_bold != bold_font {
            delete_font(old_bold);
        }
        if old_copy != copy_font {
            delete_font(old_copy);
        }
    }
}

// DPI of the monitor at the anchor point, 96 when it cannot be read.
fn dpi_for_anchor(caret: &CaretRect) -> u32 {
    let point = POINT {
        x: caret.x as i32,
        y: caret.y as i32,
    };
    // SAFETY: point is plain integers for the monitor lookup below.
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
    let mut dpi_x: u32 = 0;
    let mut dpi_y: u32 = 0;
    // SAFETY: dpi_x/dpi_y are writable u32s for the monitor DPI query.
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_ok()
        && dpi_x > 0
    {
        dpi_x
    } else {
        96
    }
}

fn ensure_window() -> Option<HWND> {
    if let Some(window) = STATE.with(|state| state.borrow().as_ref().map(|popup| popup.window)) {
        // SAFETY: window came from STATE.
        if unsafe { IsWindow(Some(window)) }.as_bool() {
            return Some(window);
        }
        STATE.with(|state| {
            *state.borrow_mut() = None;
        });
    }
    // The rich text control lives in Msftedit.dll; without it there is no
    // body control, so fail like any other creation failure (no fallback).
    // SAFETY: the DLL name is a live null-terminated string.
    if let Err(error) = unsafe { LoadLibraryW(PCWSTR(wide("Msftedit.dll").as_ptr())) } {
        eprintln!("Writely: could not load the rich text control: {error}");
        return None;
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
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(popup_proc),
            hInstance: HINSTANCE(module.0),
            // SAFETY: COLOR_WINDOW is a valid system colour index.
            hbrBackground: unsafe { GetSysColorBrush(COLOR_WINDOW) },
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
            scale(CARD_WIDTH, 96),
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
    // Rounded corners on Windows 11; Windows 10 keeps square corners.
    let preference = DWMWCP_ROUND;
    // SAFETY: preference is a live DWM_WINDOW_CORNER_PREFERENCE for the call.
    if let Err(error) = unsafe {
        DwmSetWindowAttribute(
            window,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const DWM_WINDOW_CORNER_PREFERENCE as *const core::ffi::c_void,
            std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        )
    } {
        if !ROUNDED_LOGGED.swap(true, Ordering::SeqCst) {
            eprintln!("Writely: rounded popup corners aren't available: {error}");
        }
    }
    let make_child = |class: &str,
                      text: &str,
                      ex_style: WINDOW_EX_STYLE,
                      style: WINDOW_STYLE,
                      id: i32|
     -> Option<HWND> {
        let class_wide = wide(class);
        let text_wide = wide(text);
        // SAFETY: class/text strings outlive the call and the parent is live.
        match unsafe {
            CreateWindowExW(
                ex_style,
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
    let header = make_child("STATIC", "Writely", WINDOW_EX_STYLE(0), WINDOW_STYLE(0), 0)?;
    let close = make_child(
        "BUTTON",
        "\u{2715}",
        WINDOW_EX_STYLE(0),
        WINDOW_STYLE(BS_OWNERDRAW as u32),
        ID_CLOSE,
    )?;
    let body = make_child(
        "RICHEDIT50W",
        "",
        WINDOW_EX_STYLE(0),
        WINDOW_STYLE(BODY_STYLE),
        0,
    )?;
    // Read-only with no caret: focus stays in the user's text field.
    // SAFETY: body is a live RichEdit control on this thread.
    unsafe {
        SendMessageW(
            body,
            EM_SETOPTIONS,
            Some(WPARAM(ECOOP_SET as usize)),
            Some(LPARAM(ECO_READONLY as isize)),
        );
        SendMessageW(
            body,
            EM_SETEVENTMASK,
            Some(WPARAM(0)),
            Some(LPARAM(ENM_REQUESTRESIZE as isize)),
        );
    }
    let style = make_child(
        "BUTTON",
        "Fix grammar \u{25be}",
        WINDOW_EX_STYLE(0),
        WINDOW_STYLE(BS_PUSHBUTTON as u32),
        ID_STYLE,
    )?;
    let cancel = make_child(
        "BUTTON",
        "Cancel",
        WINDOW_EX_STYLE(0),
        WINDOW_STYLE(BS_PUSHBUTTON as u32),
        ID_CANCEL,
    )?;
    let copy = make_child(
        "BUTTON",
        "\u{E8C8}",
        WINDOW_EX_STYLE(0),
        WINDOW_STYLE(BS_PUSHBUTTON as u32),
        ID_COPY,
    )?;
    let replace = make_child(
        "BUTTON",
        "Replace",
        WINDOW_EX_STYLE(0),
        WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
        ID_REPLACE,
    )?;
    let retry = make_child(
        "BUTTON",
        "Retry",
        WINDOW_EX_STYLE(0),
        WINDOW_STYLE(BS_PUSHBUTTON as u32),
        ID_RETRY,
    )?;
    // Input-box controls, created hidden and shown only by `show_input`.
    // The EDIT wraps (no ES_AUTOHSCROLL) and scrolls vertically.
    let note = make_child("STATIC", "", WINDOW_EX_STYLE(0), WINDOW_STYLE(0), 0)?;
    let hint = make_child("STATIC", "", WINDOW_EX_STYLE(0), WINDOW_STYLE(0), 0)?;
    let edit = make_child(
        "EDIT",
        "",
        WINDOW_EX_STYLE(0),
        WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL) as u32) | WS_VSCROLL | WS_BORDER,
        0,
    )?;
    // SAFETY: edit is a live EDIT control; the subclass dies with the window.
    if !unsafe { SetWindowSubclass(edit, Some(edit_subclass_proc), 1, 0) }.as_bool() {
        let error = windows::core::Error::from_thread();
        eprintln!("Writely: the popup text box may not take the Enter key: {error}");
    }
    let submit = make_child(
        "BUTTON",
        "Write",
        WINDOW_EX_STYLE(0),
        WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
        ID_SUBMIT,
    )?;
    show_child(note, false);
    show_child(hint, false);
    show_child(edit, false);
    show_child(submit, false);
    // Owned tooltip window for the copy button ("Copy suggestion" / "Copied").
    // It is destroyed with the popup, so there is nothing to free.
    let tooltip = match unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            PCWSTR::null(),
            WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            Some(window),
            None,
            Some(instance),
            None,
        )
    } {
        Ok(tooltip) => tooltip,
        Err(error) => {
            eprintln!("Writely: could not create the copy tooltip: {error}");
            HWND::default()
        }
    };
    if !tooltip.0.is_null() {
        let text = wide("Copy suggestion");
        let tool = TTTOOLINFOW {
            cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_IDISHWND | TTF_SUBCLASS,
            hwnd: window,
            uId: copy.0 as usize,
            lpszText: PWSTR(text.as_ptr() as *mut u16),
            ..Default::default()
        };
        // SAFETY: tooltip is a live tooltip window and tool outlives the call.
        let added = unsafe {
            SendMessageW(
                tooltip,
                TTM_ADDTOOLW,
                Some(WPARAM(0)),
                Some(LPARAM(&tool as *const TTTOOLINFOW as isize)),
            )
        };
        if added.0 == 0 {
            eprintln!("Writely: could not add the copy tooltip.");
        }
    }
    STATE.with(|state| {
        *state.borrow_mut() = Some(PopupState {
            window,
            header,
            close,
            body,
            loading: false,
            spinner_step: 0,
            spinner_rect: RECT::default(),
            style,
            cancel,
            replace,
            retry,
            copy,
            note,
            hint,
            edit,
            submit,
            tooltip,
            selected: PopupTab::Correct,
            font: HFONT::default(),
            bold_font: HFONT::default(),
            copy_font: HFONT::default(),
            dpi: 0,
            error: false,
            visible: false,
            footer_y: -1,
        });
    });
    Some(window)
}

// Positions a child from a logical rect, logging a failure the user would see.
fn set_child_pos(child: HWND, x: i32, y: i32, width: i32, height: i32, dpi: u32) {
    // SAFETY: child is a live child window on this thread.
    if let Err(error) = unsafe {
        SetWindowPos(
            child,
            None,
            scale(x, dpi),
            scale(y, dpi),
            scale(width, dpi),
            scale(height, dpi),
            SWP_NOACTIVATE,
        )
    } {
        eprintln!("Writely: could not position a popup control: {error}");
    }
}

fn show_child(child: HWND, show: bool) {
    // SAFETY: child is a live child window; ShowWindow reports the previous
    // visibility, not success.
    let _previously_visible =
        unsafe { ShowWindow(child, if show { SW_SHOWNOACTIVATE } else { SW_HIDE }) };
}

// Sets the body's plain text and colour, returning false when the text
// could not be set.
fn set_plain_text(body: HWND, text: &str, is_error: bool) -> bool {
    let value = wide(text);
    // SAFETY: body is a live RichEdit child window.
    if let Err(error) = unsafe { SetWindowTextW(body, PCWSTR(value.as_ptr())) } {
        eprintln!("Writely: could not update the suggestion popup: {error}");
        return false;
    }
    set_text_colour(body, is_error);
    true
}

fn set_text_colour(body: HWND, is_error: bool) {
    let colour = if is_error {
        COLORREF(0x001C2BC4)
    } else {
        // SAFETY: COLOR_WINDOWTEXT is a valid system colour index.
        COLORREF(unsafe { GetSysColor(COLOR_WINDOWTEXT) })
    };
    let mut format = CHARFORMAT2W::default();
    format.Base.cbSize = std::mem::size_of::<CHARFORMAT2W>() as u32;
    format.Base.dwMask = windows::Win32::UI::Controls::RichEdit::CFM_COLOR;
    format.Base.crTextColor = colour;
    let all = CHARRANGE {
        cpMin: 0,
        cpMax: -1,
    };
    // SAFETY: body is a live RichEdit and format/all outlive the calls.
    unsafe {
        SendMessageW(
            body,
            EM_EXSETSEL,
            Some(WPARAM(0)),
            Some(LPARAM(&all as *const CHARRANGE as isize)),
        );
        SendMessageW(
            body,
            EM_SETCHARFORMAT,
            Some(WPARAM(SCF_SELECTION as usize)),
            Some(LPARAM(&format as *const CHARFORMAT2W as isize)),
        );
        let caret = CHARRANGE { cpMin: 0, cpMax: 0 };
        SendMessageW(
            body,
            EM_EXSETSEL,
            Some(WPARAM(0)),
            Some(LPARAM(&caret as *const CHARRANGE as isize)),
        );
    }
}

// Needed body height in logical px for the current body text, measured
// with EM_FORMATRANGE and clamped to BODY_MIN..=BODY_MAX.
fn measure_body_height(body: HWND, dpi: u32) -> i32 {
    // SAFETY: body is a live RichEdit child window on this thread.
    let dc = unsafe { GetDC(Some(body)) };
    if dc.is_invalid() {
        return BODY_MIN;
    }
    let width_physical = scale(CARD_WIDTH - 2 * SIDE_PAD, dpi);
    // FORMATRANGE works in twips (1/1440 inch).
    let width_twips = width_physical * 1440 / dpi.max(1) as i32;
    let mut range = FORMATRANGE {
        hdc: dc,
        hdcTarget: dc,
        rc: RECT {
            left: 0,
            top: 0,
            right: width_twips,
            bottom: 1_000_000,
        },
        rcPage: RECT {
            left: 0,
            top: 0,
            right: width_twips,
            bottom: 1_000_000,
        },
        chrg: CHARRANGE {
            cpMin: 0,
            cpMax: -1,
        },
    };
    // SAFETY: range is a live FORMATRANGE and dc came from GetDC above.
    unsafe {
        SendMessageW(
            body,
            EM_FORMATRANGE,
            Some(WPARAM(0)),
            Some(LPARAM(&mut range as *mut FORMATRANGE as isize)),
        );
        // Free the format cache.
        SendMessageW(body, EM_FORMATRANGE, Some(WPARAM(0)), Some(LPARAM(0)));
        ReleaseDC(Some(body), dc);
    }
    // Twips to logical px: * 96 / 1440, rounding up.
    let logical = (range.rc.bottom * 96 + 1439) / 1440;
    logical.clamp(BODY_MIN, BODY_MAX)
}

// Positions the card at the caret (right of the icon, flipping above when
// there is no room) and shows it without activation.
fn show_card(window: HWND, caret: &CaretRect, width: i32, height: i32) -> bool {
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
    if let Err(error) = unsafe {
        SetWindowPos(
            window,
            Some(HWND_TOPMOST),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE,
        )
    } {
        eprintln!("Writely: could not move the suggestion popup: {error}");
        return false;
    }
    STATE.with(|state| {
        if let Some(popup) = state.borrow_mut().as_mut() {
            if !popup.visible {
                // SAFETY: window is a live popup window on this thread.
                // SW_SHOWNOACTIVATE shows it without activating Writely.
                let _previously_visible = unsafe { ShowWindow(window, SW_SHOWNOACTIVATE) };
                popup.visible = true;
            }
        }
    });
    // SAFETY: window is a live popup window on this thread.
    unsafe {
        let _ = InvalidateRect(Some(window), None, true);
    }
    true
}

#[derive(Clone, Copy)]
struct ChildHandles {
    header: HWND,
    close: HWND,
    body: HWND,
    style: HWND,
    cancel: HWND,
    replace: HWND,
    retry: HWND,
    copy: HWND,
    note: HWND,
    hint: HWND,
    edit: HWND,
    submit: HWND,
}

// Copies the child handles out of STATE for one show.
fn handles() -> Option<ChildHandles> {
    STATE.with(|state| {
        state.borrow().as_ref().map(|popup| ChildHandles {
            header: popup.header,
            close: popup.close,
            body: popup.body,
            style: popup.style,
            cancel: popup.cancel,
            replace: popup.replace,
            retry: popup.retry,
            copy: popup.copy,
            note: popup.note,
            hint: popup.hint,
            edit: popup.edit,
            submit: popup.submit,
        })
    })
}

// Height of the input box's note/hint line for `text`: wraps to about 56
// chars per line on the card, like macOS; capped so a long provider error
// can't fill the card.
fn note_box_height(text: &str) -> i32 {
    const CHARS_PER_LINE: usize = 56;
    const LINE_HEIGHT: i32 = 15;
    const MAX_LINES: usize = 6;
    let lines = text
        .chars()
        .count()
        .max(1)
        .div_ceil(CHARS_PER_LINE)
        .min(MAX_LINES);
    lines as i32 * LINE_HEIGHT
}

fn start_spinner(window: HWND) {
    STATE.with(|state| {
        if let Some(popup) = state.borrow_mut().as_mut() {
            popup.loading = true;
            popup.spinner_step = 0;
        }
    });
    // SAFETY: window is a live popup window on this thread.
    if unsafe { SetTimer(Some(window), SPINNER_TIMER_ID, 50, None) } == 0 {
        eprintln!("Writely: could not start the popup spinner.");
    }
}

fn stop_spinner(window: HWND) {
    STATE.with(|state| {
        if let Some(popup) = state.borrow_mut().as_mut() {
            popup.loading = false;
        }
    });
    // SAFETY: window is a live popup window on this thread.
    let _stopped = unsafe { KillTimer(Some(window), SPINNER_TIMER_ID) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffStyle {
    Equal,
    Delete,
    Insert,
}

// Turns the word diff into styled runs for the RichEdit. The diff tokens
// already carry their whitespace, so concatenating the runs keeps the
// spacing between tokens exactly as `build_diff_attributed` does on macOS.
fn diff_runs(original: &str, suggestion: &str) -> Vec<(String, DiffStyle)> {
    crate::diff::diff_words(original, suggestion)
        .into_iter()
        .map(|op| match op {
            DiffOp::Equal(text) => (text, DiffStyle::Equal),
            DiffOp::Delete(text) => (text, DiffStyle::Delete),
            DiffOp::Insert(text) => (text, DiffStyle::Insert),
        })
        .collect()
}

// Sets the body to the styled word diff, returning false when the text
// could not be set.
fn set_diff_text(body: HWND, original: &str, suggestion: &str) -> bool {
    let runs = diff_runs(original, suggestion);
    let display: String = runs.iter().map(|(text, _)| text.as_str()).collect();
    let value = wide(&display);
    // SAFETY: body is a live RichEdit child window.
    if let Err(error) = unsafe { SetWindowTextW(body, PCWSTR(value.as_ptr())) } {
        eprintln!("Writely: could not update the suggestion popup: {error}");
        return false;
    }
    let mut offset: i32 = 0;
    for (text, style) in &runs {
        let length = text.encode_utf16().count() as i32;
        if length == 0 {
            continue;
        }
        let (colour, effects) = match style {
            DiffStyle::Equal => (
                // SAFETY: COLOR_WINDOWTEXT is a valid system colour index.
                COLORREF(unsafe { GetSysColor(COLOR_WINDOWTEXT) }),
                windows::Win32::UI::Controls::RichEdit::CFE_EFFECTS(0),
            ),
            // RGB(196, 43, 28), struck out.
            DiffStyle::Delete => (COLORREF(0x001C_2BC4), CFE_STRIKEOUT),
            // RGB(16, 124, 16), bold.
            DiffStyle::Insert => (COLORREF(0x0010_7C10), CFE_BOLD),
        };
        let mut format = CHARFORMAT2W::default();
        format.Base.cbSize = std::mem::size_of::<CHARFORMAT2W>() as u32;
        format.Base.dwMask = CFM_COLOR | CFM_BOLD | CFM_STRIKEOUT;
        format.Base.dwEffects = effects;
        format.Base.crTextColor = colour;
        let range = CHARRANGE {
            cpMin: offset,
            cpMax: offset + length,
        };
        // SAFETY: body is a live RichEdit and format/range outlive the calls.
        unsafe {
            SendMessageW(
                body,
                EM_EXSETSEL,
                Some(WPARAM(0)),
                Some(LPARAM(&range as *const CHARRANGE as isize)),
            );
            SendMessageW(
                body,
                EM_SETCHARFORMAT,
                Some(WPARAM(SCF_SELECTION as usize)),
                Some(LPARAM(&format as *const CHARFORMAT2W as isize)),
            );
        }
        offset += length;
    }
    let caret = CHARRANGE { cpMin: 0, cpMax: 0 };
    // SAFETY: body is a live RichEdit and caret outlives the call.
    unsafe {
        SendMessageW(
            body,
            EM_EXSETSEL,
            Some(WPARAM(0)),
            Some(LPARAM(&caret as *const CHARRANGE as isize)),
        );
    }
    true
}

/// Which footer the plain message lays out: no footer, Cancel + Replace, or
/// Cancel + Retry.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Footer {
    None,
    Replace,
    Retry,
}

pub fn show_message(text: &str, is_error: bool, show_button: bool, caret: &CaretRect) {
    let footer = if show_button {
        Footer::Replace
    } else {
        Footer::None
    };
    layout_message(text, is_error, footer, caret);
}

/// Red error text with a Cancel + Retry footer (no style menu, no copy button).
pub fn show_error_with_retry(text: &str, caret: &CaretRect) {
    layout_message(text, true, Footer::Retry, caret);
}

fn layout_message(text: &str, is_error: bool, footer: Footer, caret: &CaretRect) {
    let Some(window) = ensure_window() else {
        return;
    };
    let Some(ChildHandles {
        header,
        close,
        body,
        style,
        cancel,
        replace,
        retry,
        copy,
        note,
        hint,
        edit,
        submit,
    }) = handles()
    else {
        return;
    };
    let dpi = dpi_for_anchor(caret);
    refresh_fonts(dpi);
    stop_spinner(window);
    give_back_input_focus(window);
    apply_primary_label(replace);
    show_child(style, false);
    // The input box hid the body; show it again, and hide the input
    // controls (macOS had exactly this empty-body bug in Round 1).
    show_child(body, true);
    show_child(note, false);
    show_child(hint, false);
    show_child(edit, false);
    show_child(submit, false);
    if !set_plain_text(body, text, is_error) {
        return;
    }
    let body_height = measure_body_height(body, dpi);
    let has_footer = footer != Footer::None;
    let show_replace = footer == Footer::Replace;
    let show_retry = footer == Footer::Retry;
    let card_height = HEADER_HEIGHT + body_height + if has_footer { FOOTER_HEIGHT } else { 0 };
    set_child_pos(header, SIDE_PAD, 4, 300, 20, dpi);
    set_child_pos(close, CARD_WIDTH - 36, 2, BUTTON_HEIGHT, BUTTON_HEIGHT, dpi);
    set_child_pos(
        body,
        SIDE_PAD,
        HEADER_HEIGHT,
        CARD_WIDTH - 2 * SIDE_PAD,
        body_height,
        dpi,
    );
    show_child(cancel, has_footer);
    show_child(replace, show_replace);
    show_child(retry, show_retry);
    show_child(
        copy,
        shows_copy_button(show_replace, &current_primary_label()),
    );
    if has_footer {
        let footer_y = HEADER_HEIGHT + body_height;
        set_child_pos(
            cancel,
            CARD_WIDTH - SIDE_PAD - BUTTON_WIDTH - 8 - BUTTON_WIDTH,
            footer_y + 8,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
            dpi,
        );
        set_child_pos(
            replace,
            CARD_WIDTH - SIDE_PAD - BUTTON_WIDTH,
            footer_y + 8,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
            dpi,
        );
        set_child_pos(
            retry,
            CARD_WIDTH - SIDE_PAD - BUTTON_WIDTH,
            footer_y + 8,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
            dpi,
        );
        set_child_pos(
            copy,
            CARD_WIDTH - SIDE_PAD - BUTTON_WIDTH - 8 - BUTTON_WIDTH - 8 - COPY_BUTTON_WIDTH,
            footer_y + 8,
            COPY_BUTTON_WIDTH,
            BUTTON_HEIGHT,
            dpi,
        );
        set_copy_feedback(false);
        STATE.with(|state| {
            if let Some(popup) = state.borrow_mut().as_mut() {
                popup.footer_y = scale(footer_y, dpi);
            }
        });
    } else {
        STATE.with(|state| {
            if let Some(popup) = state.borrow_mut().as_mut() {
                popup.footer_y = -1;
            }
        });
    }
    STATE.with(|state| {
        if let Some(popup) = state.borrow_mut().as_mut() {
            popup.error = is_error;
        }
    });
    show_card(
        window,
        caret,
        scale(CARD_WIDTH, dpi),
        scale(card_height, dpi),
    );
}

pub fn show_tabbed(selected: PopupTab, content: TabContent, caret: &CaretRect) {
    let Some(window) = ensure_window() else {
        return;
    };
    let Some(ChildHandles {
        header,
        close,
        body,
        style,
        cancel,
        replace,
        retry,
        copy,
        note,
        hint,
        edit,
        submit,
    }) = handles()
    else {
        return;
    };
    let dpi = dpi_for_anchor(caret);
    refresh_fonts(dpi);
    stop_spinner(window);
    give_back_input_focus(window);
    apply_primary_label(replace);
    // The input box hid the body; show it again, and hide the input controls.
    show_child(body, true);
    show_child(note, false);
    show_child(hint, false);
    show_child(edit, false);
    show_child(submit, false);
    let caption = wide(&format!("{} \u{25be}", tab_label(selected)));
    // SAFETY: style is a live child window on this thread.
    if let Err(error) = unsafe { SetWindowTextW(style, PCWSTR(caption.as_ptr())) } {
        eprintln!("Writely: could not update the suggestion popup: {error}");
        return;
    }
    show_child(style, true);
    STATE.with(|state| {
        if let Some(popup) = state.borrow_mut().as_mut() {
            popup.selected = selected;
        }
    });
    let (is_error, loading, show_replace, show_retry, indent) = match content {
        TabContent::Diff {
            original,
            suggestion,
        } => {
            if !set_diff_text(body, original, suggestion) {
                return;
            }
            (false, false, true, false, false)
        }
        TabContent::Message {
            text,
            is_error,
            loading,
            retry,
        } => {
            if !set_plain_text(body, text, is_error) {
                return;
            }
            (is_error, loading, false, retry, loading)
        }
    };
    if loading {
        STATE.with(|state| {
            if let Some(popup) = state.borrow_mut().as_mut() {
                popup.spinner_rect = RECT {
                    left: scale(SIDE_PAD, dpi),
                    top: scale(HEADER_HEIGHT + 2, dpi),
                    right: scale(SIDE_PAD + SPINNER_SIZE, dpi),
                    bottom: scale(HEADER_HEIGHT + 2 + SPINNER_SIZE, dpi),
                };
            }
        });
    }
    let body_height = measure_body_height(body, dpi);
    let card_height = HEADER_HEIGHT + body_height + FOOTER_HEIGHT;
    let body_x = if indent { SIDE_PAD + 18 } else { SIDE_PAD };
    let body_width = CARD_WIDTH - SIDE_PAD - body_x;
    set_child_pos(header, SIDE_PAD, 4, 300, 20, dpi);
    set_child_pos(close, CARD_WIDTH - 36, 2, BUTTON_HEIGHT, BUTTON_HEIGHT, dpi);
    set_child_pos(body, body_x, HEADER_HEIGHT, body_width, body_height, dpi);
    show_child(cancel, true);
    show_child(replace, show_replace);
    show_child(retry, show_retry);
    show_child(
        copy,
        shows_copy_button(show_replace, &current_primary_label()),
    );
    let footer_y = HEADER_HEIGHT + body_height;
    set_child_pos(
        style,
        SIDE_PAD,
        footer_y + 8,
        STYLE_BUTTON_WIDTH,
        BUTTON_HEIGHT,
        dpi,
    );
    set_child_pos(
        cancel,
        CARD_WIDTH - SIDE_PAD - BUTTON_WIDTH - 8 - BUTTON_WIDTH,
        footer_y + 8,
        BUTTON_WIDTH,
        BUTTON_HEIGHT,
        dpi,
    );
    set_child_pos(
        replace,
        CARD_WIDTH - SIDE_PAD - BUTTON_WIDTH,
        footer_y + 8,
        BUTTON_WIDTH,
        BUTTON_HEIGHT,
        dpi,
    );
    set_child_pos(
        retry,
        CARD_WIDTH - SIDE_PAD - BUTTON_WIDTH,
        footer_y + 8,
        BUTTON_WIDTH,
        BUTTON_HEIGHT,
        dpi,
    );
    set_child_pos(
        copy,
        CARD_WIDTH - SIDE_PAD - BUTTON_WIDTH - 8 - BUTTON_WIDTH - 8 - COPY_BUTTON_WIDTH,
        footer_y + 8,
        COPY_BUTTON_WIDTH,
        BUTTON_HEIGHT,
        dpi,
    );
    set_copy_feedback(false);
    STATE.with(|state| {
        if let Some(popup) = state.borrow_mut().as_mut() {
            popup.footer_y = scale(footer_y, dpi);
            popup.error = is_error;
        }
    });
    if show_card(
        window,
        caret,
        scale(CARD_WIDTH, dpi),
        scale(card_height, dpi),
    ) && loading
    {
        start_spinner(window);
    }
}

/// Shows the popup with an editable text box: `prefill` as its text and the
/// optional `note` (an error) above it in red, else the gray hint. Compose
/// has no style menu and a Write button; Change keeps the style menu
/// (showing "Describe a change…") and a Rewrite button. Takes keyboard
/// focus with the text selected so the user can type right away.
pub fn show_input(kind: InputKind, prefill: &str, note: Option<&str>, caret: &CaretRect) {
    let Some(window) = ensure_window() else {
        return;
    };
    let Some(ChildHandles {
        header,
        close,
        body,
        style,
        cancel,
        replace,
        retry,
        copy,
        note: note_static,
        hint,
        edit,
        submit,
    }) = handles()
    else {
        return;
    };
    let dpi = dpi_for_anchor(caret);
    refresh_fonts(dpi);
    stop_spinner(window);
    let (hint_text, button_title) = match kind {
        InputKind::Compose => (
            "Describe what you want to write, e.g. a short thank-you note to my team",
            "Write",
        ),
        InputKind::Change => (
            "Describe the change, e.g. mention the Friday deadline and sound confident",
            "Rewrite",
        ),
    };
    // The body stays hidden behind the box; every other layout shows it again.
    show_child(body, false);
    let note_box = match note {
        Some(text) => {
            let value = wide(text);
            // SAFETY: note_static is a live child window on this thread.
            if let Err(error) = unsafe { SetWindowTextW(note_static, PCWSTR(value.as_ptr())) } {
                eprintln!("Writely: could not update the suggestion popup: {error}");
                return;
            }
            show_child(note_static, true);
            show_child(hint, false);
            note_box_height(text)
        }
        None => {
            let value = wide(hint_text);
            // SAFETY: hint is a live child window on this thread.
            if let Err(error) = unsafe { SetWindowTextW(hint, PCWSTR(value.as_ptr())) } {
                eprintln!("Writely: could not update the suggestion popup: {error}");
                return;
            }
            show_child(note_static, false);
            show_child(hint, true);
            note_box_height(hint_text)
        }
    };
    let value = wide(prefill);
    // SAFETY: edit is a live EDIT control on this thread.
    if let Err(error) = unsafe { SetWindowTextW(edit, PCWSTR(value.as_ptr())) } {
        eprintln!("Writely: could not update the suggestion popup: {error}");
        return;
    }
    let caption = wide(button_title);
    // SAFETY: submit is a live child window on this thread.
    if let Err(error) = unsafe { SetWindowTextW(submit, PCWSTR(caption.as_ptr())) } {
        eprintln!("Writely: could not update the suggestion popup: {error}");
        return;
    }
    if kind == InputKind::Change {
        let caption = wide(&format!("{} \u{25be}", tab_label(PopupTab::Custom)));
        // SAFETY: style is a live child window on this thread.
        if let Err(error) = unsafe { SetWindowTextW(style, PCWSTR(caption.as_ptr())) } {
            eprintln!("Writely: could not update the suggestion popup: {error}");
            return;
        }
        show_child(style, true);
        STATE.with(|state| {
            if let Some(popup) = state.borrow_mut().as_mut() {
                popup.selected = PopupTab::Custom;
            }
        });
    } else {
        show_child(style, false);
    }
    // Mirrors the macOS body: 6 px, the note/hint, a 6 px gap under a note,
    // the field, 8 px.
    let gap = if note.is_some() { 6 } else { 0 };
    let body_height = 6 + note_box + gap + INPUT_FIELD_HEIGHT + 8;
    let card_height = HEADER_HEIGHT + body_height + FOOTER_HEIGHT;
    let footer_y = HEADER_HEIGHT + body_height;
    let field_width = CARD_WIDTH - 2 * SIDE_PAD;
    set_child_pos(header, SIDE_PAD, 4, 300, 20, dpi);
    set_child_pos(close, CARD_WIDTH - 36, 2, BUTTON_HEIGHT, BUTTON_HEIGHT, dpi);
    set_child_pos(
        if note.is_some() { note_static } else { hint },
        SIDE_PAD,
        HEADER_HEIGHT + 6,
        field_width,
        note_box,
        dpi,
    );
    set_child_pos(
        edit,
        SIDE_PAD,
        footer_y - 8 - INPUT_FIELD_HEIGHT,
        field_width,
        INPUT_FIELD_HEIGHT,
        dpi,
    );
    show_child(edit, true);
    if kind == InputKind::Change {
        set_child_pos(
            style,
            SIDE_PAD,
            footer_y + 8,
            STYLE_BUTTON_WIDTH,
            BUTTON_HEIGHT,
            dpi,
        );
    }
    set_child_pos(
        cancel,
        CARD_WIDTH - SIDE_PAD - BUTTON_WIDTH - 8 - BUTTON_WIDTH,
        footer_y + 8,
        BUTTON_WIDTH,
        BUTTON_HEIGHT,
        dpi,
    );
    set_child_pos(
        submit,
        CARD_WIDTH - SIDE_PAD - BUTTON_WIDTH,
        footer_y + 8,
        BUTTON_WIDTH,
        BUTTON_HEIGHT,
        dpi,
    );
    show_child(cancel, true);
    show_child(submit, true);
    show_child(replace, false);
    show_child(retry, false);
    show_child(copy, false);
    STATE.with(|state| {
        if let Some(popup) = state.borrow_mut().as_mut() {
            popup.footer_y = scale(footer_y, dpi);
            popup.error = note.is_some();
        }
    });
    show_card(
        window,
        caret,
        scale(CARD_WIDTH, dpi),
        scale(card_height, dpi),
    );
    // The popup never activates on clicks; the input box is the exception:
    // take focus so the user can type right away, after remembering the
    // app to give focus back to.
    remember_input_focus(window);
    // SAFETY: window is a live popup window on this thread.
    if !unsafe { SetForegroundWindow(window) }.as_bool() {
        let error = windows::core::Error::from_thread();
        eprintln!("Writely: could not bring the popup to the front for the text box: {error}");
    }
    // SAFETY: edit is a live EDIT control on this thread.
    if let Err(error) = unsafe { SetFocus(Some(edit)) } {
        eprintln!("Writely: could not focus the popup text box: {error}");
    }
    // SAFETY: edit is a live EDIT control on this thread.
    unsafe {
        SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
    }
}

/// Draft mode for composed text: the style button and menu item 1 read
/// "Draft" and the primary button reads "Insert" (unless it reads "Copy").
/// Off restores "Fix grammar" and the stored primary label.
pub fn set_draft_mode(draft: bool) {
    DRAFT_MODE.with(|mode| {
        *mode.borrow_mut() = draft;
    });
    STATE.with(|state| {
        let popup = state.borrow();
        let Some(popup) = popup.as_ref() else {
            return;
        };
        apply_primary_label(popup.replace);
        let caption = wide(&format!("{} \u{25be}", tab_label(popup.selected)));
        // SAFETY: style is a live child window on this thread.
        if let Err(error) = unsafe { SetWindowTextW(popup.style, PCWSTR(caption.as_ptr())) } {
            eprintln!("Writely: could not update the suggestion popup: {error}");
        }
    });
}

pub fn hide() {
    let window = STATE.with(|state| state.borrow().as_ref().map(|popup| popup.window));
    if let Some(window) = window {
        stop_spinner(window);
        give_back_input_focus(window);
    }
    STATE.with(|state| {
        if let Some(popup) = state.borrow_mut().as_mut() {
            // Every show path sets Retry explicitly; hide it here so it can
            // never linger visible.
            show_child(popup.retry, false);
            show_child(popup.note, false);
            show_child(popup.hint, false);
            show_child(popup.edit, false);
            show_child(popup.submit, false);
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

#[cfg(test)]
mod tests {
    use super::{
        diff_runs, focus_handoff_is_recent, menu_id_for_tab, note_box_height, tab_for_menu_id,
        DiffStyle, PopupTab, RewriteStyle,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn diff_runs_cover_equal_delete_and_insert_with_spacing() {
        let runs = diff_runs("Today are a good day.", "Today is a good day.");
        assert_eq!(
            runs,
            vec![
                ("Today ".to_string(), DiffStyle::Equal),
                ("are".to_string(), DiffStyle::Delete),
                ("is".to_string(), DiffStyle::Insert),
                (" a good day.".to_string(), DiffStyle::Equal),
            ]
        );
        // Dropping the deleted runs rebuilds the suggestion, spacing included.
        let rebuilt: String = runs
            .iter()
            .filter(|(_, style)| *style != DiffStyle::Delete)
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(rebuilt, "Today is a good day.");
    }

    #[test]
    fn menu_ids_round_trip_for_all_tabs() {
        let tabs = [PopupTab::Correct, PopupTab::Custom]
            .into_iter()
            .chain(RewriteStyle::ALL.map(PopupTab::Rewrite));
        for tab in tabs {
            assert_eq!(tab_for_menu_id(menu_id_for_tab(tab)), Some(tab));
        }
        assert_eq!(menu_id_for_tab(PopupTab::Custom), 7);
        assert_eq!(tab_for_menu_id(0), None);
        assert_eq!(tab_for_menu_id(99), None);
    }

    #[test]
    fn note_box_height_wraps_and_caps() {
        assert_eq!(note_box_height("Short."), 15);
        assert_eq!(note_box_height(&"x".repeat(56)), 15);
        assert_eq!(note_box_height(&"x".repeat(57)), 30);
        assert_eq!(note_box_height(&"x".repeat(600)), 90);
    }

    #[test]
    fn focus_handoff_window_is_500_ms() {
        let handoff = Instant::now();
        assert!(focus_handoff_is_recent(handoff, handoff));
        assert!(focus_handoff_is_recent(
            handoff,
            handoff + Duration::from_millis(499)
        ));
        assert!(!focus_handoff_is_recent(
            handoff,
            handoff + Duration::from_millis(500)
        ));
        assert!(!focus_handoff_is_recent(
            handoff,
            handoff + Duration::from_secs(2)
        ));
    }

    #[test]
    fn diff_runs_keep_identical_text_equal() {
        assert_eq!(
            diff_runs("Same text.", "Same text."),
            vec![("Same text.".to_string(), DiffStyle::Equal)]
        );
    }
}
