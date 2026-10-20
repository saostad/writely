use std::cell::{OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSButton, NSColor, NSMenu, NSMenuItem, NSPopUpButton,
    NSSecureTextField, NSTextField, NSWindow, NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSRunLoop, NSSize, NSString, NSURL,
};

use crate::config::{AppSettings, Effort, Provider};

const PROVIDER_TITLES: [&str; 3] = [
    "Google Gemini",
    "Together AI",
    "Ollama (local, no key needed)",
];

const EFFORT_TITLES: [&str; 5] = ["Default", "Minimal (fastest)", "Low", "Medium", "High"];

fn provider_at(index: isize) -> Provider {
    match index {
        1 => Provider::Together,
        2 => Provider::Ollama,
        _ => Provider::Gemini,
    }
}

fn provider_index(provider: Provider) -> isize {
    match provider {
        Provider::Gemini => 0,
        Provider::Together => 1,
        Provider::Ollama => 2,
    }
}

fn effort_at(index: isize) -> Effort {
    match index {
        1 => Effort::Minimal,
        2 => Effort::Low,
        3 => Effort::Medium,
        4 => Effort::High,
        _ => Effort::Default,
    }
}

fn effort_index(effort: Effort) -> isize {
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

#[derive(Default)]
struct DelegateIvars {
    field: OnceCell<Retained<NSSecureTextField>>,
    status: OnceCell<Retained<NSTextField>>,
}

fn set_status(status: &NSTextField, text: &str, is_error: bool) {
    status.setStringValue(&NSString::from_str(text));
    if is_error {
        status.setTextColor(Some(&NSColor::systemRedColor()));
    } else {
        status.setTextColor(Some(&NSColor::labelColor()));
    }
}

/// Refreshes the provider-dependent rows (model text, placeholders, key vs.
/// Ollama-address visibility, link title) for the currently selected provider.
fn refresh_provider_rows() {
    struct Rows {
        provider: Provider,
        model_field: Retained<NSTextField>,
        key_field: Retained<NSSecureTextField>,
        url_field: Retained<NSTextField>,
        key_label: Retained<NSTextField>,
        key_help: Retained<NSTextField>,
        link_button: Retained<NSButton>,
        url_label: Retained<NSTextField>,
        url_help: Retained<NSTextField>,
        status: Retained<NSTextField>,
        model: String,
        ollama_url: String,
    }
    let rows = STATE.with(|state| {
        state.borrow().as_ref().map(|window_state| Rows {
            provider: provider_at(window_state.provider_popup.indexOfSelectedItem()),
            model_field: window_state.model_field.clone(),
            key_field: window_state.field.clone(),
            url_field: window_state.url_field.clone(),
            key_label: window_state.key_label.clone(),
            key_help: window_state.key_help.clone(),
            link_button: window_state.link_button.clone(),
            url_label: window_state.url_label.clone(),
            url_help: window_state.url_help.clone(),
            status: window_state.status.clone(),
            model: window_state.settings.model_for(provider_at(
                window_state.provider_popup.indexOfSelectedItem(),
            )),
            ollama_url: window_state.settings.ollama_url.clone(),
        })
    });
    let Some(rows) = rows else { return };
    rows.model_field
        .setStringValue(&NSString::from_str(&rows.model));
    rows.model_field
        .setPlaceholderString(Some(&NSString::from_str(model_placeholder_for(
            rows.provider,
        ))));
    let is_ollama = rows.provider == Provider::Ollama;
    rows.key_label.setHidden(is_ollama);
    rows.key_field.setHidden(is_ollama);
    rows.key_help.setHidden(is_ollama);
    rows.link_button.setHidden(is_ollama);
    rows.url_label.setHidden(!is_ollama);
    rows.url_field.setHidden(!is_ollama);
    rows.url_help.setHidden(!is_ollama);
    rows.key_label
        .setStringValue(&NSString::from_str(key_title_for(rows.provider)));
    rows.key_help
        .setStringValue(&NSString::from_str(key_help_for(rows.provider)));
    if let Some(page) = super::api_key_page_for(rows.provider) {
        rows.link_button.setTitle(&NSString::from_str(page));
    }
    if !is_ollama {
        match super::has_api_key_for(rows.provider) {
            Ok(true) => rows
                .key_field
                .setPlaceholderString(Some(&NSString::from_str("••••••••"))),
            Ok(false) => rows
                .key_field
                .setPlaceholderString(Some(&NSString::from_str("Paste your key here"))),
            Err(error) => set_status(&rows.status, &error, true),
        }
    }
    if rows.url_field.stringValue().to_string().is_empty() {
        rows.url_field
            .setStringValue(&NSString::from_str(&rows.ollama_url));
    }
    rows.url_field
        .setPlaceholderString(Some(&NSString::from_str(crate::config::DEFAULT_OLLAMA_URL)));
}

/// Reads the draft provider, model, effort, Ollama URL, start-at-login and
/// clipboard-fallback checkboxes from the controls.
fn read_draft() -> Option<(Provider, String, Effort, String, bool, bool)> {
    STATE.with(|state| {
        state.borrow().as_ref().map(|window_state| {
            (
                provider_at(window_state.provider_popup.indexOfSelectedItem()),
                window_state.model_field.stringValue().to_string(),
                effort_at(window_state.effort_popup.indexOfSelectedItem()),
                window_state.url_field.stringValue().to_string(),
                window_state.autostart_checkbox.state() == 1,
                window_state.clipboard_checkbox.state() == 1,
            )
        })
    })
}

/// Stores the current control values into the draft settings.
fn stash_draft() {
    if let Some((provider, model, effort, url, launch_at_login, clipboard_fallback)) = read_draft()
    {
        STATE.with(|state| {
            if let Some(window_state) = state.borrow_mut().as_mut() {
                // The model field still shows the draft provider's model: on
                // a provider change the popup already has the new selection.
                let shown = window_state.settings.provider;
                window_state.settings.set_model(shown, model.trim());
                window_state.settings.provider = provider;
                window_state.settings.effort = effort;
                window_state.settings.launch_at_login = launch_at_login;
                window_state.settings.clipboard_fallback = clipboard_fallback;
                if !url.trim().is_empty() {
                    window_state.settings.ollama_url = url.trim().to_string();
                }
            }
        });
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and SettingsDelegate does not implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = DelegateIvars]
    struct SettingsDelegate;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for SettingsDelegate {}

    impl SettingsDelegate {
        #[unsafe(method(saveClicked:))]
        fn save_clicked(&self, _sender: Option<&AnyObject>) {
            let Some(field) = self.ivars().field.get() else {
                return;
            };
            let Some(status) = self.ivars().status.get() else {
                return;
            };
            let Some((provider, model, _, _, launch_at_login, _)) = read_draft() else {
                return;
            };
            if let Err(error) = crate::config::validate_model_name(&model) {
                set_status(status, &error, true);
                return;
            }
            stash_draft();
            let save_result = STATE.with(|state| {
                state.borrow().as_ref().map(|window_state| {
                    crate::config::save_settings(&window_state.settings)
                })
            });
            match save_result {
                Some(Ok(())) => {}
                Some(Err(error)) => {
                    set_status(status, &error, true);
                    return;
                }
                None => return,
            }
            if let Err(error) = crate::autostart::set_enabled(launch_at_login) {
                set_status(status, &error, true);
                return;
            }
            super::set_autostart_message(None);
            let value = field.stringValue().to_string();
            if value.trim().is_empty() {
                set_status(status, "Saved.", false);
            } else {
                match super::save_api_key_for(provider, &value) {
                    Ok(()) => set_status(status, "Saved.", false),
                    Err(error) => {
                        set_status(status, &error, true);
                        return;
                    }
                }
            }
            field.setStringValue(&NSString::from_str(""));
            match super::has_api_key_for(provider) {
                Ok(true) => field.setPlaceholderString(Some(&NSString::from_str("••••••••"))),
                Ok(false) => {
                    field.setPlaceholderString(Some(&NSString::from_str("Paste your key here")))
                }
                Err(error) => set_status(status, &error, true),
            }
        }

        #[unsafe(method(removeClicked:))]
        fn remove_clicked(&self, _sender: Option<&AnyObject>) {
            let Some(field) = self.ivars().field.get() else {
                return;
            };
            let Some(status) = self.ivars().status.get() else {
                return;
            };
            let provider = read_draft().map(|draft| draft.0).unwrap_or(Provider::Gemini);
            match super::remove_api_key_for(provider) {
                Ok(()) => {
                    field.setStringValue(&NSString::from_str(""));
                    field.setPlaceholderString(Some(&NSString::from_str("Paste your key here")));
                    set_status(status, "Key removed.", false);
                }
                Err(error) => set_status(status, &error, true),
            }
        }

        #[unsafe(method(openLink:))]
        fn open_link(&self, _sender: Option<&AnyObject>) {
            let Some(status) = self.ivars().status.get() else {
                return;
            };
            let provider = read_draft().map(|draft| draft.0).unwrap_or(Provider::Gemini);
            let Some(page) = super::api_key_page_for(provider) else {
                set_status(status, "Ollama runs on your computer and needs no API key.", true);
                return;
            };
            let url_string = NSString::from_str(page);
            let Some(url) = NSURL::URLWithString(&url_string) else {
                set_status(
                    status,
                    "Could not open the API key page in your browser: invalid API key page URL",
                    true,
                );
                return;
            };
            if !NSWorkspace::sharedWorkspace().openURL(&url) {
                set_status(
                    status,
                    "Could not open the API key page in your browser: NSWorkspace could not open the API key page",
                    true,
                );
            }
        }

        #[unsafe(method(providerChanged:))]
        fn provider_changed(&self, _sender: Option<&AnyObject>) {
            // Stash the model text into the draft for the previous provider
            // first, so switching providers keeps each one's model.
            stash_draft();
            refresh_provider_rows();
        }

        #[unsafe(method(openAccessibilitySettings:))]
        fn open_accessibility_settings(&self, _sender: Option<&AnyObject>) {
            let Some(status) = self.ivars().status.get() else {
                return;
            };
            let url_string = NSString::from_str(super::ACCESSIBILITY_SETTINGS_URL);
            let Some(url) = NSURL::URLWithString(&url_string) else {
                set_status(
                    status,
                    "Could not open System Settings: invalid Accessibility settings URL",
                    true,
                );
                return;
            };
            if !NSWorkspace::sharedWorkspace().openURL(&url) {
                set_status(
                    status,
                    "Could not open System Settings: NSWorkspace could not open System Settings",
                    true,
                );
            }
        }
    }

    // SAFETY: NSWindowDelegate has no safety requirements.
    unsafe impl NSWindowDelegate for SettingsDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            STATE.with(|state| {
                if let Some(window_state) = state.borrow().as_ref() {
                    window_state.window.setDelegate(None);
                }
            });
            let taken = STATE.with(|state| state.borrow_mut().take());
            if let Some(taken) = taken {
                let taken_cell = std::cell::RefCell::new(Some(taken));
                let block = block2::RcBlock::new(move || {
                    drop(taken_cell.borrow_mut().take());
                });
                // SAFETY: the main run loop is valid and the block is copied by the run loop.
                unsafe { NSRunLoop::mainRunLoop().performBlock(&block) };
            }
        }
    }
);

