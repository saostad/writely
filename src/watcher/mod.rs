#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use tao::event_loop::EventLoopProxy;

use crate::UserEvent;

pub struct Watcher {
    #[cfg(target_os = "macos")]
    inner: macos::WatcherInner,
    #[cfg(target_os = "windows")]
    inner: windows::WatcherInner,
    #[cfg(target_os = "linux")]
    inner: linux::WatcherInner,
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    inner: (),
}

impl Watcher {
    pub fn start(proxy: EventLoopProxy<UserEvent>) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        {
            let inner = macos::WatcherInner::start(proxy)?;
            Ok(Self { inner })
        }
        #[cfg(target_os = "windows")]
        {
            let inner = windows::WatcherInner::start(proxy)?;
            Ok(Self { inner })
        }
        #[cfg(target_os = "linux")]
        {
            let inner = linux::WatcherInner::start(proxy)?;
            Ok(Self { inner })
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            let _ = proxy;
            Ok(Self { inner: () })
        }
    }

    pub fn handle_caret_changed(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.inner.handle_caret_changed();
        }
        #[cfg(target_os = "windows")]
        {
            self.inner.handle_caret_changed();
        }
        #[cfg(target_os = "linux")]
        {
            self.inner.handle_caret_changed();
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {}
    }
}

#[cfg(target_os = "linux")]
pub fn is_wayland_without_x11() -> bool {
    linux::is_wayland_without_x11()
}

#[cfg(not(target_os = "linux"))]
pub fn is_wayland_without_x11() -> bool {
    false
}

pub const WAYLAND_MESSAGE: &str =
    "Writely can't follow the text cursor on Wayland. Log in with an X11 session to use it.";
