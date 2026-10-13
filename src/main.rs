#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gemini;
mod overlay;
mod popup;
mod settings;
mod text_access;
mod watcher;

use std::ops::Range;

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use text_access::{CaretRect, FieldSnapshot};

#[derive(Debug)]
pub enum UserEvent {
    MenuEvent(MenuEvent),
    CaretChanged,
    IconClicked,
    ReplaceClicked,
    Suggestion {
        generation: u64,
        result: Result<String, String>,
    },
}

/// The in-flight click: snapshot + sentence range + sentence text, waiting
/// for the Gemini reply.
struct Checking {
    snap: FieldSnapshot,
    range: Range<usize>,
    sentence: String,
}

/// A shown suggestion waiting for Replace.
struct Pending {
    snap: FieldSnapshot,
    range: Range<usize>,
    suggestion: String,
}

/// Shows the popup at `anchor` (falling back to `fallback`), remembers the
/// position for later updates, and reports whether it was shown.
fn show_popup(
    text: &str,
    is_error: bool,
    show_button: bool,
    anchor: Option<CaretRect>,
    fallback: Option<CaretRect>,
    last_anchor: &mut Option<CaretRect>,
) {
    if let Some(rect) = anchor.or(fallback) {
        *last_anchor = Some(rect);
        popup::show_message(text, is_error, show_button, &rect);
    } else {
        eprintln!("Writely: could not show the suggestion: the caret position is unknown.");
    }
}

fn handle_icon_clicked(
    proxy: &EventLoopProxy<UserEvent>,
    generation: &mut u64,
    checking: &mut Option<Checking>,
    pending: &mut Option<Pending>,
    last_anchor: &mut Option<CaretRect>,
) {
    // A new click invalidates any in-flight request; stale replies are dropped.
    *generation = generation.wrapping_add(1);
    let my_generation = *generation;
    *pending = None;
    *checking = None;
    let live_anchor = match text_access::caret_info() {
        Ok(Some(info)) => Some(info.caret_rect),
        Ok(None) => None,
        Err(error) => {
            eprintln!("Writely: {error}");
            None
        }
    };
    let key = match settings::load_api_key() {
        Ok(Some(key)) => key,
        Ok(None) => {
            show_popup(
                "Add your Gemini API key first: click the Writely menu-bar icon › Settings…",
                false,
                false,
                live_anchor,
                None,
                last_anchor,
            );
            return;
        }
        Err(error) => {
            show_popup(&error, true, false, live_anchor, None, last_anchor);
            return;
        }
    };
    let Some(snap) = (match text_access::focused_field() {
        Ok(snap) => snap,
        Err(error) => {
            show_popup(&error, true, false, live_anchor, None, last_anchor);
            return;
        }
    }) else {
        show_popup(
            "Writely can't read this text field.",
            false,
            false,
            live_anchor,
            None,
            last_anchor,
        );
        return;
    };
    let anchor = live_anchor.or(Some(snap.caret_rect));
    let (range, sentence) = match text_access::sentence::sentence_range(&snap.text, snap.caret) {
        Some(range) => {
            let sentence: String = snap
                .text
                .chars()
                .skip(range.start)
                .take(range.end - range.start)
                .collect();
            (range, sentence)
        }
        None => {
            show_popup(
                "Type a sentence first.",
                false,
                false,
                anchor,
                None,
                last_anchor,
            );
            return;
        }
    };
    if sentence.trim().is_empty() {
        show_popup(
            "Type a sentence first.",
            false,
            false,
            anchor,
            None,
            last_anchor,
        );
        return;
    }
    show_popup("Checking…", false, false, anchor, None, last_anchor);
    *checking = Some(Checking {
        snap,
        range,
        sentence: sentence.clone(),
    });
    let worker_proxy = proxy.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("writely-gemini".to_string())
        .spawn(move || {
            // The key lives only in this thread's request; it is never
            // logged or put in an error.
            let result = gemini::suggest(&key, &sentence);
            if let Err(error) = worker_proxy.send_event(UserEvent::Suggestion {
                generation: my_generation,
                result,
            }) {
                eprintln!("Writely: could not deliver the suggestion: {error}");
            }
        })
    {
        *checking = None;
        show_popup(
            &format!("Could not check the sentence: {error}"),
            true,
            false,
            anchor,
            None,
            last_anchor,
        );
    }
}

fn handle_suggestion(
    current_generation: u64,
    event_generation: u64,
    result: Result<String, String>,
    checking: &mut Option<Checking>,
    pending: &mut Option<Pending>,
    last_anchor: &mut Option<CaretRect>,
) {
    // Drop late replies: a newer click, or typing that hid the popup, wins.
    if event_generation != current_generation || !popup::is_visible() {
        return;
    }
    match result {
        Err(error) => {
            *checking = None;
            *pending = None;
            show_popup(&error, true, false, *last_anchor, None, last_anchor);
        }
        Ok(suggestion) => {
            let Some(active) = checking.take() else {
                return;
            };
            if suggestion.trim() == active.sentence.trim() {
                show_popup(
                    "Looks good — no changes needed.",
                    false,
                    false,
                    *last_anchor,
                    Some(active.snap.caret_rect),
                    last_anchor,
                );
            } else {
                show_popup(
                    &suggestion,
                    false,
                    true,
                    *last_anchor,
                    Some(active.snap.caret_rect),
                    last_anchor,
                );
                *pending = Some(Pending {
                    snap: active.snap,
                    range: active.range,
                    suggestion,
                });
            }
        }
    }
}

