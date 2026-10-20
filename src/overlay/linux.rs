use std::cell::RefCell;
use std::sync::OnceLock;

use gtk::prelude::*;
use gtk::{Button, Window, WindowType};
use tao::event_loop::EventLoopProxy;

use crate::text_access::CaretRect;
use crate::UserEvent;

const ICON_SIZE: i32 = 20;

static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();

struct IconState {
    window: Window,
    x: i32,
    y: i32,
}

thread_local! {
    static STATE: RefCell<Option<IconState>> = const { RefCell::new(None) };
}

pub fn init(proxy: EventLoopProxy<UserEvent>) {
    if PROXY.set(proxy).is_err() {
        // Keep the first proxy installed for this process.
    }
}

fn ensure_window() -> Option<Window> {
    if let Some(window) =
        STATE.with(|state| state.borrow().as_ref().map(|icon| icon.window.clone()))
    {
        return Some(window);
    }
    if let Err(error) = gtk::init() {
        // GTK may already be initialised by the tray/event-loop stack; only
        // fail when there is no usable display at all.
        eprintln!("Writely: GTK initialisation note on Linux: {error}");
        if !gtk::is_initialized() {
            eprintln!("Writely: could not show the icon: GTK is not initialized.");
            return None;
        }
    }
    let window = Window::new(WindowType::Popup);
    window.set_title("Writely");
    window.set_default_size(ICON_SIZE, ICON_SIZE);
    window.set_resizable(false);
    window.set_accept_focus(false);
    window.set_skip_taskbar_hint(true);
    let button = Button::with_label("W");
    button.set_can_focus(false);
    button.set_tooltip_text(Some("Check grammar"));
    button.connect_clicked(|_| {
        if let Some(proxy) = PROXY.get() {
            if let Err(error) = proxy.send_event(UserEvent::IconClicked) {
                eprintln!("Writely: could not deliver the icon click event: {error}");
            }
        }
    });
    window.add(&button);
    STATE.with(|state| {
        *state.borrow_mut() = Some(IconState {
            window: window.clone(),
            x: 0,
            y: 0,
        });
    });
    Some(window)
}

pub fn show_at(rect: &CaretRect) {
    // An all-zero box means the app gave no screen position (native Wayland
    // apps); hide rather than park the icon in the screen's top-left corner.
    if rect.x == 0.0 && rect.y == 0.0 && rect.width == 0.0 && rect.height == 0.0 {
        hide();
        return;
    }
    if ensure_window().is_none() {
        return;
    };
    let x = (rect.x + rect.width + 4.0) as i32;
    let y = (rect.y + rect.height / 2.0 - ICON_SIZE as f64 / 2.0) as i32;
    STATE.with(|state| {
        if let Some(icon) = state.borrow_mut().as_mut() {
            // The window is kept alive and reused; it is never re-created.
            if icon.window.is_visible() && icon.x == x && icon.y == y {
                return;
            }
            icon.window.move_(x, y);
            icon.window.show_all();
            icon.x = x;
            icon.y = y;
        }
    });
}

pub fn hide() {
    STATE.with(|state| {
        if let Some(icon) = state.borrow().as_ref() {
            // hide() runs on every keystroke; skip the call when already hidden.
            if icon.window.is_visible() {
                icon.window.hide();
            }
        }
    });
}
