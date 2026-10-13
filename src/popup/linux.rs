use tao::event_loop::EventLoopProxy;

use crate::text_access::CaretRect;
use crate::UserEvent;

// The Linux popup (a reused gtk Window with a wrapping Label and a Replace
// Button) is implemented in the next item of this round.

pub fn init(proxy: EventLoopProxy<UserEvent>) {
    let _ = proxy;
}

pub fn show_message(text: &str, _is_error: bool, _show_button: bool, _caret: &CaretRect) {
    // Not implemented yet; log instead of failing silently.
    eprintln!("Writely: the suggestion popup is not implemented on Linux yet: {text}");
}

pub fn hide() {}

pub fn is_visible() -> bool {
    false
}
