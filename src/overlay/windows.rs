use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use tao::event_loop::EventLoopProxy;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW,
    MonitorFromPoint, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HGDIOBJ, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WindowsProgramming::MulDiv;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, IsWindow, RegisterClassW, SetWindowPos, ShowWindow,
    UpdateLayeredWindow, HWND_TOPMOST, MA_NOACTIVATE, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE,
    ULW_ALPHA, WM_DESTROY, WM_LBUTTONUP, WM_MOUSEACTIVATE, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::text_access::CaretRect;
use crate::UserEvent;

// Logical (96 DPI) geometry, mirroring `macos.rs:18-20`.
const ICON_SIZE: i32 = 20;
const GLYPH_SIZE: i32 = 14;
const CORNER_RADIUS: f64 = 5.0;

const TRAY_RGBA: &[u8] = include_bytes!("../../icons/tray-32.rgba");
const TRAY_SIZE: u32 = 32;

static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();
static REGISTERED: AtomicBool = AtomicBool::new(false);
static DPI_FALLBACK_LOGGED: AtomicBool = AtomicBool::new(false);

struct IconState {
    window: HWND,
    x: i32,
    y: i32,
    visible: bool,
    // Pixel size of the last rendered icon; 0 means nothing rendered yet.
    pixel_size: u32,
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

// Scales a logical (96 DPI) value to physical pixels at `dpi`.
fn scale(value: i32, dpi: u32) -> i32 {
    // SAFETY: MulDiv is a pure arithmetic function with no pointers.
    unsafe { MulDiv(value, dpi as i32, 96) }
}

// DPI of the monitor at the caret, 96 when it cannot be read (logged once).
fn dpi_for_caret(caret: &CaretRect) -> u32 {
    let point = POINT {
        x: caret.x as i32,
        y: caret.y as i32,
    };
    // SAFETY: point is plain integers for the monitor lookup below.
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
    let mut dpi_x: u32 = 0;
    let mut dpi_y: u32 = 0;
    // SAFETY: dpi_x/dpi_y are writable u32s for the monitor DPI query.
    match unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) } {
        Ok(()) if dpi_x > 0 => dpi_x,
        Ok(()) => {
            if !DPI_FALLBACK_LOGGED.swap(true, Ordering::SeqCst) {
                eprintln!("Writely: could not read the monitor DPI; using 96.");
            }
            96
        }
        Err(error) => {
            if !DPI_FALLBACK_LOGGED.swap(true, Ordering::SeqCst) {
                eprintln!("Writely: could not read the monitor DPI; using 96: {error}");
            }
            96
        }
    }
}

