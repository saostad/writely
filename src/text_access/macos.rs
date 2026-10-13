use super::{CaretRect, FieldSnapshot};
use std::ops::Range;

pub(crate) struct PlatformHandle;

pub(crate) fn check_permission(_prompt: bool) -> Result<(), String> {
    Ok(())
}

pub(crate) fn focused_field() -> Result<Option<FieldSnapshot>, String> {
    Ok(None)
}

pub(crate) fn read_text(_handle: &PlatformHandle) -> Result<String, String> {
    Err("Could not read the focused text field: not implemented yet.".to_string())
}

pub(crate) fn replace_range(
    _snap: &FieldSnapshot,
    _range: Range<usize>,
    _new_text: &str,
) -> Result<(), String> {
    let _ = CaretRect {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
        physical: false,
    };
    Err("Writely cannot replace text yet.".to_string())
}