impl SettingsDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars::default());
        // SAFETY: NSObject init signature is correct.
        unsafe { msg_send![super(this), init] }
    }
}

struct WindowState {
    window: Retained<NSWindow>,
    #[allow(dead_code)]
    delegate: Retained<SettingsDelegate>,
    field: Retained<NSSecureTextField>,
    status: Retained<NSTextField>,
    accessibility_button: Option<Retained<NSButton>>,
    provider_popup: Retained<NSPopUpButton>,
    model_field: Retained<NSTextField>,
    effort_popup: Retained<NSPopUpButton>,
    url_field: Retained<NSTextField>,
    key_label: Retained<NSTextField>,
    key_help: Retained<NSTextField>,
    link_button: Retained<NSButton>,
    url_label: Retained<NSTextField>,
    url_help: Retained<NSTextField>,
    autostart_checkbox: Retained<NSButton>,
    clipboard_checkbox: Retained<NSButton>,
    settings: AppSettings,
}

thread_local! {
    static STATE: RefCell<Option<WindowState>> = const { RefCell::new(None) };
}

fn ensure_edit_menu(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    if let Some(main_menu) = app.mainMenu() {
        let count = main_menu.numberOfItems();
        for index in 0..count {
            if let Some(item) = main_menu.itemAtIndex(index) {
                if item.title().to_string() == "Edit" {
                    return;
                }
            }
        }
        let edit_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Edit"));
        // SAFETY: paste: and selectAll: are valid NSResponder selectors with no target.
        let paste = unsafe {
            edit_menu.addItemWithTitle_action_keyEquivalent(
                ns_string!("Paste"),
                Some(sel!(paste:)),
                ns_string!("v"),
            )
        };
        drop(paste);
        // SAFETY: paste: and selectAll: are valid NSResponder selectors with no target.
        let select_all = unsafe {
            edit_menu.addItemWithTitle_action_keyEquivalent(
                ns_string!("Select All"),
                Some(sel!(selectAll:)),
                ns_string!("a"),
            )
        };
        drop(select_all);
        let edit_item = NSMenuItem::new(mtm);
        edit_item.setTitle(ns_string!("Edit"));
        edit_item.setSubmenu(Some(&edit_menu));
        main_menu.addItem(&edit_item);
        return;
    }
    let main_menu = NSMenu::new(mtm);
    let edit_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Edit"));
    // SAFETY: paste: and selectAll: are valid NSResponder selectors with no target.
    let paste = unsafe {
        edit_menu.addItemWithTitle_action_keyEquivalent(
            ns_string!("Paste"),
            Some(sel!(paste:)),
            ns_string!("v"),
        )
    };
    drop(paste);
    // SAFETY: paste: and selectAll: are valid NSResponder selectors with no target.
    let select_all = unsafe {
        edit_menu.addItemWithTitle_action_keyEquivalent(
            ns_string!("Select All"),
            Some(sel!(selectAll:)),
            ns_string!("a"),
        )
    };
    drop(select_all);
    let edit_item = NSMenuItem::new(mtm);
    edit_item.setTitle(ns_string!("Edit"));
    edit_item.setSubmenu(Some(&edit_menu));
    main_menu.addItem(&edit_item);
    app.setMainMenu(Some(&main_menu));
}

