#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gemini;
mod overlay;
mod settings;
#[allow(dead_code)]
mod text_access;
mod watcher;

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Debug)]
pub enum UserEvent {
    MenuEvent(MenuEvent),
    CaretChanged,
    IconClicked,
}

fn main() {
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }

    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        if let Err(error) = proxy.send_event(UserEvent::MenuEvent(event)) {
            eprintln!("Writely: could not deliver the tray menu event: {error}");
        }
    }));
    let watcher_proxy = event_loop.create_proxy();
    overlay::init(watcher_proxy.clone());

    let mut tray_icon: Option<TrayIcon> = None;
    let mut settings_id = tray_icon::menu::MenuId::new("settings");
    let mut quit_id = tray_icon::menu::MenuId::new("quit");
    let mut caret_watcher: Option<watcher::Watcher> = None;
    let mut last_caret_error: Option<String> = None;

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
            Event::UserEvent(UserEvent::IconClicked) => {}
            _ => {}
        }
    });
}
