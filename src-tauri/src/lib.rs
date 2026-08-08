//! OmniRoute Desktop — Tauri edition.
//!
//! 1:1 port of `electron/main.js` + `electron/preload.js` to Tauri 2.

mod autostart;
mod env_bootstrap;
mod remote_server;
mod server;
mod state;
mod tray;
mod window;

use std::sync::Arc;

use anyhow::Context;
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use tokio::runtime::Handle;

use crate::state::{initial_state, AppState, SharedState};

#[derive(Clone, Debug, Serialize)]
pub struct ServerStatus {
    pub status: String,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PortChanged {
    pub port: u16,
}

#[derive(Clone, Debug, Serialize)]
pub struct UpdateStatus {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transferred: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub is_dev: bool,
    pub port: u16,
    pub remote_server_url: Option<String>,
}

/// The preload shim — a JS string that exposes `window.electronAPI` (and
/// `window.remoteServerPrompt`) with the exact same shape as
/// `electron/preload.js` + `electron/remoteServerPromptPreload.js`.
const PRELOAD_SHIM: &str = include_str!("preload_shim.js");

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    // Install a panic hook that logs the panic instead of letting it crash
    // the process silently. This turns the 0xC0000409 (STACK_BUFFER_OVERRUN)
    // crash into a log entry we can diagnose, and keeps the process alive
    // where possible.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        log::error!("[OmniRoute PANIC] at {location}: {payload}");
        // Also call the default hook so we still get a backtrace on stderr
        // if RUST_BACKTRACE=1.
        default_hook(info);
    }));

    let preload_shim = PRELOAD_SHIM.to_string();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Mirrors `app.on("second-instance", …)` in Electron main.js.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .setup(move |app| {
            let handle = app.handle().clone();

            // ---- Shared state -------------------------------------------------
            let state: SharedState = Arc::new(Mutex::new(initial_state()));
            app.manage(state.clone());

            // ---- Resolve dev / remote URL -------------------------------------
            let is_dev = cfg!(debug_assertions)
                || std::env::var("NODE_ENV").as_deref() == Ok("development");
            let data_dir = state::lock(&state).data_dir.clone();
            let remote_server_url = remote_server::resolve_remote_url(&data_dir);
            {
                let mut s = state::lock(&state);
                s.is_dev = is_dev;
                s.data_dir = data_dir.clone();
                s.remote_server_url = remote_server_url.clone();
            }

            log::info!(
                "[OmniRoute] starting in {} mode, data_dir={}, remote={:?}",
                if is_dev { "dev" } else { "production" },
                data_dir.display(),
                remote_server_url
            );

            // ---- Inject the preload shim into the main window -----------------
            // NB: on Windows + WebView2, calling eval() before the webview has
            // finished initializing can trigger STATUS_STACK_BUFFER_OVERRUN
            // (0xC0000409). We defer the eval to a `PageLoaded` event handler
            // so the shim only runs once the placeholder HTML has loaded and
            // the JS context is ready.
            if let Some(main_window) = app.get_webview_window("main") {
                #[cfg(target_os = "macos")]
                {
                    use tauri::TitleBarStyle;
                    let _ = main_window.set_title_bar_style(TitleBarStyle::Overlay);
                }

                // Inject the preload shim on every page load (placeholder +
                // after navigation to the Next.js server). Using on_page_load
                // avoids the WebView2-before-ready crash.
                let shim = preload_shim.clone();
                main_window.on_page_load(move |_webview, _payload| {
                    // eval() is safe here — the page is loaded.
                    if let Err(e) = _webview.eval(&shim) {
                        log::warn!("[OmniRoute] failed to inject preload shim: {e}");
                    }
                });

                // Show the window once the placeholder page has loaded,
                // unless --hidden / --minimized was passed (tray-only launch).
                let hidden_requested =
                    std::env::args().any(|a| a == "--hidden" || a == "--minimized");
                if !hidden_requested {
                    let _ = main_window.show();
                    let _ = main_window.set_focus();
                } else {
                    log::info!("[OmniRoute] launched hidden in tray");
                }
            }

            // ---- Server lifecycle: spawn sidecar, wait for ready, navigate ----
            let handle_for_server = handle.clone();
            let state_for_server = state.clone();
            Handle::current().spawn(async move {
                if let Err(err) =
                    server::startup_sequence(&handle_for_server, state_for_server.clone()).await
                {
                    log::error!("[OmniRoute] server startup sequence failed: {err:#}");
                    let port = state::lock(&state_for_server).port;
                    let _ = handle_for_server.emit(
                        "server-status",
                        ServerStatus {
                            status: "error".into(),
                            port,
                            remote_url: None,
                        },
                    );
                }
            });

            // ---- Build the tray (always — even in dev, mirrors Electron) ------
            tray::build_tray(&handle, state.clone())?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            window::get_app_info,
            window::open_external,
            window::get_data_dir,
            window::restart_server,
            window::get_app_version,
            window::window_minimize,
            window::window_maximize,
            window::window_close,
            window::check_for_updates,
            window::download_update,
            window::install_update,
            window::get_autostart_status,
            window::enable_autostart,
            window::disable_autostart,
            window::start_login,
            window::cancel_login,
            window::get_login_status,
            remote_server::get_initial_url,
            remote_server::submit_remote_url,
            remote_server::cancel_remote_prompt,
        ])
        .on_window_event(|window, event| {
            // Mirror Electron's "close → hide to tray" behavior on the main
            // window. The remote-server-prompt window closes normally.
            if window.label() == "main" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    let app = window.app_handle();
                    let state: tauri::State<'_, Arc<Mutex<AppState>>> = app.state();
                    let quitting = state::lock(state.inner()).is_quitting;
                    if !quitting {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            }
        })
        .build(tauri::generate_context!())
        .context("error while building tauri application")
        .expect("failed to build tauri application")
        .run(|app, event| {
            // Mirror Electron's `before-quit` handler: stop the embedded server
            // and wait up to 5s for graceful WAL checkpoint before letting the
            // process exit.
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let state: tauri::State<'_, Arc<Mutex<AppState>>> = app.state();
                let server_handle = state::lock(state.inner()).server_handle.take();
                if let Some(handle) = server_handle {
                    let app = app.clone();
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .ok();
                        if let Some(rt) = rt {
                            rt.block_on(async {
                                let _ = server::stop_server(&app, handle).await;
                            });
                        }
                        app.cleanup_before_exit();
                    });
                }
            }
        });
}

/// Convenience: emit a `server-status` event to the main window.
pub fn emit_server_status(app: &AppHandle, status: &str, port: u16, remote_url: Option<&str>) {
    let payload = ServerStatus {
        status: status.into(),
        port,
        remote_url: remote_url.map(|s| s.to_string()),
    };
    let _ = app.emit("server-status", payload);
}

/// Convenience: emit a `port-changed` event to the main window.
pub fn emit_port_changed(app: &AppHandle, port: u16) {
    let _ = app.emit("port-changed", PortChanged { port });
}
