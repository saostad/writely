use tao::event_loop::EventLoopProxy;

use crate::UserEvent;

pub struct WatcherInner {
    _private: (),
}

impl WatcherInner {
    pub fn start(_proxy: EventLoopProxy<UserEvent>) -> Result<Self, String> {
        Ok(Self { _private: () })
    }

    pub fn handle_caret_changed(&mut self) {}
}
