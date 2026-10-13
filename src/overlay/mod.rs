use tao::event_loop::EventLoopProxy;

use crate::text_access::CaretRect;
use crate::UserEvent;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Stores the event-loop proxy so a click on the icon can send
/// `UserEvent::IconClicked`. Call once at startup.
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

/// Shows the icon next to the caret rectangle (top-left screen coords).
/// Hides it when Writely's Settings window is key.
pub fn show_at(rect: &CaretRect) {
    #[cfg(target_os = "macos")]
    {
        macos::show_at(rect);
    }
    #[cfg(target_os = "windows")]
    {
        windows::show_at(rect);
    }
    #[cfg(target_os = "linux")]
    {
        linux::show_at(rect);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = rect;
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
