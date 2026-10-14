use std::ops::Range;
use std::sync::{Mutex, OnceLock};

use atspi::proxy::accessible::{AccessibleProxy, ObjectRefExt};
use atspi::proxy::proxy_ext::ProxyExt;
use atspi::{AccessibilityConnection, CoordType, ObjectRefOwned, Role};

use super::{replaced_text, CaretInfo, CaretRect, FieldSnapshot};

/// The focused accessible from the watcher's focus/caret events.
/// `focused_field` reads this instead of walking the registry tree, so a
/// burst of keystrokes never triggers a full tree scan.
static FOCUSED: Mutex<Option<ObjectRefOwned>> = Mutex::new(None);

pub(crate) fn set_focused(object: Option<ObjectRefOwned>) {
    match FOCUSED.lock() {
        Ok(mut guard) => *guard = object,
        Err(poisoned) => *poisoned.into_inner() = object,
    }
}

/// Clears the cached focused accessible only when it is the item that lost
/// focus, so a stale "lost focus" arriving after the new field's "got focus"
/// cannot wipe the cache.
pub(crate) fn clear_focused_if(item: &ObjectRefOwned) {
    let is_current = match FOCUSED.lock() {
        Ok(guard) => guard.as_ref() == Some(item),
        Err(poisoned) => poisoned.into_inner().as_ref() == Some(item),
    };
    if is_current {
        set_focused(None);
    }
}

