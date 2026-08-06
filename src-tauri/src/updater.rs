//! Auto-updater — port of the `autoUpdater` event handlers in `electron/main.js`.
//!
//! Uses `tauri-plugin-updater` (Tauri 2.x). The GitHub releases feed is
//! configured in `tauri.conf.json` → `plugins.updater.endpoints`.
//!
//! Status events are emitted to the renderer as `update-status`:
//!   { status: "checking"|"available"|"not-available"|"downloading"|"downloaded"|"error", ... }

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;

pub struct UpdateStatus(pub Value);

/// Check for an available update. Returns `Some(status_payload)` if there is
/// one, `None` if up-to-date.
pub async fn check(app: &AppHandle, silent: bool) -> Result<Option<Value>, String> {
    if cfg!(dev) {
        log::info!("[Tauri] Dev mode — skipping auto-update");
        if !silent {
            let _ = app.emit("update-status", json!({ "status": "error", "message": "Updates disabled in dev mode" }));
        }
        return Ok(None);
    }

    let _ = app.emit("update-status", json!({ "status": "checking" }));
    log::info!("[Tauri] Checking for updates...");

    let updater = app
        .updater()
        .map_err(|e| format!("updater builder failed: {}", e))?;

    let update = match updater.check().await {
        Ok(Some(u)) => u,
        Ok(None) => {
            let _ = app.emit("update-status", json!({ "status": "not-available" }));
            log::info!("[Tauri] No update available");
            return Ok(None);
        }
        Err(e) => {
            let msg = e.to_string();
            let _ = app.emit("update-status", json!({ "status": "error", "message": msg }));
            log::warn!("[Tauri] Update check failed (non-fatal): {}", msg);
            return Err(msg);
        }
    };

    let version = update.version.clone();
    let _ = app.emit("update-status", json!({ "status": "available", "version": version.clone() }));
    log::info!("[Tauri] Update available: {}", version);

    Ok(Some(json!({ "status": "available", "version": version })))
}

/// Download the pending update. Emits `download-progress` events.
pub async fn download(app: &AppHandle) -> Result<(), String> {
    let updater = app
        .updater()
        .map_err(|e| format!("updater builder failed: {}", e))?;

    let update = updater
        .check()
        .await
        .map_err(|e| format!("check failed: {}", e))?
        .ok_or("no update available")?;

    let total = update.content_length.unwrap_or(0);
    let mut downloaded: u64 = 0;
    let app_clone = app.clone();

    update
        .download(
            move |chunk, _content_length| {
                downloaded += chunk.len() as u64;
                let percent = if total > 0 {
                    (downloaded as f64 / total as f64 * 100.0) as u32
                } else {
                    0
                };
                let _ = app_clone.emit(
                    "update-status",
                    json!({
                        "status": "downloading",
                        "percent": percent,
                        "transferred": downloaded,
                        "total": total,
                    }),
                );
            },
            |download_path| {
                log::info!("[Tauri] Update downloaded to: {:?}", download_path);
            },
        )
        .await
        .map_err(|e| format!("download failed: {}", e))?;

    let _ = app.emit("update-status", json!({ "status": "downloaded" }));
    Ok(())
}

/// Install the downloaded update (quits and restarts).
pub async fn install(app: &AppHandle) -> Result<(), String> {
    let updater = app
        .updater()
        .map_err(|e| format!("updater builder failed: {}", e))?;

    let update = updater
        .check()
        .await
        .map_err(|e| format!("check failed: {}", e))?
        .ok_or("no update available")?;

    update
        .install_and_restart()
        .await
        .map_err(|e| format!("install failed: {}", e))
}
