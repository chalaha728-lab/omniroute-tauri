//! Tauri command handlers — Rust port of the `setupIpcHandlers()` block in
//! `electron/main.js` and the matching `electron/preload.js` channel
//! whitelist.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_shell::ShellExt;
use tokio::runtime::Handle;

use crate::state::{self, SharedState};
use crate::{AppInfo, UpdateStatus};

// ── App info / version ───────────────────────────────────────────────────

#[tauri::command]
pub async fn get_app_info(
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<AppInfo, String> {
    let s = state::lock(state.inner());
    let version = app.package_info().version.to_string();
    let name = app.package_info().name.clone();
    Ok(AppInfo {
        name,
        version,
        platform: std::env::consts::OS.to_string(),
        is_dev: s.is_dev,
        port: s.port,
        remote_server_url: s.remote_server_url.clone(),
    })
}

#[tauri::command]
pub fn get_app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

// ── open-external ────────────────────────────────────────────────────────

#[tauri::command]
pub async fn open_external(app: AppHandle, url: String) -> Result<(), String> {
    let parsed = url::Url::parse(&url).map_err(|_| format!("invalid URL: {url}"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        log::warn!("[OmniRoute] blocked unsafe protocol: {}", parsed.scheme());
        return Ok(());
    }
    #[allow(deprecated)]
    app.shell()
        .open(parsed.as_str(), None)
        .map_err(|e| e.to_string())
}

// ── Data dir ─────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_data_dir(state: State<'_, SharedState>) -> String {
    state::lock(state.inner())
        .data_dir
        .to_string_lossy()
        .to_string()
}

// ── Server lifecycle: restart ────────────────────────────────────────────

#[tauri::command]
pub async fn restart_server(
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let state = state.inner().clone();
    crate::server::restart_server(&app, state)
        .await
        .map_err(|e| e.to_string())
}

// ── Window controls ──────────────────────────────────────────────────────

#[tauri::command]
pub fn window_minimize(app: AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.minimize();
    }
}

#[tauri::command]
pub fn window_maximize(app: AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_maximized().unwrap_or(false) {
            let _ = window.unmaximize();
        } else {
            let _ = window.maximize();
        }
    }
}

#[tauri::command]
pub fn window_close(app: AppHandle, state: State<'_, SharedState>) {
    state::lock(state.inner()).mark_quitting();
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.close();
    }
}

// ── Auto-update ──────────────────────────────────────────────────────────

pub fn spawn_check_for_updates(app: AppHandle, silent: bool) {
    Handle::current().spawn(async move {
        let is_dev = state::lock(&crate::server::fetch_state(&app)).is_dev;
        if is_dev {
            log::info!("[OmniRoute] dev mode — skipping auto-update");
            if !silent {
                let _ = app.emit(
                    "update-status",
                    UpdateStatus {
                        status: "error".into(),
                        version: None,
                        percent: None,
                        transferred: None,
                        total: None,
                        message: Some("Updates disabled in dev mode".into()),
                    },
                );
            }
            return;
        }
        match try_check_updates(&app).await {
            Ok(true) => log::info!("[OmniRoute] update available"),
            Ok(false) => log::info!("[OmniRoute] no update available"),
            Err(e) => log::warn!("[OmniRoute] update check failed (non-fatal): {e}"),
        }
    });
}

async fn try_check_updates(app: &AppHandle) -> Result<bool, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater.check().await.map_err(|e| e.to_string())?;
    let Some(update) = update else {
        let _ = app.emit(
            "update-status",
            UpdateStatus {
                status: "not-available".into(),
                version: None,
                percent: None,
                transferred: None,
                total: None,
                message: None,
            },
        );
        return Ok(false);
    };
    let _ = app.emit(
        "update-status",
        UpdateStatus {
            status: "available".into(),
            version: Some(update.version.clone()),
            percent: None,
            transferred: None,
            total: None,
            message: None,
        },
    );
    Ok(true)
}

#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> Result<(), String> {
    spawn_check_for_updates(app, false);
    Ok(())
}

#[tauri::command]
pub async fn download_update(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater.check().await.map_err(|e| e.to_string())?;
    let Some(update) = update else {
        return Ok(());
    };
    let _ = app.emit(
        "update-status",
        UpdateStatus {
            status: "downloading".into(),
            version: Some(update.version.clone()),
            percent: Some(0),
            transferred: None,
            total: None,
            message: None,
        },
    );
    // `download_and_install` takes two callbacks: `on_download_progress` and
    // `on_exit`. The progress callback receives `(download_chunk_length,
    // total_content_length)`. We ignore both here and emit a single
    // "downloaded" event on completion.
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit(
        "update-status",
        UpdateStatus {
            status: "downloaded".into(),
            version: Some(update.version.clone()),
            percent: Some(100),
            transferred: None,
            total: None,
            message: None,
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    // Stop the embedded server before installing — mirrors the
    // `killProcessTree(nextServer)` block in `installUpdate()` (#3347).
    let old_handle = state::lock(state.inner()).server_handle.take();
    if let Some(mut old) = old_handle {
        let _ = old.kill_tree().await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), old.child.wait()).await;
    }
    // Tauri's `download_and_install` (called from `download_update`) already
    // installs the update; the app needs to restart to apply it. Tauri 2
    // exposes `AppHandle::restart` directly in the core.
    app.restart();
    Ok(())
}

// ── Autostart ────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_autostart_status(app: AppHandle) -> bool {
    #[cfg(target_os = "linux")]
    {
        if crate::autostart::is_linux_desktop_autostart_enabled() {
            return true;
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = &app;
    }
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
pub fn enable_autostart(app: AppHandle) -> bool {
    #[cfg(target_os = "linux")]
    {
        if crate::autostart::enable_linux_desktop_autostart() {
            return true;
        }
    }
    match app.autolaunch().enable() {
        Ok(_) => true,
        Err(e) => {
            log::error!("[OmniRoute] enable autostart failed: {e}");
            false
        }
    }
}

#[tauri::command]
pub fn disable_autostart(app: AppHandle) -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::autostart::disable_linux_desktop_autostart();
    }
    match app.autolaunch().disable() {
        Ok(_) => true,
        Err(e) => {
            log::error!("[OmniRoute] disable autostart failed: {e}");
            false
        }
    }
}

// ── Web-cookie login ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct LoginResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials: Option<serde_json::Value>,
}

#[tauri::command]
pub async fn start_login(
    provider_id: String,
    _options: Option<serde_json::Value>,
) -> Result<LoginResult, String> {
    log::warn!(
        "[OmniRoute] start_login({provider_id}) — web-cookie login is not yet implemented in the Tauri port"
    );
    Ok(LoginResult {
        success: false,
        error: Some(format!(
            "Web-cookie login for '{provider_id}' is not yet implemented in the Tauri desktop shell. \
             Use the Next.js dashboard's server-side OAuth flow instead, or run the Electron build \
             to perform this login once."
        )),
        credentials: None,
    })
}

#[tauri::command]
pub async fn cancel_login() -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub async fn get_login_status() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({ "active": false }))
}
