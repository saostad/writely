//! Copy-and-paste fallback for Windows apps whose text UI Automation
//! doesn't expose. It never runs for a secure (password) field — the caller
//! checks that first.
//!
//! Copy snapshots the clipboard, sends Ctrl+C, waits 50 ms then polls the
//! clipboard sequence number every 5 ms for up to 2 s, reads
//! `CF_UNICODETEXT`, and restores the snapshot only when nothing else
//! changed the clipboard. Paste writes the suggestion (marked so clipboard
//! history skips it), sends Ctrl+V, waits 300 ms, then restores the same
//! way. The user's text is never written to logs.

use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_C, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL,
    VK_RMENU, VK_RSHIFT, VK_RWIN, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use super::CopyResult;

const CF_UNICODETEXT_FORMAT: u32 = 13;
const CF_BITMAP_FORMAT: u32 = 2;
const CF_METAFILEPICT_FORMAT: u32 = 3;
const CF_PALETTE_FORMAT: u32 = 9;
const CF_ENHMETAFILE_FORMAT: u32 = 14;
const CF_OWNERDISPLAY_FORMAT: u32 = 0x0080;
const CF_DSPTEXT_FORMAT: u32 = 0x0081;
const CF_DSPBITMAP_FORMAT: u32 = 0x0082;
const CF_DSPMETAFILEPICT_FORMAT: u32 = 0x0083;
const CF_DSPENHMETAFILE_FORMAT: u32 = 0x008E;

/// How long to wait for the clipboard to change after copying: 50 ms, then
/// poll the sequence number every 5 ms for up to 2 s (same as macOS).
const COPY_INITIAL_WAIT_MS: u64 = 50;
const COPY_POLL_MS: u64 = 5;
const COPY_POLLS: u32 = 400;

/// How long to wait after sending Ctrl+V before restoring the clipboard.
const PASTE_WAIT_MS: u64 = 300;

/// Whether `format` holds a GDI handle (or is owner-drawn) rather than an
/// `HGLOBAL`, so it cannot be snapshotted byte-for-byte. Pure for testing.
pub fn is_skipped_format(format: u32) -> bool {
    matches!(
        format,
        CF_BITMAP_FORMAT
            | CF_METAFILEPICT_FORMAT
            | CF_PALETTE_FORMAT
            | CF_ENHMETAFILE_FORMAT
            | CF_OWNERDISPLAY_FORMAT
            | CF_DSPTEXT_FORMAT
            | CF_DSPBITMAP_FORMAT
            | CF_DSPMETAFILEPICT_FORMAT
            | CF_DSPENHMETAFILE_FORMAT
    )
}

/// Whether the clipboard may be restored: only when its sequence number is
/// still the one our copy/paste produced. Pure for testing.
pub fn should_restore(current_sequence: u32, our_sequence: u32) -> bool {
    current_sequence == our_sequence
}

/// Closes the clipboard when dropped, logging a failure.
struct ClipboardGuard;

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: this guard is created only after a successful OpenClipboard
        // and is dropped exactly once, so the clipboard is open here.
        if let Err(error) = unsafe { CloseClipboard() } {
            eprintln!("Writely: could not close the clipboard: {error}");
        }
    }
}

