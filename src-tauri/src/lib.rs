//! OmniRoute Desktop — Tauri 2.x shell
//!
//! Drop-in replacement for `electron/main.js`. Same responsibilities:
//!   - Spawn & supervise the Next.js standalone server (Node sidecar)
//!   - Bootstrap secrets (JWT_SECRET / STORAGE_ENCRYPTION_KEY / API_KEY_SECRET)
//!   - Wait for server readiness before showing the window
//!   - System tray with port change + check-for-updates + quit
//!   - IPC handlers that mirror `electron/preload.js` 1:1
//!   - Web-cookie LoginManager (port of `electron/loginManager.js`)
//!   - Auto-update via tauri-plugin-updater (GitHub releases)
//!   - Autostart via tauri-plugin-autostart
//!   - Single instance lock
//!
//! The renderer-side contract (`window.electronAPI`) is preserved by a small
//! TypeScript preload shim — see `src-tauri/preload-shim.ts`. The existing
//! React hooks in `src/shared/hooks/useElectron.ts` are NOT modified.

pub mod autostart;
pub mod csp;
pub mod ipc;
pub mod login;
pub mod secrets;
pub mod server;
pub mod tray;
pub mod updater;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use once_cell::sync::Lazy;
use tauri::{Manager, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;

/// Whether the app is currently quitting (mirrors `app.isQuitting` in main.js).
pub static IS_QUITTING: AtomicBool = AtomicBool::new(false);

/// Whether the user passed `--headless` / `--cli` or set OMNIROUTE_HEADLESS=true.
pub static IS_HEADLESS: AtomicBool = AtomicBool::new(false);

/// Currently active server port (defaults to 20128, like the Electron build).
pub static SERVER_PORT: Lazy<Mutex<u16>> = Lazy::new(|| Mutex::new(20128));

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    // Parse CLI args once: --headless / --cli / --hidden / --minimized
    let args: Vec<String> = std::env::args().collect();
    let headless = args.iter().any(|a| a == "--headless" || a == "--cli")
        || std::env::var("OMNIROUTE_HEADLESS").ok().as_deref() == Some("true");
    IS_HEADLESS.store(headless, Ordering::SeqCst);

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Mirrors `app.on("second-instance", ...)` in main.js
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            // Single-instance lock is enforced by the plugin above.

            // Resolve config paths up-front so spawned processes can use them.
            let resource_dir = app.path().resource_dir()?;
            log::info!("[Tauri] resource_dir = {}", resource_dir.display());

            // Bootstrap state shared across modules.
            *SERVER_PORT.lock().unwrap() = std::env::var("OMNIROUTE_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(20128);

            // 1. Auto-generate JWT_SECRET / STORAGE_ENCRYPTION_KEY / API_KEY_SECRET
            //    (mirrors the bootstrap block in main.js)
            let data_dir = secrets::resolve_data_dir();
            let bootstrap = secrets::bootstrap(&data_dir)?;
            if bootstrap.changed {
                log::info!("[Tauri] ✨ secrets persisted to {}/server.env", data_dir.display());
            }

            // 2. Start the Next.js sidecar (or assume dev server is already running)
            let port = *SERVER_PORT.lock().unwrap();
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if cfg!(dev) {
                    log::info!("[Tauri] dev mode — connecting to existing Next.js dev server on port {}", port);
                    let _ = ipc::emit_server_status(&app_handle, "running", port);
                    return;
                }
                server::start(&app_handle, &data_dir, port).await;
                // Wait for readiness before showing window
                let url = format!("http://localhost:{}/api/monitoring/health", port);
                let ready = server::wait_for_health(&url, std::time::Duration::from_secs(180)).await;
                if !ready {
                    log::warn!("[Tauri] server readiness timeout — showing window anyway");
                }
                if let Some(window) = app_handle.get_webview_window("main") {
                    if !IS_HEADLESS.load(Ordering::SeqCst) {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            });

            // 3. Window + tray (unless headless)
            if !IS_HEADLESS.load(Ordering::SeqCst) {
                // Show once `ready-to-show` fires (Tauri emits this on first paint).
                if let Some(window) = app.get_webview_window("main") {
                    let w = window.clone();
                    window.on_window_event(move |event| {
                        if let WindowEvent::Ready = event {
                            let hidden = std::env::args().any(|a| a == "--hidden" || a == "--minimized");
                            if !hidden {
                                let _ = w.show();
                                let _ = w.set_focus();
                            } else {
                                log::info!("[Tauri] Launched hidden in background tray");
                            }
                        }
                    });
                }
                tray::create(app.handle())?;
            }

            // 4. CSP — applied on every navigation via the navigation event
            csp::install(app.handle())?;

            // 5. IPC handlers — mirror electron/preload.js
            //    All commands are registered via `generate_handler!` below.

            // 6. Auto-updater — non-blocking check after 3s
            let ah = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                if !cfg!(dev) {
                    let _ = updater::check(&ah, true).await;
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::get_app_info,
            ipc::open_external,
            ipc::get_data_dir,
            ipc::restart_server,
            ipc::get_app_version,
            ipc::check_for_updates,
            ipc::download_update,
            ipc::install_update,
            ipc::get_autostart_status,
            ipc::enable_autostart,
            ipc::disable_autostart,
            ipc::login_start,
            ipc::login_cancel,
            ipc::login_status,
            ipc::window_minimize,
            ipc::window_maximize,
            ipc::window_close,
        ])
        .on_window_event(move |window, event| {
            // Mirrors `mainWindow.on("close", ...)` in main.js — minimize to tray
            // instead of quitting, unless `IS_QUITTING` is set.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if !IS_QUITTING.load(Ordering::SeqCst) && window.label() == "main" {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
