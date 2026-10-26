use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{
    Box as GtkBox, Button, ButtonsType, CheckButton, ComboBoxText, DialogFlags, Entry, Expander,
    Label, LinkButton, MessageDialog, MessageType, Orientation, PolicyType, ResponseType,
    ScrolledWindow, Window, WindowType,
};

use crate::config::{AppSettings, Effort, LocalModelId, Provider};
use crate::local::{catalog, download, install, server};

const PROVIDER_NAMES: [&str; 4] = [
    "Google Gemini",
    "Together AI",
    "Ollama (local, no key needed)",
    "On this computer",
];
const EFFORT_NAMES: [&str; 5] = ["Default", "Minimal (fastest)", "Low", "Medium", "High"];

fn provider_at(index: u32) -> Provider {
    match index {
        1 => Provider::Together,
        2 => Provider::Ollama,
        3 => Provider::Local,
        _ => Provider::Gemini,
    }
}

fn provider_index(provider: Provider) -> u32 {
    match provider {
        Provider::Gemini => 0,
        Provider::Together => 1,
        Provider::Ollama => 2,
        Provider::Local => 3,
    }
}

fn local_model_at(index: u32) -> LocalModelId {
    match index {
        1 => LocalModelId::Qwen35_2B,
        _ => LocalModelId::Qwen35_0_8B,
    }
}

fn local_model_index(model: LocalModelId) -> u32 {
    match model {
        LocalModelId::Qwen35_0_8B => 0,
        LocalModelId::Qwen35_2B => 1,
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
        Provider::Local => "",
    }
}

fn key_title_for(provider: Provider) -> &'static str {
    match provider {
        Provider::Gemini => "Google Gemini API key",
        Provider::Together => "Together AI API key",
        Provider::Ollama => "Ollama address",
        Provider::Local => "Ollama address",
    }
}

