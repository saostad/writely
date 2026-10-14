use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use atspi::events::object::{StateChangedEvent, TextCaretMovedEvent, TextChangedEvent};
use atspi::{AccessibilityConnection, Event, ObjectEvents, State};
use futures_lite::StreamExt;
use tao::event_loop::EventLoopProxy;

use crate::UserEvent;

static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();
static PENDING: AtomicBool = AtomicBool::new(false);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn flag_caret_changed() {
    if !PENDING.swap(true, Ordering::SeqCst) {
        if let Some(proxy) = PROXY.get() {
            if proxy.send_event(UserEvent::CaretChanged).is_err() {
                PENDING.store(false, Ordering::SeqCst);
            }
        } else {
            PENDING.store(false, Ordering::SeqCst);
        }
    }
}

pub fn clear_pending() {
    PENDING.store(false, Ordering::SeqCst);
}

fn log_once(message: String) {
    let mut last = LAST_ERROR
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if last.as_deref() != Some(message.as_str()) {
        eprintln!("Writely: {message}");
        *last = Some(message);
    }
}

pub fn is_wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}

pub struct WatcherInner {
    _private: (),
}

impl WatcherInner {
    pub fn start(proxy: EventLoopProxy<UserEvent>) -> Result<Self, String> {
        if PROXY.set(proxy).is_err() {
            // Keep the first proxy installed for this process.
        }
        // The thread blocks on the AT-SPI event stream; no polling, no timers.
        std::thread::Builder::new()
            .name("writely-atspi-watch".to_string())
            .spawn(watch_thread)
            .map_err(|error| format!("Could not start the text cursor watcher: {error}"))?;
        Ok(Self { _private: () })
    }

    pub fn handle_caret_changed(&mut self) {
        clear_pending();
    }
}

fn watch_thread() {
    pollster::block_on(watch_loop());
}

async fn watch_loop() {
    let connection = match AccessibilityConnection::new().await {
        Ok(connection) => connection,
        Err(error) => {
            log_once(format!(
                "Could not connect to the accessibility bus: {error}"
            ));
            return;
        }
    };
    for registration in [
        connection.register_event::<TextCaretMovedEvent>().await,
        connection.register_event::<StateChangedEvent>().await,
        connection.register_event::<TextChangedEvent>().await,
    ] {
        if let Err(error) = registration {
            log_once(format!(
                "Could not subscribe to text cursor events: {error}"
            ));
            return;
        }
    }
    let mut stream = connection.event_stream();
    while let Some(next) = stream.next().await {
        let event = match next {
            Ok(event) => event,
            Err(error) => {
                log_once(format!("Could not read an accessibility event: {error}"));
                continue;
            }
        };
        match event {
            Event::Object(ObjectEvents::TextCaretMoved(moved)) => {
                crate::text_access::set_focused(Some(moved.item));
                flag_caret_changed();
            }
            Event::Object(ObjectEvents::StateChanged(changed))
                if changed.state == State::Focused =>
            {
                if changed.enabled {
                    crate::text_access::set_focused(Some(changed.item));
                } else {
                    crate::text_access::clear_focused_if(&changed.item);
                }
                flag_caret_changed();
            }
            Event::Object(ObjectEvents::TextChanged(_)) => {
                flag_caret_changed();
            }
            _ => {}
        }
    }
}
