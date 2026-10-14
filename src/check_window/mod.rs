use tao::event_loop::EventLoopProxy;

use crate::UserEvent;

#[cfg(target_os = "linux")]
mod linux;

/// Stores the event-loop proxy so a finished check can send
/// `UserEvent::TextCheckResult`. Call once at startup.
pub fn init(proxy: EventLoopProxy<UserEvent>) {
    #[cfg(target_os = "linux")]
    {
        linux::init(proxy);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = proxy;
    }
}

/// Opens the "Check text" window (Linux only for now).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn show() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        linux::show()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("The Check text window isn't available on this platform yet.".to_string())
    }
}

/// Shows a finished check, dropping it when a newer Check or Clear happened
/// or the window was closed.
pub fn handle_result(generation: u64, result: Result<String, String>) {
    #[cfg(target_os = "linux")]
    {
        linux::handle_result(generation, result);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (generation, result);
    }
}
