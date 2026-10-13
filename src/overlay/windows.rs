use tao::event_loop::EventLoopProxy;

use crate::text_access::CaretRect;
use crate::UserEvent;

pub fn init(_proxy: EventLoopProxy<UserEvent>) {}

pub fn show_at(_rect: &CaretRect) {}

pub fn hide() {}
