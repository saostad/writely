use tao::event_loop::EventLoopProxy;

use crate::text_access::CaretRect;
use crate::UserEvent;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Stores the event-loop proxy so the Replace button can send
/// `UserEvent::ReplaceClicked`. Call once at startup.
pub fn init(proxy: EventLoopProxy<UserEvent>) {
    #[cfg(target_os = "macos")]
    {
        macos::init(proxy);
    }
    #[cfg(target_os = "windows")]
    {
        windows::init(proxy);
    }
    #[cfg(target_os = "linux")]
    {
        linux::init(proxy);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = proxy;
    }
}

/// Shows the popup near the caret rectangle (top-left screen coords).
/// `is_error` renders the text in red; `show_button` shows Replace.
pub fn show_message(text: &str, is_error: bool, show_button: bool, caret: &CaretRect) {
    #[cfg(target_os = "macos")]
    {
        macos::show_message(text, is_error, show_button, caret);
    }
    #[cfg(target_os = "windows")]
    {
        windows::show_message(text, is_error, show_button, caret);
    }
    #[cfg(target_os = "linux")]
    {
        linux::show_message(text, is_error, show_button, caret);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = (text, is_error, show_button, caret);
    }
}

pub fn hide() {
    #[cfg(target_os = "macos")]
    {
        macos::hide();
    }
    #[cfg(target_os = "windows")]
    {
        windows::hide();
    }
    #[cfg(target_os = "linux")]
    {
        linux::hide();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {}
}

pub fn is_visible() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::is_visible()
    }
    #[cfg(target_os = "windows")]
    {
        windows::is_visible()
    }
    #[cfg(target_os = "linux")]
    {
        linux::is_visible()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}
