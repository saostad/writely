use tao::event_loop::EventLoopProxy;

use crate::UserEvent;

pub fn is_wayland_without_x11() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var("GDK_BACKEND")
            .map(|value| value != "x11")
            .unwrap_or(true)
}

pub struct WatcherInner {
    _private: (),
}

impl WatcherInner {
    pub fn start(_proxy: EventLoopProxy<UserEvent>) -> Result<Self, String> {
        Ok(Self { _private: () })
    }

    pub fn handle_caret_changed(&mut self) {}
}
