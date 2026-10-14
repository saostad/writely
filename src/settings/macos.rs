use std::cell::{OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSButton, NSColor, NSMenu, NSMenuItem, NSSecureTextField,
    NSTextField, NSWindow, NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSRunLoop, NSSize, NSString, NSURL,
};

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
            let value = field.stringValue().to_string();
            match super::save_api_key(&value) {
                Ok(()) => {
                    field.setStringValue(&NSString::from_str(""));
                    field.setPlaceholderString(Some(&NSString::from_str("••••••••")));
                    set_status(status, "Saved.", false);
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
            match super::remove_api_key() {
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
            let url_string = NSString::from_str(super::API_KEY_PAGE);
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
                    match super::has_api_key() {
                        Ok(true) => window_state
                            .field
                            .setPlaceholderString(Some(&NSString::from_str("••••••••"))),
                        Ok(false) => window_state
                            .field
                            .setPlaceholderString(Some(&NSString::from_str("Paste your key here"))),
                        Err(error) => set_status(&window_state.status, &error, true),
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
        260.0
    } else {
        200.0
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

    // When Accessibility permission is missing the controls shift up to make room
    // for the "Open Accessibility Settings" button row and a taller wrapping
    // status label; otherwise the layout is unchanged from before.
    let dy = if permission_text.is_some() { 52.0 } else { 0.0 };

    let title_label = NSTextField::labelWithString(ns_string!("Google Gemini API key"), mtm);
    title_label.setFrame(NSRect::new(
        NSPoint::new(20.0, 172.0 + dy),
        NSSize::new(380.0, 18.0),
    ));

    let field = NSSecureTextField::initWithFrame(
        NSSecureTextField::alloc(mtm),
        NSRect::new(NSPoint::new(20.0, 142.0 + dy), NSSize::new(380.0, 24.0)),
    );

    let mut initial_error: Option<String> = None;
    match super::has_api_key() {
        Ok(true) => field.setPlaceholderString(Some(&NSString::from_str("••••••••"))),
        Ok(false) => field.setPlaceholderString(Some(&NSString::from_str("Paste your key here"))),
        Err(error) => {
            field.setPlaceholderString(Some(&NSString::from_str("Paste your key here")));
            initial_error = Some(error);
        }
    }

    let help_label = NSTextField::wrappingLabelWithString(
        ns_string!("Writely sends the sentence you're editing to Google Gemini to check its grammar. Get a free key at "),
        mtm,
    );
    help_label.setFrame(NSRect::new(
        NSPoint::new(20.0, 104.0 + dy),
        NSSize::new(380.0, 32.0),
    ));

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
    link_button.setFrame(NSRect::new(
        NSPoint::new(20.0, 86.0 + dy),
        NSSize::new(280.0, 18.0),
    ));
    link_button.setBordered(false);

    // SAFETY: delegate implements saveClicked: and lives in STATE while the window is open.
    let save_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("Save"),
            Some(target),
            Some(sel!(saveClicked:)),
            mtm,
        )
    };
    save_button.setFrame(NSRect::new(
        NSPoint::new(20.0, 48.0 + dy),
        NSSize::new(80.0, 24.0),
    ));

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
        NSPoint::new(110.0, 48.0 + dy),
        NSSize::new(110.0, 24.0),
    ));

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
        button.setFrame(NSRect::new(
            NSPoint::new(20.0, 74.0),
            NSSize::new(220.0, 24.0),
        ));
        content.addSubview(&button);
        Some(button)
    } else {
        None
    };

    let status_label = if permission_text.is_some() {
        NSTextField::wrappingLabelWithString(ns_string!(""), mtm)
    } else {
        NSTextField::labelWithString(ns_string!(""), mtm)
    };
    status_label.setFrame(NSRect::new(
        NSPoint::new(
            20.0,
            if permission_text.is_some() {
                14.0
            } else {
                18.0
            },
        ),
        NSSize::new(
            380.0,
            if permission_text.is_some() {
                54.0
            } else {
                18.0
            },
        ),
    ));
    if let Some(message) = permission_text.as_ref() {
        set_status(&status_label, message, true);
    } else if let Some(error) = &initial_error {
        set_status(&status_label, error, true);
    }

    if delegate.ivars().field.set(field.clone()).is_err() {
        eprintln!("Writely: could not store the Settings key field.");
    }
    if delegate.ivars().status.set(status_label.clone()).is_err() {
        eprintln!("Writely: could not store the Settings status label.");
    }

    content.addSubview(&title_label);
    content.addSubview(&field);
    content.addSubview(&help_label);
    content.addSubview(&link_button);
    content.addSubview(&save_button);
    content.addSubview(&remove_button);
    content.addSubview(&status_label);

    STATE.with(|state| {
        *state.borrow_mut() = Some(WindowState {
            window: window.clone(),
            delegate,
            field: field.clone(),
            status: status_label.clone(),
            accessibility_button,
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