fn take_focused() -> Option<ObjectRefOwned> {
    match FOCUSED.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// The Linux backend, chosen once per process: the input method bridge
/// on Wayland, AT-SPI otherwise.
static IME_BRIDGE: OnceLock<bool> = OnceLock::new();

pub(crate) fn uses_ime_bridge() -> bool {
    *IME_BRIDGE.get_or_init(|| std::env::var_os("WAYLAND_DISPLAY").is_some())
}

pub(crate) enum PlatformHandle {
    Atspi {
        object: ObjectRefOwned,
    },
    Bridge {
        sender: String,
        context: String,
        /// True when the snapshot holds selected text because the app
        /// doesn't share its text (select fallback).
        selection: bool,
    },
}

fn read_error(error: impl std::fmt::Display) -> String {
    format!("Could not read the focused text field: {error}")
}

pub(crate) fn check_permission(_prompt: bool) -> Result<(), String> {
    Ok(())
}

async fn pid_for(conn: &AccessibilityConnection, element: &AccessibleProxy<'_>) -> u32 {
    let app = match element.get_application().await {
        Ok(app) => app,
        Err(error) => {
            eprintln!("Writely: could not read the application on Linux: {error}");
            return 0;
        }
    };
    let Some(bus_name) = app.name_as_str() else {
        return 0;
    };
    let dbus = match atspi::zbus::fdo::DBusProxy::new(conn.connection()).await {
        Ok(dbus) => dbus,
        Err(error) => {
            eprintln!("Writely: could not read the process id on Linux: {error}");
            return 0;
        }
    };
    let bus = match atspi::zbus::names::BusName::try_from(bus_name) {
        Ok(bus) => bus,
        Err(error) => {
            eprintln!("Writely: could not read the process id on Linux: {error}");
            return 0;
        }
    };
    match dbus.get_connection_unix_process_id(bus).await {
        Ok(pid) => pid,
        Err(error) => {
            eprintln!("Writely: could not read the process id on Linux: {error}");
            0
        }
    }
}

async fn focused_field_async() -> Result<Option<FieldSnapshot>, String> {
    let Some(object) = take_focused() else {
        return Ok(None);
    };
    let conn = AccessibilityConnection::new().await.map_err(read_error)?;
    let element = object
        .as_accessible_proxy(conn.connection())
        .await
        .map_err(read_error)?;
    let role = element.get_role().await.map_err(read_error)?;
    if role == Role::PasswordText {
        return Ok(None);
    }
    let proxies = match element.proxies().await {
        Ok(proxies) => proxies,
        Err(_) => return Ok(None),
    };
    let text_proxy = match proxies.text().await {
        Ok(text_proxy) => text_proxy,
        Err(_) => return Ok(None),
    };
    let count = text_proxy.character_count().await.map_err(read_error)?;
    let text = if count > 0 {
        text_proxy.get_text(0, count).await.map_err(read_error)?
    } else {
        String::new()
    };
    let caret_offset = text_proxy.caret_offset().await.map_err(read_error)?;
    let char_count = text.chars().count();
    let caret = (caret_offset.max(0) as usize).min(char_count);

    // The first selection, when there is one. A failure to read it is an
    // error for the popup, not a silent "no selection".
    let selection = {
        let n = text_proxy.get_n_selections().await.map_err(read_error)?;
        if n > 0 {
            let (raw_start, raw_end) = text_proxy.get_selection(0).await.map_err(read_error)?;
            let start = (raw_start.max(0) as usize).min(char_count);
            let end = (raw_end.max(0) as usize).min(char_count);
            let (start, end) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            if end > start {
                Some(start..end)
            } else {
                None
            }
        } else {
            None
        }
    };

    // Chromium answers an all-zero box (not an error) for the offset past the
    // last character, i.e. while typing at the end of a field; treat that like
    // an error and measure from the previous character instead.
    let (x, y, width, height) = match text_proxy
        .get_character_extents(caret_offset, CoordType::Screen)
        .await
    {
        Ok(extents) if extents != (0, 0, 0, 0) => extents,
        _ if caret_offset > 0 => {
            let (px, py, pwidth, pheight) = text_proxy
                .get_character_extents(caret_offset - 1, CoordType::Screen)
                .await
                .map_err(read_error)?;
            (px + pwidth, py, 0, pheight)
        }
        Ok(extents) => extents,
        Err(error) => return Err(read_error(error)),
    };
    // Some toolkits (Qt Quick) never report character boxes; anchor to the
    // field's top-right corner instead. Native Wayland apps report no screen
    // position at all, so this stays all-zero and the icon is not shown.
    let (x, y, width, height) = if (x, y, width, height) == (0, 0, 0, 0) {
        match proxies.component().await {
            Ok(component) => match component.get_extents(CoordType::Screen).await {
                Ok((fx, fy, fwidth, _)) if fwidth > 0 => (fx + fwidth - 28, fy + 2, 0, 20),
                _ => (0, 0, 0, 0),
            },
            Err(_) => (0, 0, 0, 0),
        }
    } else {
        (x, y, width, height)
    };
    let pid = pid_for(&conn, &element).await;
    if pid != 0 && pid == std::process::id() {
        return Ok(None);
    }
    let field_len_utf16 = super::utf16_len(&text);
    Ok(Some(FieldSnapshot {
        pid,
        text,
        caret,
        caret_known: true,
        caret_rect: CaretRect {
            x: x as f64,
            y: y as f64,
            width: width as f64,
            height: height as f64,
            physical: false,
        },
        selection,
        // Bounded reads are a later round on Linux; the snapshot covers the
        // whole field, so the whole-field write fallback stays allowed.
        window_start_utf16: 0,
        field_len_utf16,
        handle: PlatformHandle::Atspi { object },
    }))
}

async fn read_text_async(handle: &PlatformHandle) -> Result<String, String> {
    let PlatformHandle::Atspi { object } = handle else {
        unreachable!("bridge handles never reach the AT-SPI path");
    };
    let conn = AccessibilityConnection::new().await.map_err(read_error)?;
    let element = object
        .as_accessible_proxy(conn.connection())
        .await
        .map_err(read_error)?;
    let proxies = element.proxies().await.map_err(read_error)?;
    let text_proxy = proxies.text().await.map_err(read_error)?;
    let count = text_proxy.character_count().await.map_err(read_error)?;
    if count <= 0 {
        return Ok(String::new());
    }
    text_proxy.get_text(0, count).await.map_err(read_error)
}

async fn replace_range_async(
    snap: &FieldSnapshot,
    range: Range<usize>,
    new_text: &str,
) -> Result<(), String> {
    let PlatformHandle::Atspi { object } = &snap.handle else {
        unreachable!("bridge handles never reach the AT-SPI path");
    };
    let conn = AccessibilityConnection::new().await.map_err(read_error)?;
    let element = object
        .as_accessible_proxy(conn.connection())
        .await
        .map_err(read_error)?;
    let proxies = element.proxies().await.map_err(read_error)?;
    let editable = match proxies.editable_text().await {
        Ok(editable) => editable,
        Err(_) => {
            return Err("This app's text field doesn't let Writely replace text.".to_string());
        }
    };
    let start = range.start as i32;
    let end = range.end as i32;
    if end > start {
        let deleted = editable
            .delete_text(start, end)
            .await
            .map_err(|error| format!("Could not replace the text: {error}"))?;
        if !deleted {
            return Err("The app didn't accept the replacement text.".to_string());
        }
    }
    if !new_text.is_empty() {
        let inserted = editable
            .insert_text(start, new_text, new_text.chars().count() as i32)
            .await
            .map_err(|error| format!("Could not replace the text: {error}"))?;
        if !inserted {
            return Err("The app didn't accept the replacement text.".to_string());
        }
    }
    let expected = replaced_text(&snap.text, &range, new_text);
    let actual = read_text_async(&snap.handle).await?;
    if actual != expected {
        return Err("The app didn't accept the replacement text.".to_string());
    }
    Ok(())
}

pub(crate) fn caret_info() -> Result<Option<CaretInfo>, String> {
    if uses_ime_bridge() {
        return caret_info_bridge();
    }
    match focused_field()? {
        Some(snap) => Ok(Some(CaretInfo {
            pid: snap.pid,
            caret_rect: snap.caret_rect,
        })),
        None => Ok(None),
    }
}

/// Bridge-mode caret: the W shows whenever a context offers text or the
/// select fallback. Never selects anything: selection happens only on
/// click, in `focused_field_bridge`.
fn caret_info_bridge() -> Result<Option<CaretInfo>, String> {
    match crate::ime_bridge::focused_snapshot() {
        Some(_) => Ok(Some(CaretInfo {
            pid: 0,
            // The bridge panel is placed by the input method, not by Writely.
            caret_rect: CaretRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
                physical: false,
            },
        })),
        None => Ok(None),
    }
}

