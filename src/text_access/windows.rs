use std::ops::Range;

use uiautomation::core::{UIAutomation, UIElement};
use uiautomation::patterns::{UITextPattern, UIValuePattern};
use uiautomation::types::TextPatternRangeEndpoint;

use super::{replaced_text, CaretInfo, CaretRect, FieldSnapshot};

pub(crate) struct PlatformHandle {
    element: UIElement,
}

fn read_error(error: uiautomation::Error) -> String {
    format!("Could not read the focused text field: {error}")
}

pub(crate) fn check_permission(_prompt: bool) -> Result<(), String> {
    Ok(())
}

fn document_text(element: &UIElement) -> Result<String, String> {
    let pattern = element.get_pattern::<UITextPattern>().map_err(read_error)?;
    let document = pattern.get_document_range().map_err(read_error)?;
    document.get_text(-1).map_err(read_error)
}

pub(crate) fn focused_field() -> Result<Option<FieldSnapshot>, String> {
    let automation = UIAutomation::new().map_err(read_error)?;
    let element = automation.get_focused_element().map_err(read_error)?;

    let pid = element.get_process_id().map_err(read_error)?;
    if pid == std::process::id() {
        return Ok(None);
    }
    match element.is_password() {
        Ok(true) => return Ok(None),
        Ok(false) => {}
        Err(error) => return Err(read_error(error)),
    }

    let pattern = match element.get_pattern::<UITextPattern>() {
        Ok(pattern) => pattern,
        Err(_) => return Ok(None),
    };
    let document = pattern.get_document_range().map_err(read_error)?;
    let text = document.get_text(-1).map_err(read_error)?;

    let caret = match pattern.get_selection() {
        Ok(selections) => match selections.into_iter().next() {
            Some(selection) => {
                let mut before = document.clone();
                before
                    .move_endpoint_by_range(
                        TextPatternRangeEndpoint::End,
                        &selection,
                        TextPatternRangeEndpoint::Start,
                    )
                    .map_err(read_error)?;
                let before_text = before.get_text(-1).map_err(read_error)?;
                before_text.chars().count()
            }
            None => 0,
        },
        Err(error) => {
            eprintln!("Writely: could not read the caret position on Windows: {error}");
            0
        }
    };

    let rect = element.get_bounding_rectangle().map_err(read_error)?;
    eprintln!(
        "Writely: the caret position is approximate; using the focused field's rectangle on Windows."
    );
    let caret_rect = CaretRect {
        x: rect.get_left() as f64,
        y: rect.get_top() as f64,
        width: (rect.get_right() - rect.get_left()).max(0) as f64,
        height: (rect.get_bottom() - rect.get_top()).max(0) as f64,
        physical: true,
    };

    Ok(Some(FieldSnapshot {
        pid,
        text,
        caret,
        caret_rect,
        handle: PlatformHandle { element },
    }))
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

pub(crate) fn read_text(handle: &PlatformHandle) -> Result<String, String> {
    document_text(&handle.element)
}

pub(crate) fn replace_range(
    snap: &FieldSnapshot,
    range: Range<usize>,
    new_text: &str,
) -> Result<(), String> {
    let pattern = match snap.handle.element.get_pattern::<UIValuePattern>() {
        Ok(pattern) => pattern,
        Err(_) => {
            return Err("This app's text field doesn't let Writely replace text.".to_string());
        }
    };
    match pattern.is_readonly() {
        Ok(true) => {
            return Err("This app's text field doesn't let Writely replace text.".to_string());
        }
        Ok(false) => {}
        Err(error) => return Err(format!("Could not replace the text: {error}")),
    }
    let expected = replaced_text(&snap.text, &range, new_text);
    pattern
        .set_value(&expected)
        .map_err(|error| format!("Could not replace the text: {error}"))?;
    let actual = read_text(&snap.handle)?;
    if actual != expected {
        return Err("The app didn't accept the replacement text.".to_string());
    }
    Ok(())
}