/// Opens the clipboard, retrying every 10 ms for up to 500 ms because
/// another app may hold it.
fn open_clipboard() -> Result<ClipboardGuard, String> {
    let mut last_error = String::new();
    for attempt in 0..=50 {
        // SAFETY: opening the clipboard with no owner window is a plain call.
        match unsafe { OpenClipboard(None) } {
            Ok(()) => return Ok(ClipboardGuard),
            Err(error) => {
                last_error = error.to_string();
                if attempt < 50 {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }
    Err(format!("Could not open the clipboard: {last_error}"))
}

fn foreground_pid() -> Option<u32> {
    // SAFETY: querying the foreground window takes no borrowed state.
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.is_invalid() {
        return None;
    }
    let mut pid: u32 = 0;
    // SAFETY: foreground is a live window and pid is writable.
    let _thread = unsafe { GetWindowThreadProcessId(foreground, Some(&mut pid)) };
    if pid == 0 {
        None
    } else {
        Some(pid)
    }
}

fn check_foreground(pid: u32, action: &str) -> Result<(), String> {
    if foreground_pid() == Some(pid) {
        Ok(())
    } else if action == "paste" {
        Err("Writely couldn't paste the suggestion: the app you were typing in is no longer in front.".to_string())
    } else {
        Err(
            "Writely couldn't copy the text: the app you were typing in is no longer in front."
                .to_string(),
        )
    }
}

fn key_input(key: VIRTUAL_KEY, key_up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: 0,
                dwFlags: if key_up {
                    KEYEVENTF_KEYUP
                } else {
                    Default::default()
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_inputs(inputs: &[INPUT]) -> Result<(), String> {
    // SAFETY: inputs is a valid slice of INPUT records for the call.
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        // SAFETY: reading the calling thread's last error takes no state.
        let error = windows::core::Error::from_thread();
        Err(format!(
            "{error} (SendInput sent {sent} of {})",
            inputs.len()
        ))
    }
}

/// Releases Ctrl, Alt, Shift and both Win keys, but only those actually
/// down: a lone injected key-up for a Win/Alt key that was never pressed
/// can open the Start menu or focus the target app's menu bar. Releasing
/// Alt or Win is preceded by a Ctrl tap so it isn't read as a lone tap.
fn release_modifiers() {
    for key in [
        VK_LCONTROL,
        VK_RCONTROL,
        VK_LMENU,
        VK_RMENU,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_LWIN,
        VK_RWIN,
    ] {
        // SAFETY: querying the async key state takes a plain virtual-key code.
        let is_down = unsafe { GetAsyncKeyState(key.0 as i32) } < 0;
        if !is_down {
            continue;
        }
        if key == VK_LMENU || key == VK_RMENU || key == VK_LWIN || key == VK_RWIN {
            if let Err(error) =
                send_inputs(&[key_input(VK_CONTROL, false), key_input(VK_CONTROL, true)])
            {
                eprintln!("Writely: could not release a modifier key: {error}");
            }
        }
        if let Err(error) = send_inputs(&[key_input(key, true)]) {
            eprintln!("Writely: could not release a modifier key: {error}");
        }
    }
}

fn send_ctrl_key(key: VIRTUAL_KEY) -> Result<(), String> {
    send_inputs(&[
        key_input(VK_CONTROL, false),
        key_input(key, false),
        key_input(key, true),
        key_input(VK_CONTROL, true),
    ])
}

/// One saved clipboard format: its id and the bytes of its `HGLOBAL`.
type SavedFormat = (u32, Vec<u8>);

/// Snapshots every `HGLOBAL` format on the open clipboard. GDI-handle
/// formats are skipped and logged once per snapshot.
fn snapshot_clipboard() -> Vec<SavedFormat> {
    let mut saved = Vec::new();
    let mut skipped: Vec<u32> = Vec::new();
    let mut format: u32 = 0;
    loop {
        // SAFETY: the clipboard is open (the caller holds the guard).
        format = unsafe { EnumClipboardFormats(format) };
        if format == 0 {
            break;
        }
        if is_skipped_format(format) {
            skipped.push(format);
            continue;
        }
        // SAFETY: the clipboard is open and format came from enumeration.
        let handle = match unsafe { GetClipboardData(format) } {
            Ok(handle) => handle,
            Err(error) => {
                eprintln!("Writely: could not read clipboard format {format}: {error}");
                skipped.push(format);
                continue;
            }
        };
        if handle.is_invalid() {
            skipped.push(format);
            continue;
        }
        let hglobal = HGLOBAL(handle.0);
        // SAFETY: hglobal is the clipboard's handle for this format while
        // the clipboard stays open and unmodified by us.
        let size = unsafe { GlobalSize(hglobal) };
        // SAFETY: hglobal is valid as above; the lock is released below.
        let ptr = unsafe { GlobalLock(hglobal) };
        if ptr.is_null() {
            skipped.push(format);
            continue;
        }
        // SAFETY: ptr points at size readable bytes from the lock above.
        let bytes: Vec<u8> = unsafe { std::slice::from_raw_parts(ptr as *const u8, size) }.to_vec();
        // SAFETY: hglobal was locked above and is unlocked exactly once.
        let _unlocked = unsafe { GlobalUnlock(hglobal) };
        saved.push((format, bytes));
    }
    if !skipped.is_empty() {
        let ids: Vec<String> = skipped.iter().map(u32::to_string).collect();
        eprintln!(
            "Writely: some clipboard formats could not be saved and won't be restored: {}",
            ids.join(", ")
        );
    }
    saved
}

/// Takes a snapshot with the clipboard opened and closed around it.
fn take_snapshot() -> Result<Vec<SavedFormat>, String> {
    let _guard = open_clipboard()?;
    Ok(snapshot_clipboard())
}

/// Allocates a moveable `HGLOBAL` holding `bytes`.
fn alloc_global(bytes: &[u8]) -> Result<HGLOBAL, String> {
    // SAFETY: GMEM_MOVEABLE with this size is a plain allocation request.
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) }
        .map_err(|error| error.to_string())?;
    if bytes.is_empty() {
        return Ok(handle);
    }
    // SAFETY: handle is a fresh allocation of at least bytes.len() bytes.
    let ptr = unsafe { GlobalLock(handle) };
    if ptr.is_null() {
        // SAFETY: handle was just allocated and ownership is still ours.
        let _freed = unsafe { GlobalFree(Some(handle)) };
        // SAFETY: reading the calling thread's last error takes no state.
        return Err(windows::core::Error::from_thread().to_string());
    }
    // SAFETY: ptr points at bytes.len() writable bytes from the lock above,
    // and the source slice does not overlap it.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len()) };
    // SAFETY: handle was locked above and is unlocked exactly once.
    let _unlocked = unsafe { GlobalUnlock(handle) };
    Ok(handle)
}

/// Sets one format on the open clipboard; the clipboard takes ownership of
/// the handle on success.
fn set_format(format: u32, bytes: &[u8]) -> Result<(), String> {
    let handle = alloc_global(bytes)?;
    // SAFETY: the clipboard is open (the caller holds the guard) and handle
    // is a moveable HGLOBAL whose ownership passes to the clipboard.
    match unsafe { SetClipboardData(format, Some(HANDLE(handle.0))) } {
        Ok(_) => Ok(()),
        Err(error) => {
            // SAFETY: SetClipboardData failed, so the handle is still ours.
            let _freed = unsafe { GlobalFree(Some(handle)) };
            Err(error.to_string())
        }
    }
}

/// Builds the error returned when writing the suggestion failed: the
/// original error, with the restore failure appended when restoring the
/// saved clipboard also failed. Pure for testing.
pub fn paste_write_error(original: &str, restore_error: Option<&str>) -> String {
    match restore_error {
        Some(error) => {
            format!("{original} Writely also couldn't put your clipboard back: {error}")
        }
        None => original.to_string(),
    }
}

/// Restores a snapshot onto the clipboard (opened and closed here).
fn restore_clipboard(saved: &[SavedFormat]) -> Result<(), String> {
    let _guard = open_clipboard()?;
    // SAFETY: the clipboard is open (this guard holds it).
    unsafe { EmptyClipboard() }
        .map_err(|error| format!("Could not restore the clipboard: {error}"))?;
    for (format, bytes) in saved {
        set_format(*format, bytes)
            .map_err(|error| format!("Could not restore the clipboard: {error}"))?;
    }
    Ok(())
}

/// Restores only when nothing else changed the clipboard since our
/// copy/paste; otherwise logs and leaves the new content alone.
fn restore_if_unchanged(saved: &[SavedFormat], our_sequence: u32) {
    // SAFETY: reading the clipboard sequence number takes no state.
    let current = unsafe { GetClipboardSequenceNumber() };
    if should_restore(current, our_sequence) {
        if let Err(error) = restore_clipboard(saved) {
            eprintln!("Writely: {error}");
        }
    } else {
        eprintln!("Writely: clipboard changed by another app; not restored.");
    }
}

/// Reads `CF_UNICODETEXT` from the open clipboard.
fn read_unicode_text() -> Option<String> {
    // SAFETY: the clipboard is open (the caller holds the guard).
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT_FORMAT) }.ok()?;
    if handle.is_invalid() {
        return None;
    }
    let hglobal = HGLOBAL(handle.0);
    // SAFETY: hglobal is the clipboard's text handle while it stays open.
    let size = unsafe { GlobalSize(hglobal) };
    // SAFETY: hglobal is valid as above; the lock is released below.
    let ptr = unsafe { GlobalLock(hglobal) };
    if ptr.is_null() {
        return None;
    }
    let units_count = size / std::mem::size_of::<u16>();
    // SAFETY: ptr points at units_count readable u16 values from the lock.
    let units: Vec<u16> =
        unsafe { std::slice::from_raw_parts(ptr as *const u16, units_count) }.to_vec();
    // SAFETY: hglobal was locked above and is unlocked exactly once.
    let _unlocked = unsafe { GlobalUnlock(hglobal) };
    let text = String::from_utf16_lossy(&units);
    Some(text.trim_end_matches('\0').to_string())
}

