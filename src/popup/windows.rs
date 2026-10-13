use tao::event_loop::EventLoopProxy;

use crate::text_access::CaretRect;
use crate::UserEvent;

// The Windows popup (a reused WS_POPUP window with a STATIC label and a
// Replace BUTTON) is implemented in the next item of this round.

pub fn init(proxy: EventLoopProxy<UserEvent>) {
    let _ = proxy;
}

pub fn show_message(text: &str, _is_error: bool, _show_button: bool, _caret: &CaretRect) {
    // Not implemented yet; log instead of failing silently.
    eprintln!("Writely: the suggestion popup is not implemented on Windows yet: {text}");
}

pub fn hide() {}

pub fn is_visible() -> bool {
    false
}
