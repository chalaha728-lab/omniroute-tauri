//! System tray — Rust port of `createTray()` in `electron/main.js`.
//!
//! Builds the OmniRoute tray menu with: Open / Open Dashboard / Server Port
//! selector / Remote Server submenu / Check for Updates / Quit. The tray
//! persists for the lifetime of the app (mirrors Electron behavior — on
//! close, the window hides to tray rather than quitting).

use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, State,
};
use tauri_plugin_shell::ShellExt;

use crate::state::{self, SharedState};

/// Build (or rebuild) the OmniRoute tray icon + menu. Idempotent — Tauri
/// replaces any existing tray with the same id when `with_id` is used, so
/// calling this on a port/remote-server change refreshes the menu safely
/// (mirrors the "Fix #4: Destroy old tray before recreating" guard in
/// `electron/main.js`).
pub fn build_tray(app: &AppHandle, state: SharedState) -> tauri::Result<()> {
    let (port, remote_url) = {
        let s = state::lock(&state);
        (s.port, s.remote_server_url.clone())
    };

    // ---- Menu items ----------------------------------------------------
    let open = MenuItem::with_id(app, "open", "Open OmniRoute", true, None::<&str>)?;
    let open_dashboard =
        MenuItem::with_id(app, "open_dashboard", "Open Dashboard", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;

    let port_label =
        MenuItem::with_id(app, "port_current", &format!("Port: {port}"), false, None::<&str>)?;
    let sep_port = PredefinedMenuItem::separator(app)?;
    let port_20128 = MenuItem::with_id(app, "port_20128", "20128", true, None::<&str>)?;
    let port_3000 = MenuItem::with_id(app, "port_3000", "3000", true, None::<&str>)?;
    let port_8080 = MenuItem::with_id(app, "port_8080", "8080", true, None::<&str>)?;
    let port_submenu = Submenu::with_items(
        app,
        "Server Port",
        true,
        &[&port_label, &sep_port, &port_20128, &port_3000, &port_8080],
    )?;

    let remote_label_text = if let Some(url) = &remote_url {
        format!("Connected: {url}")
    } else {
        "Using local embedded server".to_string()
    };
    let remote_label =
        MenuItem::with_id(app, "remote_label", &remote_label_text, false, None::<&str>)?;
    let sep_remote = PredefinedMenuItem::separator(app)?;
    let remote_connect = MenuItem::with_id(
        app,
        "remote_connect",
        "Connect to Remote Server…",
        true,
        None::<&str>,
    )?;
    let remote_disconnect = MenuItem::with_id(
        app,
        "remote_disconnect",
        "Disconnect (use Local Server)",
        remote_url.is_some(),
        None::<&str>,
    )?;
    let remote_submenu = Submenu::with_items(
        app,
        "Remote Server",
        true,
        &[&remote_label, &sep_remote, &remote_connect, &remote_disconnect],
    )?;

    let sep2 = PredefinedMenuItem::separator(app)?;
    let check_updates =
        MenuItem::with_id(app, "check_updates", "Check for Updates", true, None::<&str>)?;
    let sep3 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit OmniRoute", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &open,
            &open_dashboard,
            &sep1,
            &port_submenu,
            &remote_submenu,
            &sep2,
            &check_updates,
            &sep3,
            &quit,
        ],
    )?;

    // ---- Tray icon -----------------------------------------------------
    let icon = load_tray_icon(app);

    let _tray = TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        .tooltip("OmniRoute")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            // Double-click → show & focus main window (mirrors
            // `tray.on("double-click", …)` in main.js)
            if let TrayIconEvent::DoubleClick { .. } = event {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "open_dashboard" => {
                let state: State<'_, SharedState> = app.state();
                let s = state::lock(state.inner());
                let url = if let Some(remote) = &s.remote_server_url {
                    remote.clone()
                } else {
                    format!("http://localhost:{}", s.port)
                };
                drop(s);
                let _ = app.shell().open(url, None);
            }
            "port_20128" => spawn_change_port(app.clone(), 20128),
            "port_3000" => spawn_change_port(app.clone(), 3000),
            "port_8080" => spawn_change_port(app.clone(), 8080),
            "remote_connect" => crate::remote_server::show_remote_prompt(app),
            "remote_disconnect" => {
                crate::remote_server::spawn_set_remote_url(app.clone(), None);
            }
            "check_updates" => crate::window::spawn_check_for_updates(app.clone(), false),
            "quit" => {
                let state: State<'_, SharedState> = app.state();
                state::lock(state.inner()).mark_quitting();
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}

/// Spawn an async task to switch the embedded server to a new port.
/// Mirrors `changePort()` in `electron/main.js`.
fn spawn_change_port(app: AppHandle, new_port: u16) {
    let state: State<'_, SharedState> = app.state();
    let state = state.inner().clone();
    tokio::spawn(async move {
        if let Err(err) = crate::server::change_port(&app, state.clone(), new_port).await {
            log::error!("[OmniRoute] change_port failed: {err:#}");
        }
        // Rebuild tray so the "Port: N" label reflects the new port.
        let _ = build_tray(&app, state);
    });
}

/// Load the tray icon from the bundled resource. Falls back to the embedded
/// `icons/tray-icon.png` bytes if the resource path can't be resolved
/// (mirrors the `icon.isEmpty()` fallback in `electron/main.js`).
fn load_tray_icon(app: &AppHandle) -> Image<'_> {
    let path = app
        .path()
        .resource_dir()
        .ok()
        .map(|d| d.join("icons").join("tray-icon.png"));
    if let Some(p) = &path {
        if let Ok(img) = Image::from_path(p) {
            return img;
        }
    }
    Image::from_bytes(include_bytes!("../icons/tray-icon.png"))
        .unwrap_or_else(|_| Image::from_bytes(&[0u8; 1]).unwrap())
}
