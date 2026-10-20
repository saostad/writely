use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{
    Box as GtkBox, Button, CheckButton, ComboBoxText, Entry, Label, LinkButton, Orientation,
    Window, WindowType,
};

use crate::config::{AppSettings, Effort, Provider};

const PROVIDER_NAMES: [&str; 3] = [
    "Google Gemini",
    "Together AI",
    "Ollama (local, no key needed)",
];
const EFFORT_NAMES: [&str; 5] = ["Default", "Minimal (fastest)", "Low", "Medium", "High"];

fn provider_at(index: u32) -> Provider {
    match index {
        1 => Provider::Together,
        2 => Provider::Ollama,
        _ => Provider::Gemini,
    }
}

fn provider_index(provider: Provider) -> u32 {
    match provider {
        Provider::Gemini => 0,
        Provider::Together => 1,
        Provider::Ollama => 2,
    }
}

fn effort_at(index: u32) -> Effort {
    match index {
        1 => Effort::Minimal,
        2 => Effort::Low,
        3 => Effort::Medium,
        4 => Effort::High,
        _ => Effort::Default,
    }
}

fn effort_index(effort: Effort) -> u32 {
    match effort {
        Effort::Default => 0,
        Effort::Minimal => 1,
        Effort::Low => 2,
        Effort::Medium => 3,
        Effort::High => 4,
    }
}

fn model_placeholder_for(provider: Provider) -> &'static str {
    match provider {
        Provider::Gemini => "gemini-3.5-flash-lite",
        Provider::Together => "deepseek-ai/DeepSeek-V4.1-Flash",
        Provider::Ollama => "llama3.2",
    }
}

fn key_title_for(provider: Provider) -> &'static str {
    match provider {
        Provider::Gemini => "Google Gemini API key",
        Provider::Together => "Together AI API key",
        Provider::Ollama => "Ollama address",
    }
}

fn key_help_for(provider: Provider) -> &'static str {
    match provider {
        Provider::Gemini => "Writely sends the sentence you're editing to Google Gemini to check its grammar. Get a free key at ",
        Provider::Together => "Writely sends the sentence you're editing to Together AI to check its grammar. Get a key at ",
        Provider::Ollama => "Where Ollama runs on your computer. No key needed.",
    }
}

struct OpenWindow {
    window: Window,
    controls: Rc<RefCell<Controls>>,
}

