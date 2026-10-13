use std::cell::RefCell;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, Entry, Label, LinkButton, Orientation, Window, WindowType};

thread_local! {
    static STATE: RefCell<Option<Window>> = const { RefCell::new(None) };
}

fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn set_status(status: &Label, text: &str, is_error: bool) {
    if is_error {
        status.set_markup(&format!(
            "<span foreground=\"red\">{}</span>",
            escape_markup(text)
        ));
    } else {
        status.set_text(text);
    }
}

pub fn show_settings() -> Result<(), String> {
    if let Err(error) = gtk::init() {
        // GTK may already be initialised by the tray/event-loop stack; only
        // fail when there is no usable display at all.
        eprintln!("Writely: GTK initialisation note on Linux: {error}");
        if !gtk::is_initialized() {
            return Err(format!("Could not open the settings window: {error}"));
        }
    }

    let already_open = STATE.with(|state| state.borrow().is_some());
    if already_open {
        STATE.with(|state| {
            if let Some(window) = state.borrow().as_ref() {
                window.present();
            }
        });
        return Ok(());
    }

    let window = Window::new(WindowType::Toplevel);
    window.set_title("Writely Settings");
    window.set_default_size(420, 200);
    window.set_resizable(false);

    let container = GtkBox::new(Orientation::Vertical, 8);
    container.set_margin_top(16);
    container.set_margin_bottom(16);
    container.set_margin_start(20);
    container.set_margin_end(20);

    let title_label = Label::new(Some("Google Gemini API key"));
    title_label.set_halign(gtk::Align::Start);
    container.pack_start(&title_label, false, false, 0);

    let entry = Entry::new();
    entry.set_visibility(false);
    let mut initial_error: Option<String> = None;
    match super::has_api_key() {
        Ok(true) => entry.set_placeholder_text(Some("••••••••")),
        Ok(false) => entry.set_placeholder_text(Some("Paste your key here")),
        Err(error) => {
            entry.set_placeholder_text(Some("Paste your key here"));
            initial_error = Some(error);
        }
    }
    container.pack_start(&entry, false, false, 0);

    let help_label = Label::new(Some(
        "Writely sends the sentence you're editing to Google Gemini to check its grammar. Get a free key at ",
    ));
    help_label.set_halign(gtk::Align::Start);
    help_label.set_line_wrap(true);
    container.pack_start(&help_label, false, false, 0);

    let link = LinkButton::with_label(super::API_KEY_PAGE, super::API_KEY_PAGE);
    link.set_halign(gtk::Align::Start);
    container.pack_start(&link, false, false, 0);

    let button_row = GtkBox::new(Orientation::Horizontal, 8);
    let save_button = Button::with_label("Save");
    let remove_button = Button::with_label("Remove key");
    button_row.pack_start(&save_button, false, false, 0);
    button_row.pack_start(&remove_button, false, false, 0);
    container.pack_start(&button_row, false, false, 0);

    let status_label = Label::new(None);
    status_label.set_halign(gtk::Align::Start);
    container.pack_start(&status_label, false, false, 0);
    if let Some(error) = &initial_error {
        set_status(&status_label, error, true);
    }

    {
        let entry_for_save = entry.clone();
        let status_for_save = status_label.clone();
        save_button.connect_clicked(move |_| {
            let value = entry_for_save.text().to_string();
            match super::save_api_key(&value) {
                Ok(()) => {
                    entry_for_save.set_text("");
                    entry_for_save.set_placeholder_text(Some("••••••••"));
                    set_status(&status_for_save, "Saved.", false);
                }
                Err(error) => set_status(&status_for_save, &error, true),
            }
        });
    }
    {
        let entry_for_remove = entry.clone();
        let status_for_remove = status_label.clone();
        remove_button.connect_clicked(move |_| match super::remove_api_key() {
            Ok(()) => {
                entry_for_remove.set_text("");
                entry_for_remove.set_placeholder_text(Some("Paste your key here"));
                set_status(&status_for_remove, "Key removed.", false);
            }
            Err(error) => set_status(&status_for_remove, &error, true),
        });
    }

    window.connect_destroy(|_| {
        STATE.with(|state| {
            *state.borrow_mut() = None;
        });
    });

    window.add(&container);
    STATE.with(|state| {
        *state.borrow_mut() = Some(window.clone());
    });
    window.show_all();
    window.present();
    Ok(())
}
