use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::ops::Range;
use std::ptr::NonNull;

use objc2_application_services::{
    kAXTrustedCheckOptionPrompt, AXError, AXIsProcessTrustedWithOptions, AXUIElement, AXValue,
    AXValueType,
};
use objc2_core_foundation::{
    kCFBooleanFalse, kCFBooleanTrue, CFBoolean, CFDictionary, CFIndex, CFRange, CFRetained,
    CFString, CFType, CGPoint, CGRect, CGSize, Type,
};

use super::{char_to_utf16_offset, replaced_text, utf16_to_char_offset, CaretRect, FieldSnapshot};

pub(crate) struct PlatformHandle {
    element: CFRetained<AXUIElement>,
}

const PERMISSION_ERROR: &str = "Writely needs Accessibility permission to see where you're typing. Open System Settings › Privacy & Security › Accessibility, turn on Writely, then restart Writely.";

fn read_error(error: AXError) -> String {
    format!("Could not read the focused text field: AXError {}", error.0)
}

fn copy_attr(element: &AXUIElement, name: &str) -> Result<CFRetained<CFType>, AXError> {
    let attr = CFString::from_str(name);
    let mut raw: *const CFType = std::ptr::null();
    let out = NonNull::from(&mut raw);
    // SAFETY: element and attr are valid CF objects and out points at a live local slot.
    let error = unsafe { element.copy_attribute_value(&attr, out) };
    if error != AXError::Success {
        return Err(error);
    }
    let Some(ptr) = NonNull::new(raw.cast_mut()) else {
        return Err(AXError::Failure);
    };
    // SAFETY: a successful copy returns a +1-retained CF object owned by the caller.
    Ok(unsafe { CFRetained::from_raw(ptr) })
}

fn copy_string(element: &AXUIElement, name: &str) -> Result<Option<String>, AXError> {
    let value = copy_attr(element, name)?;
    match value.downcast_ref::<CFString>() {
        Some(text) => Ok(Some(text.to_string())),
        None => Ok(None),
    }
}

fn copy_ax_value(element: &AXUIElement, name: &str) -> Result<CFRetained<AXValue>, AXError> {
    let value = copy_attr(element, name)?;
    value.downcast::<AXValue>().map_err(|_| AXError::Failure)
}

fn ax_range(value: &AXValue) -> Option<CFRange> {
    let mut range = MaybeUninit::<CFRange>::uninit();
    let ptr = NonNull::new(range.as_mut_ptr().cast::<c_void>())?;
    // SAFETY: ptr points at writable CFRange storage and value is an AXValue.
    if unsafe { value.value(AXValueType::CFRange, ptr) } {
        // SAFETY: AXValueGetValue filled the CFRange when it returned true.
        Some(unsafe { range.assume_init() })
    } else {
        None
    }
}

fn ax_point(value: &AXValue) -> Option<CGPoint> {
    let mut point = MaybeUninit::<CGPoint>::uninit();
    let ptr = NonNull::new(point.as_mut_ptr().cast::<c_void>())?;
    // SAFETY: ptr points at writable CGPoint storage and value is an AXValue.
    if unsafe { value.value(AXValueType::CGPoint, ptr) } {
        // SAFETY: AXValueGetValue filled the CGPoint when it returned true.
        Some(unsafe { point.assume_init() })
    } else {
        None
    }
}

fn ax_size(value: &AXValue) -> Option<CGSize> {
    let mut size = MaybeUninit::<CGSize>::uninit();
    let ptr = NonNull::new(size.as_mut_ptr().cast::<c_void>())?;
    // SAFETY: ptr points at writable CGSize storage and value is an AXValue.
    if unsafe { value.value(AXValueType::CGSize, ptr) } {
        // SAFETY: AXValueGetValue filled the CGSize when it returned true.
        Some(unsafe { size.assume_init() })
    } else {
        None
    }
}

fn ax_rect(value: &AXValue) -> Option<CGRect> {
    let mut rect = MaybeUninit::<CGRect>::uninit();
    let ptr = NonNull::new(rect.as_mut_ptr().cast::<c_void>())?;
    // SAFETY: ptr points at writable CGRect storage and value is an AXValue.
    if unsafe { value.value(AXValueType::CGRect, ptr) } {
        // SAFETY: AXValueGetValue filled the CGRect when it returned true.
        Some(unsafe { rect.assume_init() })
    } else {
        None
    }
}

fn make_ax_range(location: usize, length: usize) -> Option<CFRetained<AXValue>> {
    let mut range = CFRange {
        location: location as CFIndex,
        length: length as CFIndex,
    };
    let ptr = NonNull::new((&mut range as *mut CFRange).cast::<c_void>())?;
    // SAFETY: ptr points at a live CFRange for the duration of the call.
    unsafe { AXValue::new(AXValueType::CFRange, ptr) }
}

