use std::cell::RefCell;
use std::sync::OnceLock;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, Label, Orientation, Window, WindowType};
use tao::event_loop::EventLoopProxy;

use crate::text_access::CaretRect;
use crate::UserEvent;

static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();

struct PopupState {
    window: Window,
    label: Label,
    button: Button,
}

thread_local! {
    static STATE: RefCell<Option<PopupState>> = const { RefCell::new(None) };
    // Bridge mode: the (sender, context) the message is shown on, if any.
    static SHOWN_ON: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

pub fn init(proxy: EventLoopProxy<UserEvent>) {
    if PROXY.set(proxy).is_err() {
        // Keep the first proxy installed for this process.
    }
}

fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn ensure_window() -> Option<(Window, Label, Button)> {
    if let Some((window, label, button)) = STATE.with(|state| {
        state.borrow().as_ref().map(|popup| {
            (
                popup.window.clone(),
                popup.label.clone(),
                popup.button.clone(),
            )
        })
    }) {
        return Some((window, label, button));
    }
    if let Err(error) = gtk::init() {
        // GTK may already be initialised by the tray/event-loop stack; only
        // fail when there is no usable display at all.
        eprintln!("Writely: GTK initialisation note on Linux: {error}");
        if !gtk::is_initialized() {
            eprintln!("Writely: could not show the suggestion: GTK is not initialized.");
            return None;
        }
    }
    let window = Window::new(WindowType::Popup);
    window.set_title("Writely Suggestion");
    window.set_resizable(false);
    window.set_accept_focus(false);
    window.set_skip_taskbar_hint(true);

    let container = GtkBox::new(Orientation::Vertical, 8);
    container.set_margin_top(10);
    container.set_margin_bottom(10);
    container.set_margin_start(10);
    container.set_margin_end(10);

    let label = Label::new(None);
    label.set_halign(gtk::Align::Start);
    label.set_line_wrap(true);
    label.set_max_width_chars(50);
    container.pack_start(&label, false, false, 0);

    let button_row = GtkBox::new(Orientation::Horizontal, 8);
    let button = Button::with_label("Replace");
    button.set_can_focus(false);
    button.connect_clicked(|_| {
        if let Some(proxy) = PROXY.get() {
            if let Err(error) = proxy.send_event(UserEvent::ReplaceClicked) {
                eprintln!("Writely: could not deliver the replace event: {error}");
            }
        }
    });
    button_row.pack_end(&button, false, false, 0);
    container.pack_start(&button_row, false, false, 0);

    window.connect_destroy(|_| {
        STATE.with(|state| {
            *state.borrow_mut() = None;
        });
    });

    window.add(&container);
    STATE.with(|state| {
        *state.borrow_mut() = Some(PopupState {
            window: window.clone(),
            label: label.clone(),
            button: button.clone(),
        });
    });
    Some((window, label, button))
}

pub fn show_message(text: &str, is_error: bool, show_button: bool, caret: &CaretRect) {
    if crate::text_access::uses_ime_bridge() {
        // No GTK window in bridge mode: the input method shows the text.
        // `show_suggestion` reaches here via `popup::show_message`, so it
        // emits ShowMessage(suggestion, false, true).
        let Some((sender, context, _)) = crate::ime_bridge::focused_snapshot() else {
            hide();
            return;
        };
        let _ = caret;
        crate::ime_bridge::emit_show_message(&sender, &context, text, is_error, show_button);
        SHOWN_ON.with(|shown| *shown.borrow_mut() = Some((sender, context)));
        return;
    }
    let Some((window, label, button)) = ensure_window() else {
        return;
    };
    // The popup and its widgets are kept alive and reused; never re-created.
    if is_error {
        label.set_markup(&format!(
            "<span foreground=\"red\">{}</span>",
            escape_markup(text)
        ));
    } else {
        label.set_text(text);
    }
    if show_button {
        button.show();
    } else {
        button.hide();
    }
    // 4 pt below the icon; the icon sits 4 pt right of the caret.
    let x = (caret.x + caret.width + 4.0) as i32;
    let icon_top = caret.y + caret.height / 2.0 - 10.0;
    let y = (icon_top + 20.0 + 4.0) as i32;
    window.move_(x, y);
    // The Popup window never takes focus from the user's text field.
    window.show_all();
    if !show_button {
        button.hide();
    }
}

pub fn hide() {
    if crate::text_access::uses_ime_bridge() {
        SHOWN_ON.with(|shown| {
            if let Some((sender, context)) = shown.borrow_mut().take() {
                crate::ime_bridge::emit_hide(&sender, &context);
            }
        });
        return;
    }
    STATE.with(|state| {
        if let Some(popup) = state.borrow().as_ref() {
            popup.window.hide();
        }
    });
}

pub fn is_visible() -> bool {
    if crate::text_access::uses_ime_bridge() {
        return SHOWN_ON.with(|shown| shown.borrow().is_some());
    }
    STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|popup| popup.window.is_visible())
            .unwrap_or(false)
    })
}
