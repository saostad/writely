#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gemini;
mod settings;

use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};

fn main() {
    let app = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            settings::save_api_key,
            settings::remove_api_key,
            settings::has_api_key,
            settings::open_apikey_page
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let settings_item = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)
                .map_err(|e| format!("Could not create the Settings menu item: {e}"))?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit Writely", true, None::<&str>)
                .map_err(|e| format!("Could not create the Quit menu item: {e}"))?;
            let menu = Menu::with_items(app, &[&settings_item, &quit_item])
                .map_err(|e| format!("Could not create the tray menu: {e}"))?;

            let icon = match app.default_window_icon().cloned() {
                Some(icon) => icon,
                None => {
                    eprintln!(
                        "Writely: could not create the tray icon: no default window icon is configured"
                    );
                    std::process::exit(1);
                }
            };

            if let Err(e) = TrayIconBuilder::new()
                .icon(icon)
                .menu(&menu)
                .tooltip("Writely")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "settings" => {
                        if let Err(e) = settings::show_settings_window(app) {
                            eprintln!("Writely: {e}");
                        }
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)
            {
                eprintln!("Writely: could not create the tray icon: {e}");
                std::process::exit(1);
            }
            Ok(())
        })
        .build(tauri::generate_context!());

    let app = match app {
        Ok(app) => app,
        Err(e) => {
            eprintln!("Writely: could not start the application: {e}");
            std::process::exit(1);
        }
    };

    app.run(|_app_handle, event| {
        if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
            if code.is_none() {
                api.prevent_exit();
            }
        }
    });
}