fn registered_format(name: &str) -> Result<u32, String> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: wide is a NUL-terminated UTF-16 string for the call.
    let format = unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) };
    if format == 0 {
        // SAFETY: reading the calling thread's last error takes no state.
        Err(windows::core::Error::from_thread().to_string())
    } else {
        Ok(format)
    }
}

/// Copies the selected text in the app `pid` via Ctrl+C, restoring the
/// user's clipboard afterwards unless another app changed it meanwhile.
pub fn layered_copy_text(pid: u32) -> Result<CopyResult, String> {
    check_foreground(pid, "copy")?;
    let saved = take_snapshot()?;
    // SAFETY: reading the clipboard sequence number takes no state.
    let before = unsafe { GetClipboardSequenceNumber() };
    release_modifiers();
    if let Err(error) = send_ctrl_key(VK_C) {
        restore_if_unchanged(&saved, before);
        return Err(format!("Could not copy the selected text: {error}"));
    }
    std::thread::sleep(Duration::from_millis(COPY_INITIAL_WAIT_MS));
    let mut changed = false;
    for _ in 0..COPY_POLLS {
        // SAFETY: reading the clipboard sequence number takes no state.
        if unsafe { GetClipboardSequenceNumber() } != before {
            changed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(COPY_POLL_MS));
    }
    if !changed {
        // SAFETY: reading the clipboard sequence number takes no state.
        changed = unsafe { GetClipboardSequenceNumber() } != before;
    }
    if !changed {
        return Ok(CopyResult::NothingNew);
    }
    let (text, our_sequence) = {
        let _guard = open_clipboard()?;
        let text = read_unicode_text();
        // SAFETY: reading the clipboard sequence number takes no state.
        let sequence = unsafe { GetClipboardSequenceNumber() };
        (text, sequence)
    };
    restore_if_unchanged(&saved, our_sequence);
    match text {
        Some(copied) if !copied.is_empty() => Ok(CopyResult::Text(copied)),
        _ => Ok(CopyResult::NothingNew),
    }
}