thread_local! {
    static STATE: RefCell<Option<OpenWindow>> = const { RefCell::new(None) };
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

struct Controls {
    provider_combo: ComboBoxText,
    model_entry: Entry,
    effort_combo: ComboBoxText,
    key_entry: Entry,
    url_entry: Entry,
    key_label: Label,
    key_help: Label,
    link_box: GtkBox,
    url_label: Label,
    url_help: Label,
    autostart_check: CheckButton,
    status_label: Label,
    settings: AppSettings,
    draft_keys: [String; 3],
    suppress_provider_changed: bool,
}

/// Refreshes the provider-dependent rows for the selected provider: the model
/// text and placeholder, the key placeholder (•••• when a key is saved), the
/// key page link, and key vs. Ollama-address visibility.
fn refresh_provider_rows(controls: &Rc<RefCell<Controls>>) {
    let borrowed = controls.borrow();
    let provider = provider_at(borrowed.provider_combo.active().unwrap_or(0));
    let model = borrowed.settings.model_for(provider);
    let ollama_url = borrowed.settings.ollama_url.clone();
    drop(borrowed);
    let borrowed = controls.borrow();
    borrowed.model_entry.set_text(&model);
    borrowed
        .model_entry
        .set_placeholder_text(Some(model_placeholder_for(provider)));
    let is_ollama = provider == Provider::Ollama;
    borrowed.key_label.set_text(key_title_for(provider));
    borrowed.key_help.set_text(key_help_for(provider));
    borrowed.key_label.set_visible(!is_ollama);
    borrowed.key_entry.set_visible(!is_ollama);
    borrowed.key_help.set_visible(!is_ollama);
    borrowed.link_box.set_visible(!is_ollama);
    borrowed.url_label.set_visible(is_ollama);
    borrowed.url_entry.set_visible(is_ollama);
    borrowed.url_help.set_visible(is_ollama);
    // The link follows the provider: rebuild it for the current key page.
    for child in borrowed.link_box.children() {
        borrowed.link_box.remove(&child);
    }
    if let Some(page) = super::api_key_page_for(provider) {
        let link = LinkButton::with_label(page, page);
        link.set_halign(gtk::Align::Start);
        borrowed.link_box.pack_start(&link, false, false, 0);
        link.show();
    }
    if borrowed.url_entry.text().to_string().is_empty() {
        borrowed.url_entry.set_text(&ollama_url);
    }
    borrowed
        .url_entry
        .set_placeholder_text(Some(crate::config::DEFAULT_OLLAMA_URL));
    if !is_ollama {
        match super::has_api_key_for(provider) {
            Ok(true) => borrowed.key_entry.set_placeholder_text(Some("••••••••")),
            Ok(false) => borrowed
                .key_entry
                .set_placeholder_text(Some("Paste your key here")),
            Err(error) => set_status(&borrowed.status_label, &error, true),
        }
    }
}

/// Stores the model/effort/URL values into the draft settings.
fn stash_draft(controls: &Rc<RefCell<Controls>>) {
    let borrowed = controls.borrow();
    let provider = provider_at(borrowed.provider_combo.active().unwrap_or(0));
    let model = borrowed.model_entry.text().to_string();
    let effort = effort_at(borrowed.effort_combo.active().unwrap_or(0));
    let url = borrowed.url_entry.text().to_string();
    drop(borrowed);
    let mut borrowed = controls.borrow_mut();
    // The model entry still shows the draft provider's model: on a provider
    // change the combo already has the new selection.
    let shown = borrowed.settings.provider;
    borrowed.settings.set_model(shown, model.trim());
    borrowed.settings.provider = provider;
    borrowed.settings.effort = effort;
    borrowed.settings.launch_at_login = borrowed.autostart_check.is_active();
    if !url.trim().is_empty() {
        borrowed.settings.ollama_url = url.trim().to_string();
    }
}

/// Applies a provider change: files the shown key text, stashes the draft,
/// selects `new_provider`, and shows its draft. The dropdown handler and a
/// tray switch both call this.
fn apply_provider_change(controls: &Rc<RefCell<Controls>>, new_provider: Provider) {
    // File the key box text under the shown provider first (before stash_draft
    // moves settings.provider), so a typed key stays with its provider.
    {
        let borrowed = controls.borrow();
        let shown = borrowed.settings.provider;
        let text = borrowed.key_entry.text().to_string();
        drop(borrowed);
        let mut borrowed = controls.borrow_mut();
        super::file_key_draft(&mut borrowed.draft_keys, shown, &text);
    }
    stash_draft(controls);
    // Select without re-emitting `changed`. GTK emits `changed`
    // synchronously from `set_active` and the handler borrows `controls`,
    // so no borrow may be held across the call.
    {
        let mut borrowed = controls.borrow_mut();
        borrowed.suppress_provider_changed = true;
    }
    let combo = {
        let borrowed = controls.borrow();
        borrowed.provider_combo.clone()
    };
    combo.set_active(Some(provider_index(new_provider)));
    {
        let mut borrowed = controls.borrow_mut();
        borrowed.settings.provider = new_provider;
        borrowed.suppress_provider_changed = false;
    }
    {
        let borrowed = controls.borrow();
        let draft = super::restore_key_draft(&borrowed.draft_keys, new_provider).to_string();
        borrowed.key_entry.set_text(&draft);
    }
    refresh_provider_rows(controls);
}

/// Follows a tray switch in an open Settings window: the same steps as a
/// manual dropdown change. Returns straight away when Settings is not open.
pub fn provider_switched(provider: Provider) {
    let controls = STATE.with(|state| state.borrow().as_ref().map(|open| open.controls.clone()));
    let Some(controls) = controls else {
        return;
    };
    apply_provider_change(&controls, provider);
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
            if let Some(open) = state.borrow().as_ref() {
                if let Some(message) = super::provider_message() {
                    let borrowed = open.controls.borrow();
                    set_status(&borrowed.status_label, &message, true);
                    super::set_provider_message(None);
                }
                open.window.present();
            }
        });
        return Ok(());
    }

    // Load the draft: a missing file means defaults; an unreadable or invalid
    // file shows its error and starts from defaults.
    let mut initial_error: Option<String> = None;
    let settings = match crate::config::load_settings() {
        Ok(settings) => settings,
        Err(error) => {
            initial_error = Some(error);
            AppSettings::default()
        }
    };

    let window = Window::new(WindowType::Toplevel);
    window.set_title("Writely Settings");
    window.set_default_size(420, 620);
    window.set_resizable(false);

    let container = GtkBox::new(Orientation::Vertical, 8);
    container.set_margin_top(16);
    container.set_margin_bottom(16);
    container.set_margin_start(20);
    container.set_margin_end(20);

    let provider_label = Label::new(Some("Provider"));
    provider_label.set_halign(gtk::Align::Start);
    container.pack_start(&provider_label, false, false, 0);

    let provider_combo = ComboBoxText::new();
    for name in PROVIDER_NAMES {
        provider_combo.append_text(name);
    }
    provider_combo.set_active(Some(provider_index(settings.provider)));
    container.pack_start(&provider_combo, false, false, 0);

    let provider_help = Label::new(Some("Who checks your grammar."));
    provider_help.set_halign(gtk::Align::Start);
    container.pack_start(&provider_help, false, false, 0);

    let model_label = Label::new(Some("Model"));
    model_label.set_halign(gtk::Align::Start);
    container.pack_start(&model_label, false, false, 0);

    let model_entry = Entry::new();
    model_entry.set_text(&settings.model_for(settings.provider));
    model_entry.set_placeholder_text(Some(model_placeholder_for(settings.provider)));
    container.pack_start(&model_entry, false, false, 0);

    let model_help = Label::new(Some(
        "Paste the model name, e.g. gemini-3.5-flash-lite / deepseek-ai/DeepSeek-V4.1-Flash / llama3.2",
    ));
    model_help.set_halign(gtk::Align::Start);
    model_help.set_line_wrap(true);
    container.pack_start(&model_help, false, false, 0);

    let effort_label = Label::new(Some("Effort"));
    effort_label.set_halign(gtk::Align::Start);
    container.pack_start(&effort_label, false, false, 0);

    let effort_combo = ComboBoxText::new();
    for name in EFFORT_NAMES {
        effort_combo.append_text(name);
    }
    effort_combo.set_active(Some(effort_index(settings.effort)));
    container.pack_start(&effort_combo, false, false, 0);

    let effort_help = Label::new(Some(
        "How hard the model thinks. Not every model supports every level; you'll see the model's error if not.",
    ));
    effort_help.set_halign(gtk::Align::Start);
    effort_help.set_line_wrap(true);
    container.pack_start(&effort_help, false, false, 0);

    // The key rows and the Ollama-address rows share the same place; only one
    // set is visible at a time.
    let key_label = Label::new(Some(key_title_for(settings.provider)));
    key_label.set_halign(gtk::Align::Start);
    container.pack_start(&key_label, false, false, 0);

    let key_entry = Entry::new();
    key_entry.set_visibility(false);
    match super::has_api_key_for(settings.provider) {
        Ok(true) => key_entry.set_placeholder_text(Some("••••••••")),
        Ok(false) => key_entry.set_placeholder_text(Some("Paste your key here")),
        Err(error) => {
            key_entry.set_placeholder_text(Some("Paste your key here"));
            if initial_error.is_none() {
                initial_error = Some(error);
            }
        }
    }
    container.pack_start(&key_entry, false, false, 0);

    let key_help = Label::new(Some(key_help_for(settings.provider)));
    key_help.set_halign(gtk::Align::Start);
    key_help.set_line_wrap(true);
    container.pack_start(&key_help, false, false, 0);

    let link_box = GtkBox::new(Orientation::Vertical, 0);
    container.pack_start(&link_box, false, false, 0);

    let url_label = Label::new(Some("Ollama address"));
    url_label.set_halign(gtk::Align::Start);
    container.pack_start(&url_label, false, false, 0);

    let url_entry = Entry::new();
    url_entry.set_text(&settings.ollama_url);
    url_entry.set_placeholder_text(Some(crate::config::DEFAULT_OLLAMA_URL));
    container.pack_start(&url_entry, false, false, 0);

    let url_help = Label::new(Some("Where Ollama runs on your computer. No key needed."));
    url_help.set_halign(gtk::Align::Start);
    url_help.set_line_wrap(true);
    container.pack_start(&url_help, false, false, 0);

    let autostart_check = CheckButton::with_label("Start Writely at login");
    autostart_check.set_active(settings.launch_at_login);
    autostart_check.set_halign(gtk::Align::Start);
    container.pack_start(&autostart_check, false, false, 0);

    let autostart_help = Label::new(Some("Writely starts automatically when you log in."));
    autostart_help.set_halign(gtk::Align::Start);
    autostart_help.set_line_wrap(true);
    container.pack_start(&autostart_help, false, false, 0);

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
    } else if let Some(message) = super::provider_message() {
        set_status(&status_label, &message, true);
        super::set_provider_message(None);
    } else if let Some(error) = super::autostart_message() {
        set_status(&status_label, &error, true);
    }

    let controls = Rc::new(RefCell::new(Controls {
        provider_combo: provider_combo.clone(),
        model_entry: model_entry.clone(),
        effort_combo: effort_combo.clone(),
        key_entry: key_entry.clone(),
        url_entry: url_entry.clone(),
        key_label: key_label.clone(),
        key_help: key_help.clone(),
        link_box: link_box.clone(),
        url_label: url_label.clone(),
        url_help: url_help.clone(),
        autostart_check: autostart_check.clone(),
        status_label: status_label.clone(),
        settings,
        draft_keys: [String::new(), String::new(), String::new()],
        suppress_provider_changed: false,
    }));
    refresh_provider_rows(&controls);

    {
        let controls_for_provider = controls.clone();
        provider_combo.connect_changed(move |_| {
            if controls_for_provider.borrow().suppress_provider_changed {
                return;
            }
            let new_provider = {
                let borrowed = controls_for_provider.borrow();
                provider_at(borrowed.provider_combo.active().unwrap_or(0))
            };
            apply_provider_change(&controls_for_provider, new_provider);
        });
    }
    {
        let controls_for_save = controls.clone();
        save_button.connect_clicked(move |_| {
            let snapshot = {
                let borrowed = controls_for_save.borrow();
                (
                    provider_at(borrowed.provider_combo.active().unwrap_or(0)),
                    borrowed.model_entry.text().to_string(),
                    effort_at(borrowed.effort_combo.active().unwrap_or(0)),
                    borrowed.url_entry.text().to_string(),
                    borrowed.key_entry.text().to_string(),
                )
            };
            let (provider, model, effort, url, key) = snapshot;
            if let Err(error) = crate::config::validate_model_name(&model) {
                let borrowed = controls_for_save.borrow();
                set_status(&borrowed.status_label, &error, true);
                return;
            }
            stash_draft(&controls_for_save);
            let save_result = {
                let borrowed = controls_for_save.borrow();
                crate::config::save_settings(&borrowed.settings)
            };
            if let Err(error) = save_result {
                let borrowed = controls_for_save.borrow();
                set_status(&borrowed.status_label, &error, true);
                return;
            }
            // settings.json is written: tell the tray once, even if a key save
            // fails below.
            super::notify_changed();
            let launch_at_login = {
                let borrowed = controls_for_save.borrow();
                borrowed.settings.launch_at_login
            };
            if let Err(error) = crate::autostart::set_enabled(launch_at_login) {
                let borrowed = controls_for_save.borrow();
                set_status(&borrowed.status_label, &error, true);
                return;
            }
            super::set_autostart_message(None);
            let _ = (effort, url);
            // File the box text under the current provider, then save every
            // non-empty draft under its own provider. On the first failure the
            // remaining drafts are kept, so nothing is lost.
            {
                let mut borrowed = controls_for_save.borrow_mut();
                super::file_key_draft(&mut borrowed.draft_keys, provider, &key);
            }
            let pending = {
                let borrowed = controls_for_save.borrow();
                super::pending_key_drafts(&borrowed.draft_keys)
            };
            for (draft_provider, draft_key) in &pending {
                if let Err(error) = super::save_api_key_for(*draft_provider, draft_key) {
                    let borrowed = controls_for_save.borrow();
                    set_status(
                        &borrowed.status_label,
                        &format!(
                            "Could not save the {} key: {error}",
                            draft_provider.display_name()
                        ),
                        true,
                    );
                    return;
                }
                let mut borrowed = controls_for_save.borrow_mut();
                super::clear_key_draft(&mut borrowed.draft_keys, *draft_provider);
            }
            {
                let borrowed = controls_for_save.borrow();
                set_status(&borrowed.status_label, "Saved.", false);
            }
            let borrowed = controls_for_save.borrow();
            borrowed.key_entry.set_text("");
            match super::has_api_key_for(provider) {
                Ok(true) => borrowed.key_entry.set_placeholder_text(Some("••••••••")),
                Ok(false) => borrowed
                    .key_entry
                    .set_placeholder_text(Some("Paste your key here")),
                Err(error) => set_status(&borrowed.status_label, &error, true),
            }
            drop(borrowed);
        });
    }
    {
        let controls_for_remove = controls.clone();
        remove_button.connect_clicked(move |_| {
            let provider = {
                let borrowed = controls_for_remove.borrow();
                provider_at(borrowed.provider_combo.active().unwrap_or(0))
            };
            let mut borrowed = controls_for_remove.borrow_mut();
            match super::remove_api_key_for(provider) {
                Ok(()) => {
                    super::clear_key_draft(&mut borrowed.draft_keys, provider);
                    borrowed.key_entry.set_text("");
                    borrowed
                        .key_entry
                        .set_placeholder_text(Some("Paste your key here"));
                    set_status(&borrowed.status_label, "Key removed.", false);
                    drop(borrowed);
                    super::notify_changed();
                }
                Err(error) => set_status(&borrowed.status_label, &error, true),
            }
        });
    }

    window.connect_destroy(|_| {
        STATE.with(|state| {
            *state.borrow_mut() = None;
        });
    });

    window.add(&container);
    STATE.with(|state| {
        *state.borrow_mut() = Some(OpenWindow {
            window: window.clone(),
            controls: controls.clone(),
        });
    });
    window.show_all();
    // refresh_provider_rows ran before show_all; re-apply visibility now that
    // everything is realized.
    refresh_provider_rows(&controls);
    window.present();
    Ok(())
}
