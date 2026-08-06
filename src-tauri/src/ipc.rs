//! IPC handlers — mirror `setupIpcHandlers()` in `electron/main.js` 1:1.
//!
//! Exposed commands (invoked from `src-tauri/preload-shim.ts` via `@tauri-apps/api/core`):
//!
//!   get_app_info          → { name, version, platform, isDev, port }
//!   open_external(url)    → void   (http/https only — RCE protection)
//!   get_data_dir          → string
//!   restart_server        → { success: boolean }
//!   get_app_version       → string
//!   check_for_updates     → { success: boolean }
//!   download_update       → { success: boolean }
//!   install_update        → void
//!   get_autostart_status  → boolean
//!   enable_autostart      → boolean
//!   disable_autostart     → boolean
//!   login_start(providerId, options) → { success, credentials?, error? }
//!   login_cancel          → { success: boolean }
//!   login_status          → { active: boolean }
//!   window_minimize       → void
//!   window_maximize       → void  (toggle)
//!   window_close          → void  (hide to tray)
//!
//! Events emitted to the renderer (consumed via `@tauri-apps/api/event` `listen`):
//!   server-status  → { status: "starting"|"running"|"stopped"|"restarting"|"error", port }
//!   port-changed   → number
//!   update-status  → { status, ... }
//!   login:status   → { providerId, status, message }

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;

use crate::login::LOGIN_MANAGER;
use crate::secrets::resolve_data_dir;
use crate::updater;

/// Helper: emit a server-status event to the main window.
pub fn emit_server_status(app: &AppHandle, status: &str, port: u16) -> tauri::Result<()> {
    app.emit(
        "server-status",
        json!({ "status": status, "port": port }),
    )
}

#[derive(serde::Serialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub is_dev: bool,
    pub port: u16,
}

pub fn register(_app: &AppHandle) -> tauri::Result<()> {
    // All IPC commands are registered via `tauri::generate_handler!` in
    // `lib.rs`. This function is kept as a no-op for future setup hooks.
    Ok(())
}

// Each command is registered via `tauri::generate_handler!` in lib.rs's run().
// We expose them as `#[tauri::command]` functions below.

#[tauri::command]
pub async fn get_app_info(app: AppHandle) -> Result<AppInfo, String> {
    let pkg = app.package_info();
    Ok(AppInfo {
        name: pkg.name.clone(),
        version: pkg.version.clone(),
        platform: std::env::consts::OS.into(),
        is_dev: cfg!(dev),
        port: *crate::SERVER_PORT.lock().unwrap(),
    })
}

#[tauri::command]
pub async fn open_external(app: AppHandle, url: String) -> Result<(), String> {
    // Validate URL protocol — same RCE guard as main.js
    let parsed = url::Url::parse(&url).map_err(|_| format!("Blocked invalid URL: {}", url))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("Blocked unsafe protocol: {}", parsed.scheme()));
    }
    app.shell()
        .open(url, None)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_data_dir(_app: AppHandle) -> Result<String, String> {
    Ok(resolve_data_dir().to_string_lossy().to_string())
}

#[tauri::command]
pub async fn restart_server(app: AppHandle) -> Result<Value, String> {
    let data_dir = resolve_data_dir();
    let port = *crate::SERVER_PORT.lock().unwrap();
    let success = crate::server::restart(&app, &data_dir, port).await;
    Ok(json!({ "success": success }))
}

#[tauri::command]
pub async fn get_app_version(app: AppHandle) -> Result<String, String> {
    Ok(app.package_info().version.clone())
}

#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> Result<Value, String> {
    match updater::check(&app, false).await {
        Ok(Some(status)) => Ok(json!({ "success": true, "status": status })),
        Ok(None) => Ok(json!({ "success": true, "status": { "status": "not-available" } })),
        Err(e) => {
            let _ = app.emit("update-status", json!({ "status": "error", "message": e }));
            Ok(json!({ "success": false, "error": e }))
        }
    }
}

#[tauri::command]
pub async fn download_update(app: AppHandle) -> Result<Value, String> {
    match updater::download(&app).await {
        Ok(_) => Ok(json!({ "success": true })),
        Err(e) => {
            let _ = app.emit("update-status", json!({ "status": "error", "message": e }));
            Ok(json!({ "success": false, "error": e }))
        }
    }
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    // Stop the sidecar before quitAndInstall — matches main.js
    crate::server::stop();
    crate::server::wait_exit(std::time::Duration::from_secs(5)).await;
    updater::install(&app).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_autostart_status(app: AppHandle) -> Result<bool, String> {
    let mgr = app.autolaunch();
    mgr.is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn enable_autostart(app: AppHandle) -> Result<bool, String> {
    let mgr = app.autolaunch();
    mgr.enable().map(|_| true).map_err(|e| {
        log::error!("[Tauri] Enable autostart failed: {}", e);
        e.to_string()
    })
}

#[tauri::command]
pub async fn disable_autostart(app: AppHandle) -> Result<bool, String> {
    let mgr = app.autolaunch();
    mgr.disable().map(|_| true).map_err(|e| {
        log::error!("[Tauri] Disable autostart failed: {}", e);
        e.to_string()
    })
}

#[tauri::command]
pub async fn login_start(
    app: AppHandle,
    provider_id: String,
    options: Option<Value>,
) -> Result<Value, String> {
    let mgr = LOGIN_MANAGER.clone();
    let result = mgr.start_login(&app, &provider_id, options).await;
    Ok(json!(result))
}

#[tauri::command]
pub async fn login_cancel(_app: AppHandle) -> Result<Value, String> {
    let mgr = LOGIN_MANAGER.clone();
    mgr.cancel().await;
    Ok(json!({ "success": true }))
}

#[tauri::command]
pub async fn login_status(_app: AppHandle) -> Result<Value, String> {
    let mgr = LOGIN_MANAGER.clone();
    let active = mgr.get_active_provider().await.is_some();
    Ok(json!({ "active": active }))
}

#[tauri::command]
pub fn window_minimize(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.minimize();
    }
    Ok(())
}

#[tauri::command]
pub fn window_maximize(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("main") {
        if w.is_maximized().unwrap_or(false) {
            let _ = w.unmaximize();
        } else {
            let _ = w.maximize();
        }
    }
    Ok(())
}

#[tauri::command]
pub fn window_close(app: AppHandle) -> Result<(), String> {
    // Mirrors preload's window-close → mainWindow.close() which triggers
    // the CloseRequested event, which hides to tray (see lib.rs).
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.close();
    }
    Ok(())
}
