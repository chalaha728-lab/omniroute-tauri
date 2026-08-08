//! Remote-server mode — Rust port of `showRemoteServerPrompt()` /
//! `setRemoteServerUrl()` / `resolveRemoteServerUrl()` and the
//! `lib/remoteServerPreferences.js` + `lib/resolveRemoteServerUrl.js`
//! helpers in the Electron build.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::runtime::Handle;

use crate::state::{self, SharedState};
use crate::tray;

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
struct Preferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_server_url: Option<String>,
}

pub fn resolve_remote_url(data_dir: &Path) -> Option<String> {
    if let Ok(env_url) = std::env::var("OMNIROUTE_REMOTE_URL") {
        let trimmed = env_url.trim();
        if !trimmed.is_empty() && is_valid_http_url(trimmed) {
            return Some(strip_trailing_slash(trimmed));
        }
    }
    let prefs_path = data_dir.join("electron-preferences.json");
    if !prefs_path.exists() {
        return None;
    }
    let content = match std::fs::read_to_string(&prefs_path) {
        Ok(c) => c,
        Err(_) => return None,
    };
    let parsed: Preferences = match serde_json::from_str(&content) {
        Ok(p) => p,
        Err(_) => return None,
    };
    if let Some(url) = parsed.remote_server_url {
        let trimmed = url.trim();
        if !trimmed.is_empty() && is_valid_http_url(trimmed) {
            return Some(strip_trailing_slash(trimmed));
        }
    }
    None
}

pub fn is_valid_http_url(s: &str) -> bool {
    let parsed = url::Url::parse(s);
    matches!(parsed, Ok(u) if u.scheme() == "http" || u.scheme() == "https")
}

fn strip_trailing_slash(s: &str) -> String {
    let mut out = s.to_string();
    while out.ends_with('/') {
        out.pop();
    }
    out
}

pub fn write_remote_server_url(data_dir: &Path, url: Option<&str>) {
    let prefs_path = data_dir.join("electron-preferences.json");
    let current = read_preferences(&prefs_path);
    let next = Preferences {
        remote_server_url: url.and_then(|s| {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }),
        ..current
    };
    if let Err(e) = std::fs::create_dir_all(data_dir) {
        log::error!("[remote_server] create_dir_all({}): {e}", data_dir.display());
        return;
    }
    match serde_json::to_string_pretty(&next) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&prefs_path, format!("{json}\n")) {
                log::error!("[remote_server] write {}: {e}", prefs_path.display());
            }
        }
        Err(e) => log::error!("[remote_server] serialize: {e}"),
    }
}

fn read_preferences(prefs_path: &Path) -> Preferences {
    if !prefs_path.exists() {
        return Preferences::default();
    }
    let content = match std::fs::read_to_string(prefs_path) {
        Ok(c) => c,
        Err(_) => return Preferences::default(),
    };
    serde_json::from_str(&content).unwrap_or_default()
}

// ── Prompt window ────────────────────────────────────────────────────────

pub fn show_remote_prompt(app: &AppHandle) {
    let label = "remote-server-prompt";
    if let Some(existing) = app.get_webview_window(label) {
        let _ = existing.show();
        let _ = existing.set_focus();
        return;
    }

    // Try parented first (modal on top of main window); fall back to
    // unparented on platforms that don't support `parent()`.
    let parented = app.get_webview_window("main").and_then(|main| {
        WebviewWindowBuilder::new(app, label, WebviewUrl::App("remoteServerPrompt.html".into()))
            .title("Connect to Remote Server")
            .inner_size(480.0, 210.0)
            .resizable(false)
            .minimizable(false)
            .maximizable(false)
            .fullscreen(false)
            .decorations(true)
            .visible(false)
            .parent(&main)
            .ok()?
            .build()
            .ok()
    });

    let window = match parented {
        Some(w) => w,
        None => match WebviewWindowBuilder::new(
            app,
            label,
            WebviewUrl::App("remoteServerPrompt.html".into()),
        )
        .title("Connect to Remote Server")
        .inner_size(480.0, 210.0)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .fullscreen(false)
        .decorations(true)
        .visible(false)
        .build()
        {
            Ok(w) => w,
            Err(e) => {
                log::error!("[OmniRoute] failed to open remote-server prompt: {e}");
                return;
            }
        },
    };

    let w = window;
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = w.show();
        let _ = w.set_focus();
    });
}

pub fn spawn_set_remote_url(app: AppHandle, url: Option<String>) {
    let state: State<'_, SharedState> = app.state();
    let state = state.inner().clone();
    Handle::current().spawn(async move {
        if let Err(e) = set_remote_url(&app, state.clone(), url).await {
            log::error!("[OmniRoute] set_remote_url failed: {e:#}");
        }
        let _ = tray::build_tray(&app, state);
    });
}

async fn set_remote_url(
    app: &AppHandle,
    state: SharedState,
    next_url: Option<String>,
) -> anyhow::Result<()> {
    let normalized = next_url
        .as_deref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if normalized.is_some() && !is_valid_http_url(normalized.as_deref().unwrap_or("")) {
        log::warn!("[OmniRoute] rejected invalid remote server URL: {normalized:?}");
        return Ok(());
    }

    let (current_url, port) = {
        let s = state::lock(&state);
        (s.remote_server_url.clone(), s.port)
    };
    if normalized == current_url {
        return Ok(());
    }

    crate::emit_server_status(app, "restarting", port, normalized.as_deref());

    let old_handle = state::lock(&state).server_handle.take();
    if let Some(mut old) = old_handle {
        let _ = old.kill_tree().await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), old.child.wait()).await;
    }

    let data_dir = state::lock(&state).data_dir.clone();
    write_remote_server_url(&data_dir, normalized.as_deref());
    state::lock(&state).remote_server_url = normalized.clone();

    if normalized.is_none() {
        let mut server_env = crate::env_bootstrap::bootstrap_server_env(&data_dir).await?;
        server_env.insert("PORT".into(), port.to_string());
        server_env.insert("NODE_ENV".into(), "production".into());
        server_env.insert("DATA_DIR".into(), data_dir.to_string_lossy().into());
        let handle = crate::server::spawn_server(app, &server_env).await?;
        state::lock(&state).server_handle = Some(handle);
        let url = format!("http://localhost:{port}/api/monitoring/health");
        let _ = crate::server::wait_for_server(&url, std::time::Duration::from_secs(180)).await;
    }

    crate::server::navigate_main_to_server(app, &state).await;
    crate::emit_server_status(app, "running", port, normalized.as_deref());
    log::info!(
        "[OmniRoute] remote-server mode: {}",
        normalized.as_deref().unwrap_or("(disconnected — using local server)")
    );
    Ok(())
}

// ── Prompt-window IPC commands ───────────────────────────────────────────

#[tauri::command]
pub async fn get_initial_url(state: State<'_, SharedState>) -> Result<String, String> {
    Ok(state::lock(state.inner())
        .remote_server_url
        .clone()
        .unwrap_or_default())
}

#[tauri::command]
pub async fn submit_remote_url(app: AppHandle, url: String) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("remote-server-prompt") {
        let _ = window.close();
    }
    spawn_set_remote_url(app, Some(url));
    Ok(())
}

#[tauri::command]
pub async fn cancel_remote_prompt(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("remote-server-prompt") {
        let _ = window.close();
    }
    Ok(())
}