fn handle_replace_clicked(pending: &mut Option<Pending>, last_anchor: &mut Option<CaretRect>) {
    let Some(active) = pending.take() else {
        return;
    };
    match text_access::replace_range(&active.snap, active.range, &active.suggestion) {
        Ok(()) => {
            popup::hide();
        }
        Err(error) => {
            show_popup(
                &error,
                true,
                false,
                *last_anchor,
                Some(active.snap.caret_rect),
                last_anchor,
            );
        }
    }
}

fn main() {
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        event_loop.set_activate_ignoring_other_apps(false);
    }

    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        if let Err(error) = proxy.send_event(UserEvent::MenuEvent(event)) {
            eprintln!("Writely: could not deliver the tray menu event: {error}");
        }
    }));
    let watcher_proxy = event_loop.create_proxy();
    overlay::init(watcher_proxy.clone());
    popup::init(watcher_proxy.clone());

    let mut tray_icon: Option<TrayIcon> = None;
    let mut settings_id = tray_icon::menu::MenuId::new("settings");
    let mut quit_id = tray_icon::menu::MenuId::new("quit");
    let mut caret_watcher: Option<watcher::Watcher> = None;
    let mut last_caret_error: Option<String> = None;
    // Popup flow state: bumped on every click and on hide so late Gemini
    // replies are dropped.
    let mut generation: u64 = 0;
    let mut checking: Option<Checking> = None;
    let mut pending: Option<Pending> = None;
    let mut last_anchor: Option<CaretRect> = None;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::NewEvents(StartCause::Init) => {
                let settings_item = MenuItem::with_id("settings", "Settings…", true, None);
                let quit_item = MenuItem::with_id("quit", "Quit Writely", true, None);
                settings_id = settings_item.id().clone();
                quit_id = quit_item.id().clone();

                let menu = match Menu::with_items(&[&settings_item, &quit_item]) {
                    Ok(menu) => menu,
                    Err(error) => {
                        eprintln!("Writely: could not create the tray icon: {error}");
                        std::process::exit(1);
                    }
                };

                let icon =
                    match Icon::from_rgba(include_bytes!("../icons/tray-32.rgba").to_vec(), 32, 32)
                    {
                        Ok(icon) => icon,
                        Err(error) => {
                            eprintln!("Writely: could not create the tray icon: {error}");
                            std::process::exit(1);
                        }
                    };

                let builder = TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    .with_icon(icon)
                    .with_tooltip("Writely");
                #[cfg(target_os = "macos")]
                let builder = builder.with_icon_as_template(true);

                match builder.build() {
                    Ok(icon) => {
                        tray_icon = Some(icon);
                    }
                    Err(error) => {
                        eprintln!("Writely: could not create the tray icon: {error}");
                        std::process::exit(1);
                    }
                }

                if watcher::is_wayland_without_x11() {
                    eprintln!("Writely: {}", watcher::WAYLAND_MESSAGE);
                    settings::set_permission_message(Some(watcher::WAYLAND_MESSAGE.to_string()));
                    if let Some(tray) = &tray_icon {
                        if let Err(error) = tray.set_tooltip(Some(watcher::WAYLAND_MESSAGE)) {
                            eprintln!("Writely: could not update the tray tooltip: {error}");
                        }
                    }
                } else {
                    match text_access::check_permission(true) {
                        Ok(()) => match watcher::Watcher::start(watcher_proxy.clone()) {
                            Ok(watcher) => {
                                caret_watcher = Some(watcher);
                            }
                            Err(error) => {
                                eprintln!("Writely: {error}");
                            }
                        },
                        Err(error) => {
                            eprintln!("Writely: {error}");
                            settings::set_permission_message(Some(error));
                            if let Some(tray) = &tray_icon {
                                if let Err(tooltip_error) = tray.set_tooltip(Some(
                                    "Writely needs Accessibility permission — see Settings",
                                )) {
                                    eprintln!(
                                        "Writely: could not update the tray tooltip: {tooltip_error}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::MenuEvent(event)) => {
                if event.id == settings_id {
                    popup::hide();
                    if let Err(error) = settings::show_settings() {
                        eprintln!("Writely: {error}");
                    }
                } else if event.id == quit_id {
                    drop(tray_icon.take());
                    caret_watcher = None;
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UserEvent::CaretChanged) => {
                // Typing or moving focus hides the popup and drops any
                // in-flight suggestion.
                if popup::is_visible() {
                    popup::hide();
                    generation = generation.wrapping_add(1);
                    checking = None;
                    pending = None;
                }
                if let Some(watcher) = caret_watcher.as_mut() {
                    watcher.handle_caret_changed();
                }
                match text_access::caret_info() {
                    Ok(Some(info)) => {
                        last_caret_error = None;
                        overlay::show_at(&info.caret_rect);
                    }
                    Ok(None) => {
                        last_caret_error = None;
                        overlay::hide();
                    }
                    Err(error) => {
                        overlay::hide();
                        if last_caret_error.as_deref() != Some(error.as_str()) {
                            eprintln!("Writely: {error}");
                            last_caret_error = Some(error);
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::IconClicked) => {
                handle_icon_clicked(
                    &watcher_proxy,
                    &mut generation,
                    &mut checking,
                    &mut pending,
                    &mut last_anchor,
                );
            }
            Event::UserEvent(UserEvent::ReplaceClicked) => {
                handle_replace_clicked(&mut pending, &mut last_anchor);
            }
            Event::UserEvent(UserEvent::Suggestion {
                generation: event_generation,
                result,
            }) => {
                handle_suggestion(
                    generation,
                    event_generation,
                    result,
                    &mut checking,
                    &mut pending,
                    &mut last_anchor,
                );
            }
            _ => {}
        }
    });
}