/// Writes `text` to the clipboard as `CF_UNICODETEXT` and keeps it
/// there (no snapshot, no restore, no history-exclusion formats), for
/// terminal apps where the suggestion can only be copied, not pasted
/// over a selection. The caller adds its own message prefix to errors.
pub fn copy_suggestion(text: &str) -> Result<(), String> {
    let _guard = open_clipboard()?;
    // SAFETY: the clipboard is open (this guard holds it).
    unsafe { EmptyClipboard() }.map_err(|error| error.to_string())?;
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let mut bytes = Vec::with_capacity(units.len() * 2);
    for unit in &units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    set_format(CF_UNICODETEXT_FORMAT, &bytes)
}

/// Pastes `suggestion` into the app `pid` via Ctrl+V, then restores the
/// saved clipboard only when nothing else changed it. The suggestion is
/// marked so Windows clipboard history and cloud sync skip it.
pub fn paste_suggestion(pid: u32, suggestion: &str) -> Result<(), String> {
    check_foreground(pid, "paste")?;
    let saved = take_snapshot()?;
    let write_result: Result<u32, String> = {
        let _guard = open_clipboard()?;
        // SAFETY: the clipboard is open (this guard holds it).
        unsafe { EmptyClipboard() }
            .map_err(|error| format!("Could not write the suggestion to the clipboard: {error}"))?;
        let result = (|| -> Result<u32, String> {
            let units: Vec<u16> = suggestion
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut bytes = Vec::with_capacity(units.len() * 2);
            for unit in &units {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            set_format(CF_UNICODETEXT_FORMAT, &bytes).map_err(|error| {
                format!("Could not write the suggestion to the clipboard: {error}")
            })?;
            // History/cloud exclusion markers are best-effort: a failure
            // must not lose the paste (or the saved clipboard).
            match registered_format("ExcludeClipboardContentFromMonitorProcessing") {
                Ok(exclude) => {
                    if let Err(error) = set_format(exclude, &[]) {
                        eprintln!(
                            "Writely: could not hide the suggestion from clipboard history: {error}"
                        );
                    }
                }
                Err(error) => {
                    eprintln!(
                        "Writely: could not hide the suggestion from clipboard history: {error}"
                    );
                }
            }
            for name in ["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"] {
                match registered_format(name) {
                    Ok(format_id) => {
                        if let Err(error) = set_format(format_id, &0u32.to_le_bytes()) {
                            eprintln!("Writely: could not hide the suggestion from clipboard history: {error}");
                        }
                    }
                    Err(error) => {
                        eprintln!(
                            "Writely: could not hide the suggestion from clipboard history: {error}"
                        );
                    }
                }
            }
            // SAFETY: reading the clipboard sequence number takes no state.
            Ok(unsafe { GetClipboardSequenceNumber() })
        })();
        result
        // The guard drops here, closing the clipboard before any restore.
    };
    let our_sequence = match write_result {
        Ok(sequence) => sequence,
        Err(original) => {
            return Err(match restore_clipboard(&saved) {
                Ok(()) => paste_write_error(&original, None),
                Err(restore_error) => paste_write_error(&original, Some(&restore_error)),
            });
        }
    };
    release_modifiers();
    if let Err(error) = send_ctrl_key(VK_V) {
        restore_if_unchanged(&saved, our_sequence);
        return Err(format!("Could not paste the suggestion: {error}"));
    }
    std::thread::sleep(Duration::from_millis(PASTE_WAIT_MS));
    restore_if_unchanged(&saved, our_sequence);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{is_skipped_format, paste_write_error, should_restore};

    #[test]
    fn gdi_handle_formats_are_skipped() {
        // CF_BITMAP, CF_METAFILEPICT, CF_PALETTE, CF_ENHMETAFILE,
        // CF_OWNERDISPLAY and the CF_DSP* display formats.
        for format in [2, 3, 9, 14, 0x0080, 0x0081, 0x0082, 0x0083, 0x008E] {
            assert!(is_skipped_format(format), "format {format} must be skipped");
        }
        // CF_TEXT, CF_UNICODETEXT, CF_HDROP and a registered format are not.
        for format in [1, 13, 15, 0xC000] {
            assert!(!is_skipped_format(format), "format {format} must be saved");
        }
    }

    #[test]
    fn paste_write_error_keeps_original_when_restore_succeeds() {
        assert_eq!(
            paste_write_error(
                "Could not write the suggestion to the clipboard: boom",
                None
            ),
            "Could not write the suggestion to the clipboard: boom"
        );
    }

    #[test]
    fn paste_write_error_appends_restore_failure() {
        assert_eq!(
            paste_write_error(
                "Could not write the suggestion to the clipboard: boom",
                Some("Could not restore the clipboard: nope")
            ),
            "Could not write the suggestion to the clipboard: boom Writely also couldn't put your clipboard back: Could not restore the clipboard: nope"
        );
    }

    #[test]
    fn restore_only_when_sequence_is_unchanged() {
        assert!(should_restore(7, 7));
        assert!(!should_restore(8, 7));
        assert!(!should_restore(6, 7));
    }
}