fn key_help_for(provider: Provider) -> &'static str {
    match provider {
        Provider::Gemini => "Writely sends the sentence you're editing to Google Gemini to check its grammar. Get a free key at ",
        Provider::Together => "Writely sends the sentence you're editing to Together AI to check its grammar. Get a key at ",
        Provider::Ollama => "Where Ollama runs on your computer. No key needed.",
        Provider::Local => "This runs a small AI model on your computer. Download and memory options are being added.",
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
    model_label: Label,
    model_entry: Entry,
    model_help: Label,
    local_box: GtkBox,
    local_model_combo: ComboBoxText,
    local_status_label: Label,
    local_download_button: Button,
    local_cancel_button: Button,
    local_delete_button: Button,
    local_warm_check: CheckButton,
    local_idle_entry: Entry,
    local_threads_entry: Entry,
    local_max_reply_entry: Entry,
    local_context_entry: Entry,
    local_startup_entry: Entry,
    local_reply_entry: Entry,
    local_runtime_error: bool,
    local_was_busy: bool,
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

/// Re-renders the local-model status, buttons and model-combo sensitivity
/// from the current install phase for the selected model.
fn render_local(controls: &Rc<RefCell<Controls>>) {
    let (selected, runtime_error) = {
        let borrowed = controls.borrow();
        (
            local_model_at(borrowed.local_model_combo.active().unwrap_or(0)),
            borrowed.local_runtime_error,
        )
    };
    let phase = install::install_phase();
    let ready = install::model_ready(selected);
    let view = super::local_view(&phase, ready, catalog::model_file(selected).label);
    let busy = matches!(
        phase,
        install::InstallPhase::Downloading { .. }
            | install::InstallPhase::Verifying { .. }
            | install::InstallPhase::Unpacking
    );
    let finished = {
        let mut borrowed = controls.borrow_mut();
        set_status(&borrowed.local_status_label, &view.status, view.is_error);
        borrowed
            .local_download_button
            .set_visible(view.show_download && !runtime_error);
        borrowed
            .local_cancel_button
            .set_visible(view.show_cancel && !runtime_error);
        borrowed
            .local_delete_button
            .set_visible(view.show_delete && !runtime_error);
        borrowed
            .local_model_combo
            .set_sensitive(view.fields_enabled);
        let finished =
            borrowed.local_was_busy && matches!(phase, install::InstallPhase::Idle) && ready;
        borrowed.local_was_busy = busy;
        finished
    };
    if finished {
        super::notify_changed();
    }
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
    let is_local = provider == Provider::Local;
    borrowed.key_label.set_text(key_title_for(provider));
    borrowed.key_help.set_text(key_help_for(provider));
    borrowed.model_label.set_visible(!is_local);
    borrowed.model_entry.set_visible(!is_local);
    borrowed.model_help.set_visible(!is_local);
    borrowed.local_box.set_visible(is_local);
    borrowed.key_label.set_visible(!is_ollama && !is_local);
    borrowed.key_entry.set_visible(!is_ollama && !is_local);
    borrowed.key_help.set_visible(!is_ollama && !is_local);
    borrowed.link_box.set_visible(!is_ollama && !is_local);
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
    if provider.needs_key() {
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

    // The "On this computer" section, shown only for that provider.
    let local_box = GtkBox::new(Orientation::Vertical, 8);
    fn local_label(text: &str, parent: &GtkBox) -> Label {
        let label = Label::new(Some(text));
        label.set_halign(gtk::Align::Start);
        label.set_line_wrap(true);
        parent.pack_start(&label, false, false, 0);
        label
    }
    fn local_entry(value: u32, parent: &GtkBox) -> Entry {
        let entry = Entry::new();
        entry.set_text(&value.to_string());
        entry.set_input_purpose(gtk::InputPurpose::Digits);
        parent.pack_start(&entry, false, false, 0);
        entry
    }
    local_label("Model", &local_box);
    let local_model_combo = ComboBoxText::new();
    for (_id, text) in super::local_model_choices() {
        local_model_combo.append_text(&text);
    }
    local_model_combo.set_active(Some(local_model_index(settings.local_model)));
    local_box.pack_start(&local_model_combo, false, false, 0);
    local_label(
        "Smaller is faster; larger makes fewer mistakes. Your text never leaves this computer.",
        &local_box,
    );
    let local_status_label = local_label("", &local_box);
    let local_button_row = GtkBox::new(Orientation::Horizontal, 8);
    let local_download_button = Button::with_label("Download");
    let local_cancel_button = Button::with_label("Cancel");
    let local_delete_button = Button::with_label("Delete model");
    local_button_row.pack_start(&local_download_button, false, false, 0);
    local_button_row.pack_start(&local_cancel_button, false, false, 0);
    local_button_row.pack_start(&local_delete_button, false, false, 0);
    local_box.pack_start(&local_button_row, false, false, 0);
    let (local_runtime_help_text, local_runtime_error) = match catalog::runtime_file() {
        Ok(runtime) => (
            format!(
                "The first download also gets the llama.cpp helper ({}). Files are saved in {}.",
                download::human_size(runtime.size),
                catalog::local_data_dir().display()
            ),
            false,
        ),
        Err(error) => (error, true),
    };
    local_label(&local_runtime_help_text, &local_box);
    let local_warm_check = CheckButton::with_label("Load the model when Writely starts");
    local_warm_check.set_active(settings.local_warm_up);
    local_warm_check.set_halign(gtk::Align::Start);
    local_box.pack_start(&local_warm_check, false, false, 0);
    local_label(
        "Makes the first check fast. While loaded it uses about 1 GB of memory (2 GB for the 2B model).",
        &local_box,
    );
    local_label("Free memory after (minutes)", &local_box);
    let local_idle_entry = local_entry(settings.local_idle_minutes, &local_box);
    local_label(
        "Unload the model after this many minutes without use. 0 = never. It loads again in about a second when needed.",
        &local_box,
    );
    let local_advanced = Expander::new(Some("Advanced"));
    local_advanced.set_expanded(false);
    let local_advanced_box = GtkBox::new(Orientation::Vertical, 8);
    local_label("Processor threads", &local_advanced_box);
    let local_threads_entry = local_entry(settings.local_threads, &local_advanced_box);
    local_label(
        "How many processor cores the model uses. 4 suits most computers.",
        &local_advanced_box,
    );
    local_label("Longest reply (tokens)", &local_advanced_box);
    let local_max_reply_entry = local_entry(settings.local_max_reply_tokens, &local_advanced_box);
    local_label(
        "Stops a reply that runs too long. A token is about 4 characters.",
        &local_advanced_box,
    );
    local_label("Context size (tokens)", &local_advanced_box);
    let local_context_entry = local_entry(settings.local_context_tokens, &local_advanced_box);
    local_label(
        "How much text the model reads at once. Larger uses more memory.",
        &local_advanced_box,
    );
    local_label("Startup time limit (seconds)", &local_advanced_box);
    let local_startup_entry = local_entry(settings.local_startup_timeout_secs, &local_advanced_box);
    local_label(
        "How long to wait for the model to load.",
        &local_advanced_box,
    );
    local_label("Reply time limit (seconds)", &local_advanced_box);
    let local_reply_entry = local_entry(settings.local_reply_timeout_secs, &local_advanced_box);
    local_label("How long to wait for a reply.", &local_advanced_box);
    local_advanced.add(&local_advanced_box);
    local_box.pack_start(&local_advanced, false, false, 0);
    container.pack_start(&local_box, false, false, 0);

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
        model_label: model_label.clone(),
        model_entry: model_entry.clone(),
        model_help: model_help.clone(),
        local_box: local_box.clone(),
        local_model_combo: local_model_combo.clone(),
        local_status_label: local_status_label.clone(),
        local_download_button: local_download_button.clone(),
        local_cancel_button: local_cancel_button.clone(),
        local_delete_button: local_delete_button.clone(),
        local_warm_check: local_warm_check.clone(),
        local_idle_entry: local_idle_entry.clone(),
        local_threads_entry: local_threads_entry.clone(),
        local_max_reply_entry: local_max_reply_entry.clone(),
        local_context_entry: local_context_entry.clone(),
        local_startup_entry: local_startup_entry.clone(),
        local_reply_entry: local_reply_entry.clone(),
        local_runtime_error,
        local_was_busy: false,
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
    render_local(&controls);

    {
        let controls_for_local_model = controls.clone();
        local_model_combo.connect_changed(move |_| {
            render_local(&controls_for_local_model);
        });
    }
    {
        let controls_for_download = controls.clone();
        local_download_button.connect_clicked(move |_| {
            let selected = {
                let borrowed = controls_for_download.borrow();
                local_model_at(borrowed.local_model_combo.active().unwrap_or(0))
            };
            if let Err(error) = install::start_install(selected, Arc::new(|| {})) {
                let borrowed = controls_for_download.borrow();
                set_status(&borrowed.status_label, &error, true);
                return;
            }
            render_local(&controls_for_download);
        });
    }
    {
        let controls_for_cancel = controls.clone();
        local_cancel_button.connect_clicked(move |_| {
            install::cancel_install();
            render_local(&controls_for_cancel);
        });
    }
    {
        let controls_for_delete = controls.clone();
        let window_for_delete = window.clone();
        local_delete_button.connect_clicked(move |_| {
            let selected = {
                let borrowed = controls_for_delete.borrow();
                local_model_at(borrowed.local_model_combo.active().unwrap_or(0))
            };
            let label = catalog::model_file(selected).label;
            // No borrow of controls is held across run(): it spins a nested
            // main loop and handlers (and the refresh timer) borrow controls.
            let dialog = MessageDialog::new(
                Some(&window_for_delete),
                DialogFlags::MODAL,
                MessageType::Question,
                ButtonsType::None,
                &format!("Delete {label}? You can download it again later."),
            );
            dialog.add_button("Cancel", ResponseType::Cancel);
            dialog.add_button("Delete", ResponseType::Accept);
            let response = dialog.run();
            dialog.close();
            if response != ResponseType::Accept {
                return;
            }
            server::stop("model deleted");
            match install::delete_model(selected) {
                Ok(()) => {
                    {
                        let borrowed = controls_for_delete.borrow();
                        set_status(&borrowed.status_label, "Deleted.", false);
                    }
                    super::notify_changed();
                }
                Err(error) => {
                    let borrowed = controls_for_delete.borrow();
                    set_status(&borrowed.status_label, &error, true);
                }
            }
            render_local(&controls_for_delete);
        });
    }
    {
        let controls_for_timer = controls.clone();
        gtk::glib::timeout_add_local(Duration::from_millis(250), move || {
            let open = STATE.with(|state| state.borrow().is_some());
            if !open {
                return gtk::glib::ControlFlow::Break;
            }
            render_local(&controls_for_timer);
            gtk::glib::ControlFlow::Continue
        });
    }

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
            if provider != Provider::Local {
                if let Err(error) = crate::config::validate_model_name(&model) {
                    let borrowed = controls_for_save.borrow();
                    set_status(&borrowed.status_label, &error, true);
                    return;
                }
            }
            let local_snapshot = {
                let borrowed = controls_for_save.borrow();
                (
                    local_model_at(borrowed.local_model_combo.active().unwrap_or(0)),
                    borrowed.local_warm_check.is_active(),
                    super::LocalFields {
                        idle_minutes: borrowed.local_idle_entry.text().to_string(),
                        threads: borrowed.local_threads_entry.text().to_string(),
                        max_reply: borrowed.local_max_reply_entry.text().to_string(),
                        context: borrowed.local_context_entry.text().to_string(),
                        startup_secs: borrowed.local_startup_entry.text().to_string(),
                        reply_secs: borrowed.local_reply_entry.text().to_string(),
                    },
                )
            };
            {
                let mut borrowed = controls_for_save.borrow_mut();
                if let Err(error) = super::apply_local_fields(
                    &mut borrowed.settings,
                    local_snapshot.0,
                    local_snapshot.1,
                    &local_snapshot.2,
                ) {
                    set_status(&borrowed.status_label, &error, true);
                    return;
                }
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

    let scrolled = ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scrolled.set_policy(PolicyType::Never, PolicyType::Automatic);
    scrolled.add(&container);
    window.add(&scrolled);
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
    render_local(&controls);
    window.present();
    Ok(())
}