fn focused_field_bridge() -> Result<Option<FieldSnapshot>, String> {
    let Some((sender, context, kind)) = crate::ime_bridge::focused_snapshot() else {
        return Ok(None);
    };
    // The bridge panel is placed by the input method, not by Writely.
    let caret_rect = CaretRect {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
        physical: false,
    };
    match kind {
        crate::ime_bridge::FocusedKind::Text(focused) => Ok(Some(FieldSnapshot {
            pid: 0,
            text: focused.text.clone(),
            caret: focused.caret,
            caret_known: true,
            caret_rect,
            selection: focused.selection,
            window_start_utf16: 0,
            field_len_utf16: super::utf16_len(&focused.text),
            handle: PlatformHandle::Bridge {
                sender,
                context,
                selection: false,
            },
        })),
        crate::ime_bridge::FocusedKind::SelectOnly => {
            // Errors propagate as Err(..) so the existing popup shows them.
            let text = crate::ime_bridge::select_and_wait(&sender, &context)?;
            let len = text.chars().count();
            Ok(Some(FieldSnapshot {
                pid: 0,
                text: text.clone(),
                caret: len,
                caret_known: true,
                caret_rect,
                selection: if len > 0 { Some(0..len) } else { None },
                window_start_utf16: 0,
                field_len_utf16: super::utf16_len(&text),
                handle: PlatformHandle::Bridge {
                    sender,
                    context,
                    selection: true,
                },
            }))
        }
    }
}

fn replace_range_bridge(
    snap: &FieldSnapshot,
    range: Range<usize>,
    sender: &str,
    context: &str,
    selection: bool,
    new_text: &str,
) -> Result<(), String> {
    if selection {
        // The bridge replaces the whole active selection; a partial range
        // (e.g. after trimming) can't be mapped onto it.
        let len = snap.text.chars().count();
        if range != (0..len) {
            return Err(
                "Writely can only replace the whole selected text in this app.".to_string(),
            );
        }
        return crate::ime_bridge::replace_selection_and_wait(sender, context, new_text);
    }
    let Some(current) = crate::ime_bridge::current_text(sender, context) else {
        return Err(
            "The input method closed the text field before the replacement finished.".to_string(),
        );
    };
    if current != snap.text {
        return Err(
            "The text changed since Writely checked it. Click W again to check the new text."
                .to_string(),
        );
    }
    // No re-read verification: the bridge's ok is the confirmation.
    crate::ime_bridge::replace_and_wait(
        sender,
        context,
        range.start as u32,
        range.end as u32,
        new_text,
    )
}

pub(crate) fn focused_field() -> Result<Option<FieldSnapshot>, String> {
    if uses_ime_bridge() {
        return focused_field_bridge();
    }
    pollster::block_on(focused_field_async())
}

pub(crate) fn read_text(handle: &PlatformHandle) -> Result<String, String> {
    if let PlatformHandle::Bridge {
        sender,
        context,
        selection,
    } = handle
    {
        if *selection {
            return Err(
                "This app doesn't share its text; Writely works on the selection instead."
                    .to_string(),
            );
        }
        return crate::ime_bridge::current_text(sender, context)
            .ok_or_else(|| "The text field is no longer available.".to_string());
    }
    pollster::block_on(read_text_async(handle))
}

pub(crate) fn replace_range(
    snap: &FieldSnapshot,
    range: Range<usize>,
    new_text: &str,
) -> Result<(), String> {
    if let PlatformHandle::Bridge {
        sender,
        context,
        selection,
    } = &snap.handle
    {
        return replace_range_bridge(snap, range, sender, context, *selection, new_text);
    }
    pollster::block_on(replace_range_async(snap, range, new_text))
}