pub fn show_settings() -> Result<(), String> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(
            "Could not open the settings window: Writely must open Settings on the main thread."
                .to_string(),
        );
    };
    ensure_edit_menu(mtm);

    let already_open = STATE.with(|state| state.borrow().is_some());
    if already_open {
        STATE.with(|state| {
            if let Some(window_state) = state.borrow().as_ref() {
                if let Some(message) = super::permission_message() {
                    set_status(&window_state.status, &message, true);
                    if let Some(button) = window_state.accessibility_button.as_ref() {
                        button.setHidden(false);
                    }
                } else {
                    if let Some(button) = window_state.accessibility_button.as_ref() {
                        button.setHidden(true);
                    }
                    let provider = provider_at(window_state.provider_popup.indexOfSelectedItem());
                    if provider != Provider::Ollama {
                        match super::has_api_key_for(provider) {
                            Ok(true) => window_state
                                .field
                                .setPlaceholderString(Some(&NSString::from_str("••••••••"))),
                            Ok(false) => {
                                window_state
                                    .field
                                    .setPlaceholderString(Some(&NSString::from_str(
                                        "Paste your key here",
                                    )))
                            }
                            Err(error) => set_status(&window_state.status, &error, true),
                        }
                    }
                }
                NSApplication::sharedApplication(mtm).activate();
                window_state.window.makeKeyAndOrderFront(None);
            }
        });
        return Ok(());
    }

    let delegate = SettingsDelegate::new(mtm);
    let permission_text = super::permission_message();
    let window_height = if permission_text.is_some() {
        672.0
    } else {
        604.0
    };
    // SAFETY: the content rect, style mask and backing store are valid and mtm is the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(420.0, window_height)),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window is owned by a Retained in STATE, so AppKit must not also release it on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(ns_string!("Writely Settings"));
    window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    window.center();

    let Some(content) = window.contentView() else {
        return Err(
            "Could not open the settings window: the window has no content view.".to_string(),
        );
    };

    // Top-down layout: each row takes its height plus a gap, so nothing overlaps.
    let mut cursor = window_height - 16.0;
    let mut row = |height: f64, gap: f64| -> NSRect {
        cursor -= height;
        let rect = NSRect::new(NSPoint::new(20.0, cursor), NSSize::new(380.0, height));
        cursor -= gap;
        rect
    };

    let provider_label = NSTextField::labelWithString(ns_string!("Provider"), mtm);
    provider_label.setFrame(row(18.0, 4.0));

    // SAFETY: the frame is valid and mtm is the main thread.
    let provider_popup =
        NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), row(26.0, 2.0), false);
    for title in PROVIDER_TITLES {
        provider_popup.addItemWithTitle(&NSString::from_str(title));
    }

    let provider_help = NSTextField::labelWithString(ns_string!("Who checks your grammar."), mtm);
    provider_help.setFrame(row(16.0, 10.0));

    let model_label = NSTextField::labelWithString(ns_string!("Model"), mtm);
    model_label.setFrame(row(18.0, 4.0));

    let model_field = NSTextField::initWithFrame(NSTextField::alloc(mtm), row(24.0, 2.0));

    let model_help = NSTextField::wrappingLabelWithString(
        ns_string!("Paste the model name, e.g. gemini-3.5-flash-lite / deepseek-ai/DeepSeek-V4.1-Flash / llama3.2"),
        mtm,
    );
    model_help.setFrame(row(44.0, 10.0));

    let effort_label = NSTextField::labelWithString(ns_string!("Effort"), mtm);
    effort_label.setFrame(row(18.0, 4.0));

    // SAFETY: the frame is valid and mtm is the main thread.
    let effort_popup =
        NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), row(26.0, 2.0), false);
    for title in EFFORT_TITLES {
        effort_popup.addItemWithTitle(&NSString::from_str(title));
    }

    let effort_help = NSTextField::wrappingLabelWithString(
        ns_string!("How hard the model thinks. Not every model supports every level; you'll see the model's error if not."),
        mtm,
    );
    effort_help.setFrame(row(34.0, 10.0));

    // The key rows and the Ollama-address rows share the same frames; only one
    // set is visible at a time.
    let key_label = NSTextField::labelWithString(ns_string!("Google Gemini API key"), mtm);
    key_label.setFrame(row(18.0, 4.0));
    let key_frame = key_label.frame();

    let field = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), row(24.0, 2.0));
    let key_field_frame = field.frame();

    let key_help = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
    key_help.setFrame(row(34.0, 2.0));
    let key_help_frame = key_help.frame();

    let target: &AnyObject = &delegate;
    // SAFETY: delegate implements openLink: and lives in STATE while the window is open.
    let link_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("https://aistudio.google.com/apikey"),
            Some(target),
            Some(sel!(openLink:)),
            mtm,
        )
    };
    link_button.setFrame(row(18.0, 10.0));
    link_button.setBordered(false);

    let url_label = NSTextField::labelWithString(ns_string!("Ollama address"), mtm);
    url_label.setFrame(key_frame);
    let url_field = NSTextField::initWithFrame(NSTextField::alloc(mtm), key_field_frame);
    let url_help = NSTextField::wrappingLabelWithString(
        ns_string!("Where Ollama runs on your computer. No key needed."),
        mtm,
    );
    url_help.setFrame(key_help_frame);

    // SAFETY: a checkbox with no target/action is valid; its state is read on Save.
    let autostart_checkbox = unsafe {
        NSButton::checkboxWithTitle_target_action(
            ns_string!("Start Writely at login"),
            None,
            None,
            mtm,
        )
    };
    autostart_checkbox.setFrame(row(22.0, 2.0));

    let autostart_help = NSTextField::wrappingLabelWithString(
        ns_string!("Writely starts automatically when you log in."),
        mtm,
    );
    autostart_help.setFrame(row(16.0, 10.0));

    // SAFETY: a checkbox with no target/action is valid; its state is read on Save.
    let clipboard_checkbox = unsafe {
        NSButton::checkboxWithTitle_target_action(
            ns_string!("Use the clipboard when an app doesn't share its text"),
            None,
            None,
            mtm,
        )
    };
    clipboard_checkbox.setFrame(row(22.0, 2.0));

    let clipboard_help = NSTextField::wrappingLabelWithString(
        ns_string!("Writely copies your selection and then puts your clipboard back. Turn off to never touch the clipboard."),
        mtm,
    );
    clipboard_help.setFrame(row(34.0, 10.0));

    // SAFETY: delegate implements saveClicked: and lives in STATE while the window is open.
    let save_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("Save"),
            Some(target),
            Some(sel!(saveClicked:)),
            mtm,
        )
    };
    let buttons_frame = row(26.0, 12.0);
    save_button.setFrame(NSRect::new(buttons_frame.origin, NSSize::new(80.0, 26.0)));

    // SAFETY: delegate implements removeClicked: and lives in STATE while the window is open.
    let remove_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("Remove key"),
            Some(target),
            Some(sel!(removeClicked:)),
            mtm,
        )
    };
    remove_button.setFrame(NSRect::new(
        NSPoint::new(buttons_frame.origin.x + 90.0, buttons_frame.origin.y),
        NSSize::new(110.0, 26.0),
    ));

    let hotkey_hint = NSTextField::wrappingLabelWithString(
        ns_string!("Tip: select text in any app and press ⌃⌥G to check it."),
        mtm,
    );
    hotkey_hint.setFrame(row(18.0, 10.0));

    // The Accessibility button gets its own row above the status line and only
    // exists while the permission message is set. It uses its own selector.
    let accessibility_button: Option<Retained<NSButton>> = if permission_text.is_some() {
        // SAFETY: delegate implements openAccessibilitySettings: and lives in STATE while the window is open.
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Open Accessibility Settings"),
                Some(target),
                Some(sel!(openAccessibilitySettings:)),
                mtm,
            )
        };
        button.setFrame(row(24.0, 8.0));
        content.addSubview(&button);
        Some(button)
    } else {
        None
    };

    let status_label = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
    status_label.setFrame(NSRect::new(
        NSPoint::new(20.0, 14.0),
        NSSize::new(
            380.0,
            if permission_text.is_some() {
                54.0
            } else {
                18.0
            },
        ),
    ));

    // SAFETY: delegate implements providerChanged: and lives in STATE while the window is open.
    unsafe {
        provider_popup.setTarget(Some(target));
        provider_popup.setAction(Some(sel!(providerChanged:)));
    }

    // Load the draft settings: a missing file means defaults; an unreadable
    // or invalid file shows its error and starts from defaults (saving then
    // overwrites it, which is explicit, not silent).
    let mut initial_error: Option<String> = None;
    let settings = match crate::config::load_settings() {
        Ok(settings) => settings,
        Err(error) => {
            initial_error = Some(error);
            AppSettings::default()
        }
    };
    provider_popup.selectItemAtIndex(provider_index(settings.provider));
    effort_popup.selectItemAtIndex(effort_index(settings.effort));
    model_field.setStringValue(&NSString::from_str(&settings.model_for(settings.provider)));
    model_field.setPlaceholderString(Some(&NSString::from_str(model_placeholder_for(
        settings.provider,
    ))));
    autostart_checkbox.setState(if settings.launch_at_login { 1 } else { 0 });
    clipboard_checkbox.setState(if settings.clipboard_fallback { 1 } else { 0 });
    url_field.setStringValue(&NSString::from_str(&settings.ollama_url));
    url_field.setPlaceholderString(Some(&NSString::from_str(crate::config::DEFAULT_OLLAMA_URL)));
    key_help.setStringValue(&NSString::from_str(key_help_for(settings.provider)));
    key_label.setStringValue(&NSString::from_str(key_title_for(settings.provider)));
    if let Some(page) = super::api_key_page_for(settings.provider) {
        link_button.setTitle(&NSString::from_str(page));
    }
    let is_ollama = settings.provider == Provider::Ollama;
    key_label.setHidden(is_ollama);
    field.setHidden(is_ollama);
    key_help.setHidden(is_ollama);
    link_button.setHidden(is_ollama);
    url_label.setHidden(!is_ollama);
    url_field.setHidden(!is_ollama);
    url_help.setHidden(!is_ollama);
    if !is_ollama {
        match super::has_api_key_for(settings.provider) {
            Ok(true) => field.setPlaceholderString(Some(&NSString::from_str("••••••••"))),
            Ok(false) => {
                field.setPlaceholderString(Some(&NSString::from_str("Paste your key here")))
            }
            Err(error) => initial_error = Some(error),
        }
    }
    if let Some(message) = permission_text.as_ref() {
        set_status(&status_label, message, true);
    } else if let Some(error) = &initial_error {
        set_status(&status_label, error, true);
    } else if let Some(hotkey_error) = super::hotkey_message() {
        set_status(&status_label, &hotkey_error, true);
    } else if let Some(autostart_error) = super::autostart_message() {
        set_status(&status_label, &autostart_error, true);
    }

    if delegate.ivars().field.set(field.clone()).is_err() {
        eprintln!("Writely: could not store the Settings key field.");
    }
    if delegate.ivars().status.set(status_label.clone()).is_err() {
        eprintln!("Writely: could not store the Settings status label.");
    }

    content.addSubview(&provider_label);
    content.addSubview(&provider_popup);
    content.addSubview(&provider_help);
    content.addSubview(&model_label);
    content.addSubview(&model_field);
    content.addSubview(&model_help);
    content.addSubview(&effort_label);
    content.addSubview(&effort_popup);
    content.addSubview(&effort_help);
    content.addSubview(&key_label);
    content.addSubview(&field);
    content.addSubview(&key_help);
    content.addSubview(&link_button);
    content.addSubview(&url_label);
    content.addSubview(&url_field);
    content.addSubview(&url_help);
    content.addSubview(&autostart_checkbox);
    content.addSubview(&autostart_help);
    content.addSubview(&clipboard_checkbox);
    content.addSubview(&clipboard_help);
    content.addSubview(&save_button);
    content.addSubview(&remove_button);
    content.addSubview(&hotkey_hint);
    content.addSubview(&status_label);

    STATE.with(|state| {
        *state.borrow_mut() = Some(WindowState {
            window: window.clone(),
            delegate,
            field: field.clone(),
            status: status_label.clone(),
            accessibility_button,
            provider_popup: provider_popup.clone(),
            model_field: model_field.clone(),
            effort_popup: effort_popup.clone(),
            url_field: url_field.clone(),
            key_label: key_label.clone(),
            key_help: key_help.clone(),
            link_button: link_button.clone(),
            url_label: url_label.clone(),
            url_help: url_help.clone(),
            autostart_checkbox: autostart_checkbox.clone(),
            clipboard_checkbox: clipboard_checkbox.clone(),
            settings,
        });
    });

    NSApplication::sharedApplication(mtm).activate();
    window.makeKeyAndOrderFront(None);
    window.makeFirstResponder(Some(&field));
    Ok(())
}

pub fn is_settings_key_window() -> bool {
    STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|window_state| window_state.window.isKeyWindow())
            .unwrap_or(false)
    })
}

/// Called on the main thread when Accessibility permission is granted while
/// the app is running: hides the Accessibility button in the open Settings
/// window and shows a green ready message. Returns true if a window was open.
pub(crate) fn notify_permission_granted() -> bool {
    let found = STATE.with(|state| {
        let borrow = state.borrow();
        borrow.as_ref().map(|window_state| {
            (
                window_state.accessibility_button.clone(),
                window_state.status.clone(),
            )
        })
    });
    let Some((button, status)) = found else {
        return false;
    };
    if let Some(button) = button.as_ref() {
        button.setHidden(true);
    }
    set_status(&status, "Accessibility is on — Writely is ready.", false);
    true
}
