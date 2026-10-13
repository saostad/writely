use std::ops::Range;

use atspi::proxy::accessible::{AccessibleProxy, ObjectRefExt};
use atspi::proxy::proxy_ext::ProxyExt;
use atspi::{AccessibilityConnection, CoordType, ObjectRefOwned, Role, State};

use super::{replaced_text, CaretInfo, CaretRect, FieldSnapshot};

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
    let conn = AccessibilityConnection::new().await.map_err(read_error)?;
    let root = conn
        .root_accessible_on_registry()
        .await
        .map_err(read_error)?;

    let mut stack: Vec<AccessibleProxy<'_>> = vec![root];
    let mut visited = 0usize;
    while let Some(element) = stack.pop() {
        visited += 1;
        if visited > 4096 {
            break;
        }
        let state = match element.get_state().await {
            Ok(state) => state,
            Err(_) => continue,
        };
        if state.contains(State::Focused) {
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
            let object = ObjectRefOwned::try_from(&element).map_err(read_error)?;
            return Ok(Some(FieldSnapshot {
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
                handle: PlatformHandle { object },
            }));
        }
        match element.get_children().await {
            Ok(children) => {
                for child in children {
                    if child.is_null() {
                        continue;
                    }
                    match child.as_accessible_proxy(conn.connection()).await {
                        Ok(child_proxy) => stack.push(child_proxy),
                        Err(error) => {
                            eprintln!("Writely: could not read a text field on Linux: {error}");
                        }
                    }
                }
            }
            Err(error) => {
                eprintln!("Writely: could not list text fields on Linux: {error}");
            }
        }
    }
    Ok(None)
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
