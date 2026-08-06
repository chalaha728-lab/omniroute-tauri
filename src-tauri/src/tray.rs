//! System tray — mirrors `createTray()` in `electron/main.js`.
//!
//! Menu:
//!   - Open OmniRoute
//!   - Open Dashboard (in default browser)
//!   - ─────
//!   - Server Port → 20128 / 3000 / 8080
//!   - ─────
//!   - Check for Updates
//!   - ─────
//!   - Quit

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    // Remove the previous tray if any (mirrors `if (tray) tray.destroy()`).
    if let Some(existing) = app.tray_by_id("main") {
        let _ = existing.close();
    }

    let port = *crate::SERVER_PORT.lock().unwrap();

    let open = MenuItem::with_id(app, "open", "Open OmniRoute", true, None::<&str>)?;
    let open_dashboard = MenuItem::with_id(app, "open_dashboard", "Open Dashboard", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let port_label = MenuItem::with_id(app, "port_label", &format!("Port: {}", port), false, None::<&str>)?;
    let sep_p = PredefinedMenuItem::separator(app)?;
    let port_20128 = MenuItem::with_id(app, "port_20128", "20128", true, None::<&str>)?;
    let port_3000 = MenuItem::with_id(app, "port_3000", "3000", true, None::<&str>)?;
    let port_8080 = MenuItem::with_id(app, "port_8080", "8080", true, None::<&str>)?;
    let port_submenu = Submenu::with_items(
        app,
        "Server Port",
        true,
        &[&port_label, &sep_p, &port_20128, &port_3000, &port_8080],
    )?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let check_updates = MenuItem::with_id(app, "check_updates", "Check for Updates", true, None::<&str>)?;
    let sep3 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &open,
            &open_dashboard,
            &sep1,
            &port_submenu,
            &sep2,
            &check_updates,
            &sep3,
            &quit,
        ],
    )?;

    let _tray = TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("OmniRoute")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "open_dashboard" => {
                let port = *crate::SERVER_PORT.lock().unwrap();
                let url = format!("http://localhost:{}", port);
                let _ = tauri_plugin_shell::ShellExt::shell(app).open(url, None);
            }
            "port_20128" => change_port(app, 20128),
            "port_3000" => change_port(app, 3000),
            "port_8080" => change_port(app, 8080),
            "check_updates" => {
                let ah = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = crate::updater::check(&ah, false).await;
                });
            }
            "quit" => {
                crate::IS_QUITTING.store(true, std::sync::atomic::Ordering::SeqCst);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Double-click → show window (mirrors tray.on("double-click", ...))
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .build(app)?;

    Ok(())
}

fn change_port(app: &AppHandle, new_port: u16) {
    let data_dir = crate::secrets::resolve_data_dir();
    let ah = app.clone();
    tauri::async_runtime::spawn(async move {
        crate::server::change_port(&ah, &data_dir, new_port).await;
    });
}