fn set_attr(element: &AXUIElement, name: &str, value: &CFType) -> AXError {
    let attr = CFString::from_str(name);
    // SAFETY: element, attr and value are valid CF objects of the expected kinds.
    unsafe { element.set_attribute_value(&attr, value) }
}

fn bounds_for_range(
    element: &AXUIElement,
    location: usize,
    length: usize,
) -> Result<CGRect, AXError> {
    let param = make_ax_range(location, length).ok_or(AXError::Failure)?;
    let param_type: &CFType = &param;
    let attr = CFString::from_str("AXBoundsForRange");
    let mut raw: *const CFType = std::ptr::null();
    let out = NonNull::from(&mut raw);
    // SAFETY: element and attr are valid and out points at a live local slot.
    let error = unsafe { element.copy_parameterized_attribute_value(&attr, param_type, out) };
    if error != AXError::Success {
        return Err(error);
    }
    let Some(ptr) = NonNull::new(raw.cast_mut()) else {
        return Err(AXError::Failure);
    };
    // SAFETY: a successful copy returns a +1-retained CF object owned by the caller.
    let value = unsafe { CFRetained::from_raw(ptr) };
    let ax_value = value.downcast::<AXValue>().map_err(|_| AXError::Failure)?;
    ax_rect(&ax_value).ok_or(AXError::Failure)
}

fn app_name(pid: u32) -> String {
    // SAFETY: creating an application element for a live pid is a plain FFI call.
    let app = unsafe { AXUIElement::new_application(pid as i32) };
    match copy_string(&app, "AXTitle") {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        _ => "The app".to_string(),
    }
}

pub(crate) fn check_permission(prompt: bool) -> Result<(), String> {
    let key: &CFString = {
        // SAFETY: kAXTrustedCheckOptionPrompt is a framework-provided constant string.
        unsafe { kAXTrustedCheckOptionPrompt }
    };
    let value: &CFBoolean = match if prompt {
        // SAFETY: kCFBooleanTrue is a framework-provided constant.
        unsafe { kCFBooleanTrue }
    } else {
        // SAFETY: kCFBooleanFalse is a framework-provided constant.
        unsafe { kCFBooleanFalse }
    } {
        Some(value) => value,
        None => {
            eprintln!("Writely: could not read the Accessibility prompt option constant.");
            return Err(PERMISSION_ERROR.to_string());
        }
    };
    let options: CFRetained<CFDictionary<CFString, CFBoolean>> =
        CFDictionary::from_slices(&[key], &[value]);
    let ptr = CFRetained::as_ptr(&options).cast::<CFDictionary>();
    // SAFETY: CFDictionary generics are phantom data, so the untyped borrow shares the layout and options outlives it.
    let untyped: &CFDictionary = unsafe { ptr.as_ref() };
    // SAFETY: untyped is a valid CFDictionary with the prompt option for this call.
    let trusted = unsafe { AXIsProcessTrustedWithOptions(Some(untyped)) };
    if trusted {
        Ok(())
    } else {
        Err(PERMISSION_ERROR.to_string())
    }
}

pub(crate) fn focused_field() -> Result<Option<FieldSnapshot>, String> {
    // SAFETY: creating the system-wide element is a plain FFI call with no arguments.
    let system = unsafe { AXUIElement::new_system_wide() };
    let focused = match copy_attr(&system, "AXFocusedUIElement") {
        Ok(value) => value,
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {
            return Ok(None);
        }
        Err(error) => return Err(read_error(error)),
    };
    let element_ref: &AXUIElement = {
        let ptr = CFRetained::as_ptr(&focused).cast::<AXUIElement>();
        // SAFETY: AXFocusedUIElement yields an AXUIElement valid while focused is alive.
        unsafe { ptr.as_ref() }
    };
    let element: CFRetained<AXUIElement> = element_ref.retain();

    let mut pid: i32 = 0;
    let pid_out = NonNull::from(&mut pid);
    // SAFETY: pid_out points at a writable i32 for the duration of the call.
    let pid_error = unsafe { element.pid(pid_out) };
    if pid_error != AXError::Success {
        return Err(read_error(pid_error));
    }
    if pid < 0 || pid as u32 == std::process::id() {
        return Ok(None);
    }

    match copy_string(&element, "AXSubrole") {
        Ok(Some(subrole)) if subrole == "AXSecureTextField" => return Ok(None),
        Ok(_) => {}
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {}
        Err(error) => return Err(read_error(error)),
    }

    let text = match copy_string(&element, "AXValue") {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(None),
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {
            return Ok(None);
        }
        Err(error) => return Err(read_error(error)),
    };

    let range_value = match copy_ax_value(&element, "AXSelectedTextRange") {
        Ok(value) => value,
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => {
            return Ok(None);
        }
        Err(error) => return Err(read_error(error)),
    };
    let selected = match ax_range(&range_value) {
        Some(range) => range,
        None => return Err(read_error(AXError::Failure)),
    };
    let caret_utf16 = if selected.location < 0 {
        0usize
    } else {
        selected.location as usize
    };
    let caret = utf16_to_char_offset(&text, caret_utf16);

    let caret_rect = caret_rect(&element, caret_utf16)?;

    Ok(Some(FieldSnapshot {
        pid: pid as u32,
        text,
        caret,
        caret_rect,
        handle: PlatformHandle { element },
    }))
}