// Coverage (0.0..=1.0) of one pixel by the rounded rect filling the icon.
fn rounded_rect_coverage(x: u32, y: u32, size: u32, radius: f64) -> f64 {
    let half = size as f64 / 2.0;
    let radius = radius.min(half);
    let px = x as f64 + 0.5 - half;
    let py = y as f64 + 0.5 - half;
    let qx = px.abs() - (half - radius);
    let qy = py.abs() - (half - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let distance = outside + qx.max(qy).min(0.0) - radius;
    (0.5 - distance).clamp(0.0, 1.0)
}

// Bilinear sample of the 32x32 straight-RGBA tray image at a glyph pixel.
fn sample_glyph(gx: u32, gy: u32, glyph: u32) -> (f64, f64, f64, f64) {
    if glyph == 0 || TRAY_RGBA.len() != (TRAY_SIZE * TRAY_SIZE * 4) as usize {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let src_x = (gx as f64 + 0.5) * TRAY_SIZE as f64 / glyph as f64 - 0.5;
    let src_y = (gy as f64 + 0.5) * TRAY_SIZE as f64 / glyph as f64 - 0.5;
    let x0 = src_x.floor();
    let y0 = src_y.floor();
    let fx = src_x - x0;
    let fy = src_y - y0;
    let max = TRAY_SIZE as i32 - 1;
    let xi = [x0 as i32, x0 as i32 + 1];
    let yi = [y0 as i32, y0 as i32 + 1];
    let wx = [1.0 - fx, fx];
    let wy = [1.0 - fy, fy];
    let mut channels = [0.0f64; 4];
    for (row, &yy) in yi.iter().enumerate() {
        for (col, &xx) in xi.iter().enumerate() {
            let weight = wx[col] * wy[row];
            if weight == 0.0 {
                continue;
            }
            let cx = xx.clamp(0, max) as usize;
            let cy = yy.clamp(0, max) as usize;
            let base = (cy * TRAY_SIZE as usize + cx) * 4;
            for (channel, value) in channels.iter_mut().enumerate() {
                *value += TRAY_RGBA[base + channel] as f64 * weight;
            }
        }
    }
    (channels[0], channels[1], channels[2], channels[3])
}

/// Renders the icon as `size * size` premultiplied BGRA pixels, top-down:
/// a rounded rect in RGB(38, 38, 38) at alpha 235 with the white "W" glyph
/// scaled to `glyph` x `glyph`, centred, and composited over it.
fn render_icon(size: u32, glyph: u32, radius: f64) -> Vec<u8> {
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    if size == 0 {
        return pixels;
    }
    // The background premultiplied per unit of alpha: 38 * alpha / 255.
    let offset = (size as i32 - glyph as i32) / 2;
    for y in 0..size {
        for x in 0..size {
            let coverage = rounded_rect_coverage(x, y, size, radius);
            let bg_alpha = 235.0 * coverage;
            let bg_prem = 38.0 * bg_alpha / 255.0;
            let (mut src_r, mut src_g, mut src_b, mut src_a) = (0.0, 0.0, 0.0, 0.0);
            let gx = x as i32 - offset;
            let gy = y as i32 - offset;
            if glyph > 0 && gx >= 0 && gy >= 0 && (gx as u32) < glyph && (gy as u32) < glyph {
                let sampled = sample_glyph(gx as u32, gy as u32, glyph);
                src_r = sampled.0;
                src_g = sampled.1;
                src_b = sampled.2;
                src_a = sampled.3;
            }
            let keep = (255.0 - src_a) / 255.0;
            let out_alpha = src_a + bg_alpha * keep;
            let base = ((y * size + x) * 4) as usize;
            // BGRA, premultiplied: each colour channel stays <= its alpha.
            pixels[base] = (src_b * src_a / 255.0 + bg_prem * keep)
                .round()
                .clamp(0.0, 255.0) as u8;
            pixels[base + 1] = (src_g * src_a / 255.0 + bg_prem * keep)
                .round()
                .clamp(0.0, 255.0) as u8;
            pixels[base + 2] = (src_r * src_a / 255.0 + bg_prem * keep)
                .round()
                .clamp(0.0, 255.0) as u8;
            pixels[base + 3] = out_alpha.round().clamp(0.0, 255.0) as u8;
        }
    }
    pixels
}

// Pushes `render_icon` pixels to the layered window at (`x`, `y`). Every
// GDI handle created here is released; a release failure is reported like
// any other failure here so the caller can hide the icon.
fn draw_icon(
    window: HWND,
    x: i32,
    y: i32,
    size: u32,
    glyph: u32,
    radius: f64,
) -> Result<(), String> {
    let pixels = render_icon(size, glyph, radius);
    // SAFETY: no window handle means the screen DC.
    let screen_dc = unsafe { GetDC(None) };
    if screen_dc.is_invalid() {
        return Err(windows::core::Error::from_thread().to_string());
    }
    let result = draw_with_dc(window, x, y, size, &pixels, screen_dc);
    // SAFETY: screen_dc came from GetDC above and is released once here.
    if unsafe { ReleaseDC(None, screen_dc) } == 0 {
        let error = windows::core::Error::from_thread().to_string();
        return match result {
            Ok(()) => Err(error),
            Err(first) => {
                eprintln!("Writely: could not release the icon screen DC: {error}");
                Err(first)
            }
        };
    }
    result
}

fn draw_with_dc(
    window: HWND,
    x: i32,
    y: i32,
    size: u32,
    pixels: &[u8],
    screen_dc: windows::Win32::Graphics::Gdi::HDC,
) -> Result<(), String> {
    // SAFETY: screen_dc is a live screen DC from the caller.
    let memory_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
    if memory_dc.is_invalid() {
        return Err(windows::core::Error::from_thread().to_string());
    }
    let result = draw_with_memory_dc(window, x, y, size, pixels, memory_dc, screen_dc);
    // SAFETY: memory_dc was just created above and is deleted once here.
    if !unsafe { DeleteDC(memory_dc) }.as_bool() {
        let error = windows::core::Error::from_thread().to_string();
        return match result {
            Ok(()) => Err(error),
            Err(first) => {
                eprintln!("Writely: could not release the icon memory DC: {error}");
                Err(first)
            }
        };
    }
    result
}

fn draw_with_memory_dc(
    window: HWND,
    x: i32,
    y: i32,
    size: u32,
    pixels: &[u8],
    memory_dc: windows::Win32::Graphics::Gdi::HDC,
    dest_dc: windows::Win32::Graphics::Gdi::HDC,
) -> Result<(), String> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            // Negative height means a top-down DIB.
            biHeight: -(size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: size * size * 4,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: info describes a 32-bit top-down DIB and bits is writable.
    let bitmap = match unsafe {
        CreateDIBSection(Some(memory_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
    } {
        Ok(bitmap) if !bitmap.is_invalid() && !bits.is_null() => bitmap,
        Ok(_) => return Err(windows::core::Error::from_thread().to_string()),
        Err(error) => return Err(error.to_string()),
    };
    // SAFETY: bits points at size*size*4 writable bytes owned by the DIB.
    unsafe {
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u8>(), pixels.len());
    }
    // SAFETY: memory_dc is live and the bitmap was just created for it.
    let old_bitmap: HGDIOBJ = unsafe { SelectObject(memory_dc, bitmap.into()) };
    if old_bitmap.is_invalid() {
        let error = windows::core::Error::from_thread().to_string();
        // SAFETY: the bitmap was just created and is deleted once here.
        if !unsafe { DeleteObject(bitmap.into()) }.as_bool() {
            eprintln!(
                "Writely: could not release the icon bitmap: {}",
                windows::core::Error::from_thread()
            );
        }
        return Err(error);
    }
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let dest = POINT { x, y };
    let extent = SIZE {
        cx: size as i32,
        cy: size as i32,
    };
    let source = POINT { x: 0, y: 0 };
    // SAFETY: window is a live layered window; dest/extent/source/blend
    // point at live values and memory_dc holds the DIB selected above.
    let update = unsafe {
        UpdateLayeredWindow(
            window,
            Some(dest_dc),
            Some(&dest),
            Some(&extent),
            Some(memory_dc),
            Some(&source),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )
    };
    let mut release_error: Option<String> = None;
    // SAFETY: memory_dc is live and old_bitmap was selected out above.
    if unsafe { SelectObject(memory_dc, old_bitmap) }.is_invalid() {
        release_error = Some(windows::core::Error::from_thread().to_string());
    }
    // SAFETY: the bitmap was just created, is no longer selected, and is
    // deleted once here.
    if !unsafe { DeleteObject(bitmap.into()) }.as_bool() {
        let error = windows::core::Error::from_thread().to_string();
        if release_error.is_none() {
            release_error = Some(error);
        } else {
            eprintln!("Writely: could not release the icon bitmap: {error}");
        }
    }
    match update {
        Err(error) => {
            if let Some(release) = release_error {
                eprintln!("Writely: could not release the icon bitmap: {release}");
            }
            Err(error.to_string())
        }
        Ok(()) => match release_error {
            Some(error) => Err(error),
            None => Ok(()),
        },
    }
}

fn hide_icon(icon: &mut IconState) {
    if icon.visible {
        // SAFETY: icon.window is a live icon window on this thread.
        // ShowWindow reports the previous visibility, not success.
        let _previously_visible = unsafe { ShowWindow(icon.window, SW_HIDE) };
        icon.visible = false;
    }
}

// SAFETY: the window and message parameters are provided by the system for
// the duration of each call; the handler only forwards the click.
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
                    pixel_size: 0,
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

// Unclamped icon origin: caret right + `gap`, vertically centred on the
// caret. `caret` is (x, y, width, height); all values are physical pixels.
fn raw_icon_origin(caret: (f64, f64, f64, f64), size: i32, gap: i32) -> (i32, i32) {
    (
        (caret.0 + caret.2 + gap as f64) as i32,
        (caret.1 + caret.3 / 2.0 - size as f64 / 2.0) as i32,
    )
}

/// Icon origin clamped inside the monitor work area `work`
/// (left, top, right, bottom), all in physical pixels.
fn icon_origin(
    caret: (f64, f64, f64, f64),
    size: i32,
    gap: i32,
    work: (i32, i32, i32, i32),
) -> (i32, i32) {
    let (x, y) = raw_icon_origin(caret, size, gap);
    (
        x.clamp(work.0, (work.2 - size).max(work.0)),
        y.clamp(work.1, (work.3 - size).max(work.1)),
    )
}

pub fn show_at(rect: &CaretRect) {
    let dpi = dpi_for_caret(rect);
    let size = scale(ICON_SIZE, dpi).max(1) as u32;
    let glyph = scale(GLYPH_SIZE, dpi).max(0) as u32;
    let radius = CORNER_RADIUS * dpi as f64 / 96.0;
    // 4 logical px right of the caret; caret coords are physical pixels.
    let gap = scale(4, dpi);
    let caret = (rect.x, rect.y, rect.width, rect.height);
    let (x, y) = {
        let point = POINT {
            x: (rect.x + rect.width / 2.0) as i32,
            y: (rect.y + rect.height / 2.0) as i32,
        };
        // SAFETY: point is plain integers for the monitor lookup below.
        let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: info is writable with cbSize set for the monitor query.
        if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            icon_origin(
                caret,
                size as i32,
                gap,
                (
                    info.rcWork.left,
                    info.rcWork.top,
                    info.rcWork.right,
                    info.rcWork.bottom,
                ),
            )
        } else {
            let error = windows::core::Error::from_thread();
            eprintln!("Writely: could not read the monitor work area: {error}");
            raw_icon_origin(caret, size as i32, gap)
        }
    };
    let Some(window) = ensure_window() else {
        return;
    };
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(icon) = state.as_mut() else {
            return;
        };
        if icon.visible && icon.x == x && icon.y == y && icon.pixel_size == size {
            return;
        }
        if icon.pixel_size != size {
            if let Err(error) = draw_icon(window, x, y, size, glyph, radius) {
                eprintln!("Writely: could not draw the icon: {error}");
                hide_icon(icon);
                return;
            }
            icon.pixel_size = size;
        } else if icon.x != x || icon.y != y || !icon.visible {
            // SAFETY: window is a live icon window on this thread.
            if let Err(error) = unsafe {
                SetWindowPos(
                    window,
                    Some(HWND_TOPMOST),
                    x,
                    y,
                    size as i32,
                    size as i32,
                    SWP_NOACTIVATE,
                )
            } {
                eprintln!("Writely: could not draw the icon: {error}");
                hide_icon(icon);
                return;
            }
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
            hide_icon(icon);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{icon_origin, render_icon};

    #[test]
    fn render_icon_length_and_corners() {
        let pixels = render_icon(20, 14, 5.0);
        assert_eq!(pixels.len(), 1600);
        // Corner (0, 0) is outside the rounded rect.
        assert!(pixels[3] < 64, "corner alpha was {}", pixels[3]);
        // Edge-centre (0, 10) is the opaque background.
        let edge = ((10 * 20) * 4) as usize;
        assert_eq!(pixels[edge + 3], 235);
        assert!(pixels[edge] <= pixels[edge + 3]);
        assert!(pixels[edge + 1] <= pixels[edge + 3]);
        assert!(pixels[edge + 2] <= pixels[edge + 3]);
    }

    #[test]
    fn render_icon_centre_has_white_glyph() {
        let pixels = render_icon(20, 14, 5.0);
        let mut near_white = 0;
        for y in 3..17usize {
            for x in 3..17usize {
                let base = (y * 20 + x) * 4;
                if pixels[base] > 200 && pixels[base + 1] > 200 && pixels[base + 2] > 200 {
                    near_white += 1;
                }
            }
        }
        assert!(near_white > 0, "no near-white glyph pixels in the centre");
    }

    #[test]
    fn icon_origin_normal_case() {
        // Right of the caret, vertically centred, inside the work area.
        assert_eq!(
            icon_origin((100.0, 100.0, 2.0, 20.0), 20, 4, (0, 0, 1920, 1080)),
            (106, 100)
        );
    }

    #[test]
    fn icon_origin_clamped_at_right_edge() {
        assert_eq!(
            icon_origin((1900.0, 100.0, 2.0, 20.0), 20, 4, (0, 0, 1920, 1080)),
            (1900, 100)
        );
    }

    #[test]
    fn icon_origin_clamped_at_bottom() {
        assert_eq!(
            icon_origin((100.0, 1070.0, 2.0, 20.0), 20, 4, (0, 0, 1920, 1080)),
            (106, 1060)
        );
    }

    #[test]
    fn icon_origin_second_monitor_negative_x() {
        assert_eq!(
            icon_origin((-500.0, 100.0, 2.0, 20.0), 20, 4, (-1920, 0, 0, 1080)),
            (-494, 100)
        );
    }

    #[test]
    fn render_icon_channels_never_exceed_alpha() {
        let pixels = render_icon(20, 14, 5.0);
        for base in (0..pixels.len()).step_by(4) {
            assert!(pixels[base] <= pixels[base + 3]);
            assert!(pixels[base + 1] <= pixels[base + 3]);
            assert!(pixels[base + 2] <= pixels[base + 3]);
        }
    }
}
