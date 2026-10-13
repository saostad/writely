use std::ops::Range;
use std::sync::Mutex;

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

pub(crate) struct PlatformHandle {
    object: ObjectRefOwned,
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

    let (x, y, width, height) = match text_proxy
        .get_character_extents(caret_offset, CoordType::Screen)
        .await
    {
        Ok(extents) => extents,
        Err(_) if caret_offset > 0 => {
            let (px, py, pwidth, pheight) = text_proxy
                .get_character_extents(caret_offset - 1, CoordType::Screen)
                .await
                .map_err(read_error)?;
            (px + pwidth, py, 0, pheight)
        }
        Err(error) => return Err(read_error(error)),
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
        handle: PlatformHandle { object },
    }))
}

async fn read_text_async(handle: &PlatformHandle) -> Result<String, String> {
    let conn = AccessibilityConnection::new().await.map_err(read_error)?;
    let element = handle
        .object
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
    let conn = AccessibilityConnection::new().await.map_err(read_error)?;
    let element = snap
        .handle
        .object
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
    match focused_field()? {
        Some(snap) => Ok(Some(CaretInfo {
            pid: snap.pid,
            caret_rect: snap.caret_rect,
        })),
        None => Ok(None),
    }
}

pub(crate) fn focused_field() -> Result<Option<FieldSnapshot>, String> {
    pollster::block_on(focused_field_async())
}

pub(crate) fn read_text(handle: &PlatformHandle) -> Result<String, String> {
    pollster::block_on(read_text_async(handle))
}

pub(crate) fn replace_range(
    snap: &FieldSnapshot,
    range: Range<usize>,
    new_text: &str,
) -> Result<(), String> {
    pollster::block_on(replace_range_async(snap, range, new_text))
}