fn caret_rect(element: &AXUIElement, caret_utf16: usize) -> Result<CaretRect, String> {
    if let Ok(rect) = bounds_for_range(element, caret_utf16, 0) {
        if rect.size.width != 0.0 || rect.size.height != 0.0 {
            return Ok(CaretRect {
                x: rect.origin.x,
                y: rect.origin.y,
                width: rect.size.width,
                height: rect.size.height,
                physical: false,
            });
        }
    }
    if caret_utf16 > 0 {
        if let Ok(rect) = bounds_for_range(element, caret_utf16 - 1, 1) {
            if rect.size.width != 0.0 || rect.size.height != 0.0 {
                return Ok(CaretRect {
                    x: rect.origin.x + rect.size.width,
                    y: rect.origin.y,
                    width: 0.0,
                    height: rect.size.height,
                    physical: false,
                });
            }
        }
    }
    let position_value = copy_ax_value(element, "AXPosition").map_err(read_error)?;
    let size_value = copy_ax_value(element, "AXSize").map_err(read_error)?;
    let position = ax_point(&position_value).ok_or_else(|| read_error(AXError::Failure))?;
    let size = ax_size(&size_value).ok_or_else(|| read_error(AXError::Failure))?;
    eprintln!(
        "Writely: the caret position is approximate; using the bottom-left corner of the focused field."
    );
    Ok(CaretRect {
        x: position.x,
        y: position.y + size.height,
        width: 0.0,
        height: 0.0,
        physical: false,
    })
}

pub(crate) fn read_text(handle: &PlatformHandle) -> Result<String, String> {
    match copy_string(&handle.element, "AXValue") {
        Ok(Some(text)) => Ok(text),
        Ok(None) => Err(read_error(AXError::Failure)),
        Err(error) => Err(read_error(error)),
    }
}

pub(crate) fn replace_range(
    snap: &FieldSnapshot,
    range: Range<usize>,
    new_text: &str,
) -> Result<(), String> {
    let element: &AXUIElement = &snap.handle.element;
    let name = app_name(snap.pid);
    let start = char_to_utf16_offset(&snap.text, range.start);
    let end = char_to_utf16_offset(&snap.text, range.end);
    let length = end.saturating_sub(start);

    let range_value = match make_ax_range(start, length) {
        Some(value) => value,
        None => {
            return Err(format!(
                "{name} didn't accept the replacement text (AXError {}).",
                AXError::Failure.0
            ));
        }
    };
    let range_type: &CFType = &range_value;
    let select_error = set_attr(element, "AXSelectedTextRange", range_type);
    if select_error != AXError::Success {
        return Err(format!(
            "{name} didn't accept the replacement text (AXError {}).",
            select_error.0
        ));
    }

    let new_cf = CFString::from_str(new_text);
    let new_type: &CFType = &new_cf;
    let selected_error = set_attr(element, "AXSelectedText", new_type);
    let expected = replaced_text(&snap.text, &range, new_text);
    if selected_error != AXError::Success {
        eprintln!(
            "Writely: setting AXSelectedText failed (AXError {}); replaced the whole field text instead.",
            selected_error.0
        );
        let whole_cf = CFString::from_str(&expected);
        let whole_type: &CFType = &whole_cf;
        let value_error = set_attr(element, "AXValue", whole_type);
        if value_error != AXError::Success {
            return Err(format!(
                "{name} didn't accept the replacement text (AXError {}).",
                value_error.0
            ));
        }
    }

    match read_text(&snap.handle) {
        Ok(actual) if actual == expected => Ok(()),
        Ok(_) => Err(format!(
            "{name} didn't accept the replacement text (AXError {}).",
            AXError::Failure.0
        )),
        Err(error) => Err(format!(
            "{name} didn't accept the replacement text ({error})."
        )),
    }
}
