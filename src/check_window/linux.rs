use std::cell::RefCell;
use std::sync::OnceLock;

use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use gtk::{
    Box as GtkBox, Button, Label, Orientation, PolicyType, ScrolledWindow, TextBuffer, TextTag,
    TextView, Window, WindowType, WrapMode,
};
use tao::event_loop::EventLoopProxy;

use crate::UserEvent;

static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();

struct CheckState {
    window: Window,
    input: TextView,
    result: TextView,
    bold_tag: TextTag,
    check_button: Button,
    copy_button: Button,
    status: Label,
    generation: u64,
    original: String,
    suggestion: Option<String>,
}

thread_local! {
    static STATE: RefCell<Option<CheckState>> = const { RefCell::new(None) };
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

/// Runs `f` on the open window's state, if the window is still open.
fn with_state(f: impl FnOnce(&CheckState)) {
    STATE.with(|state| {
        if let Some(st) = state.borrow().as_ref() {
            f(st);
        }
    });
}

fn input_text(state: &CheckState) -> Option<String> {
    let buffer = state.input.buffer()?;
    let (start, end) = (buffer.start_iter(), buffer.end_iter());
    Some(buffer.text(&start, &end, false)?.to_string())
}

/// Shows `suggestion` with inserted words in bold. Deletions from the
/// original are not part of the suggestion and are skipped.
fn render_suggestion(buffer: &TextBuffer, bold: &TextTag, original: &str, suggestion: &str) {
    buffer.set_text("");
    let mut end = buffer.end_iter();
    for op in crate::diff::diff_words(original, suggestion) {
        match op {
            crate::diff::DiffOp::Equal(text) => buffer.insert(&mut end, &text),
            crate::diff::DiffOp::Insert(text) => {
                let start = end;
                buffer.insert(&mut end, &text);
                buffer.apply_tag(bold, &start, &end);
            }
            crate::diff::DiffOp::Delete(_) => {}
        }
    }
}

/// Validates the input, loads the settings and key like the click flow, and
/// runs the provider call in a worker thread. Never logs the key or the text.
fn on_check() {
    let Some(raw) = STATE.with(|state| state.borrow().as_ref().and_then(input_text)) else {
        return;
    };
    let trimmed = match crate::check_text::validate_input(&raw) {
        Ok(trimmed) => trimmed,
        Err(message) => {
            with_state(|st| set_status(&st.status, &message, true));
            return;
        }
    };
    let app_settings = match crate::config::load_settings() {
        Ok(app_settings) => app_settings,
        Err(error) => {
            with_state(|st| set_status(&st.status, &error, true));
            return;
        }
    };
    if app_settings.current_model().is_empty() {
        with_state(|st| {
            set_status(
                &st.status,
                "Choose a model first: click the Writely menu-bar icon › Settings…",
                true,
            );
        });
        return;
    }
    // Ollama runs locally and needs no key; the other providers read the
    // selected provider's key from the keychain (never shown or logged).
    let key: Option<String> = match app_settings.provider {
        crate::config::Provider::Ollama => None,
        provider => match crate::settings::load_api_key_for(provider) {
            Ok(key) => key,
            Err(error) => {
                with_state(|st| set_status(&st.status, &error, true));
                return;
            }
        },
    };
    if key.is_none() && app_settings.provider != crate::config::Provider::Ollama {
        with_state(|st| {
            set_status(
                &st.status,
                &format!(
                    "Add your {} API key first: click the Writely menu-bar icon › Settings…",
                    app_settings.provider.display_name()
                ),
                true,
            );
        });
        return;
    }
    let Some(worker_proxy) = PROXY.get().cloned() else {
        with_state(|st| {
            set_status(
                &st.status,
                "Could not check the text: the app event channel is not ready.",
                true,
            );
        });
        return;
    };
    // A new check invalidates any in-flight request; stale replies are dropped.
    let my_generation = STATE.with(|state| {
        let mut borrowed = state.borrow_mut();
        let st = borrowed.as_mut()?;
        st.generation = st.generation.wrapping_add(1);
        st.original = trimmed.clone();
        st.suggestion = None;
        st.check_button.set_sensitive(false);
        st.copy_button.set_sensitive(false);
        if let Some(buffer) = st.result.buffer() {
            buffer.set_text("");
        }
        set_status(&st.status, "Checking…", false);
        Some(st.generation)
    });
    let Some(my_generation) = my_generation else {
        return;
    };
    if let Err(error) = std::thread::Builder::new()
        .name("writely-suggest".to_string())
        .spawn(move || {
            // The key lives only in this thread's request; it is never
            // logged or put in an error.
            let result = crate::providers::suggest(&app_settings, key.as_deref(), &trimmed);
            if let Err(error) = worker_proxy.send_event(UserEvent::TextCheckResult {
                generation: my_generation,
                result,
            }) {
                eprintln!("Writely: could not deliver the text check result: {error}");
            }
        })
    {
        with_state(|st| {
            st.check_button.set_sensitive(true);
            set_status(
                &st.status,
                &format!("Could not check the text: {error}"),
                true,
            );
        });
    }
}

/// Clears the input, result and status. An in-flight reply becomes stale and
/// is dropped when it arrives.
fn on_clear() {
    STATE.with(|state| {
        if let Some(st) = state.borrow_mut().as_mut() {
            st.generation = st.generation.wrapping_add(1);
            st.original.clear();
            st.suggestion = None;
            if let Some(buffer) = st.input.buffer() {
                buffer.set_text("");
            }
            if let Some(buffer) = st.result.buffer() {
                buffer.set_text("");
            }
            st.check_button.set_sensitive(true);
            st.copy_button.set_sensitive(false);
            set_status(&st.status, "", false);
            st.input.grab_focus();
        }
    });
}

fn on_copy() {
    with_state(|st| {
        let Some(suggestion) = st.suggestion.clone() else {
            return;
        };
        let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
        clipboard.set_text(&suggestion);
        set_status(&st.status, "Copied. Paste it where you need it.", false);
    });
}

pub fn handle_result(generation: u64, result: Result<String, String>) {
    STATE.with(|state| {
        let mut borrowed = state.borrow_mut();
        // Drop late replies: the window was closed, or a newer Check or Clear
        // happened.
        let Some(st) = borrowed.as_mut() else {
            return;
        };
        if generation != st.generation {
            return;
        }
        st.check_button.set_sensitive(true);
        match result {
            Err(error) => set_status(&st.status, &error, true),
            Ok(suggestion) => {
                match crate::check_text::outcome_for_suggestion(suggestion, &st.original) {
                    crate::check_text::CheckOutcome::Suggestion(text) => {
                        if let Some(buffer) = st.result.buffer() {
                            render_suggestion(&buffer, &st.bold_tag, &st.original, &text);
                        }
                        st.suggestion = Some(text);
                        st.copy_button.set_sensitive(true);
                        set_status(&st.status, "", false);
                    }
                    outcome => {
                        if let Some(buffer) = st.result.buffer() {
                            buffer.set_text("");
                        }
                        st.suggestion = None;
                        st.copy_button.set_sensitive(false);
                        if let Some(message) = crate::check_text::outcome_message(&outcome) {
                            set_status(&st.status, message, false);
                        }
                    }
                }
            }
        }
    });
}

pub fn show() -> Result<(), String> {
    if let Err(error) = gtk::init() {
        // GTK may already be initialised by the tray/event-loop stack; only
        // fail when there is no usable display at all.
        eprintln!("Writely: GTK initialisation note on Linux: {error}");
        if !gtk::is_initialized() {
            return Err(format!("Could not open the Check text window: {error}"));
        }
    }

    let already_open = STATE.with(|state| state.borrow().is_some());
    if already_open {
        STATE.with(|state| {
            if let Some(st) = state.borrow().as_ref() {
                st.window.present();
                st.input.grab_focus();
            }
        });
        return Ok(());
    }

    let window = Window::new(WindowType::Toplevel);
    window.set_title("Writely — Check text");
    window.set_default_size(560, 520);
    window.set_resizable(true);

    let container = GtkBox::new(Orientation::Vertical, 8);
    container.set_margin_top(16);
    container.set_margin_bottom(16);
    container.set_margin_start(20);
    container.set_margin_end(20);

    let input_label = Label::new(Some("Paste or type the text you want to check:"));
    input_label.set_halign(gtk::Align::Start);
    container.pack_start(&input_label, false, false, 0);

    let input_view = TextView::new();
    input_view.set_wrap_mode(WrapMode::Word);
    input_view.set_editable(true);
    let input_scroll = ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    input_scroll.set_policy(PolicyType::Automatic, PolicyType::Automatic);
    input_scroll.set_min_content_height(120);
    input_scroll.add(&input_view);
    container.pack_start(&input_scroll, true, true, 0);

    let button_row = GtkBox::new(Orientation::Horizontal, 8);
    let check_button = Button::with_label("Check");
    check_button.set_tooltip_text(Some("Check the text above (Ctrl+Enter)"));
    let clear_button = Button::with_label("Clear");
    button_row.pack_start(&check_button, false, false, 0);
    button_row.pack_start(&clear_button, false, false, 0);
    container.pack_start(&button_row, false, false, 0);

    let status_label = Label::new(None);
    status_label.set_halign(gtk::Align::Start);
    status_label.set_line_wrap(true);
    container.pack_start(&status_label, false, false, 0);

    let result_label = Label::new(Some("Suggestion:"));
    result_label.set_halign(gtk::Align::Start);
    container.pack_start(&result_label, false, false, 0);

    let result_view = TextView::new();
    result_view.set_wrap_mode(WrapMode::Word);
    result_view.set_editable(false);
    let result_scroll = ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    result_scroll.set_policy(PolicyType::Automatic, PolicyType::Automatic);
    result_scroll.set_min_content_height(120);
    result_scroll.add(&result_view);
    container.pack_start(&result_scroll, true, true, 0);

    let copy_row = GtkBox::new(Orientation::Horizontal, 8);
    let copy_button = Button::with_label("Copy suggestion");
    copy_button.set_sensitive(false);
    copy_row.pack_start(&copy_button, false, false, 0);
    container.pack_start(&copy_row, false, false, 0);

    // Bold for the inserted words in the suggestion view.
    let bold_tag = TextTag::new(Some("writely-bold"));
    bold_tag.set_weight(gtk::pango::Weight::Bold.into_glib());
    if let Some(buffer) = result_view.buffer() {
        if let Some(table) = buffer.tag_table() {
            table.add(&bold_tag);
        }
    }

    check_button.connect_clicked(|_| on_check());
    clear_button.connect_clicked(|_| on_clear());
    copy_button.connect_clicked(|_| on_copy());
    input_view.connect_key_press_event(|_, event| {
        use gtk::gdk::keys::constants::{KP_Enter, Return};
        let key = event.keyval();
        if (key == Return || key == KP_Enter)
            && event.state().contains(gtk::gdk::ModifierType::CONTROL_MASK)
        {
            on_check();
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    window.connect_key_press_event(|window, event| {
        if event.keyval() == gtk::gdk::keys::constants::Escape {
            window.close();
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    window.connect_destroy(|_| {
        STATE.with(|state| {
            *state.borrow_mut() = None;
        });
    });

    window.add(&container);
    STATE.with(|state| {
        *state.borrow_mut() = Some(CheckState {
            window: window.clone(),
            input: input_view.clone(),
            result: result_view.clone(),
            bold_tag,
            check_button,
            copy_button,
            status: status_label,
            generation: 0,
            original: String::new(),
            suggestion: None,
        });
    });
    window.show_all();
    window.present();
    input_view.grab_focus();
    Ok(())
}
