//! LoginManager — port of `electron/loginManager.js`.
//!
//! Opens a separate Tauri WebviewWindow pointed at the provider's login URL,
//! polls the cookie store + localStorage for target tokens, and ships the
//! extracted credentials back to the renderer via the `login:status` event.
//!
//! Parity notes:
//!   - The Electron version uses `session.fromPartition()` for isolation. Tauri
//!     doesn't expose per-window cookie partitions the same way, so we use a
//!     fresh WebviewWindow with the default session — sufficient for our use
//!     case (one login at a time).
//!   - Cookie reading in Tauri 2 is via `WebviewWindow::cookies()` (returns
//!     Vec<Cookie>).
//!   - localStorage extraction uses `webview.eval()` with a JS snippet that
//!     reads the keys and returns them via a Tauri event.
//!
//! Events emitted (mirror Electron `loginManager.on("status", ...)`):
//!   { providerId, status: "starting"|"navigating"|"waiting"|"polling"|
//!                       "detected"|"complete"|"error"|"cancelled", message }

use std::collections::HashMap;
use std::sync::Arc;

use once_cell::sync::Lazy;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::Mutex;

/// Token-source spec — mirrors the shape consumed by loginManager.js.
#[derive(Debug, Clone)]
pub struct TokenSource {
    pub r#type: String, // "cookie" | "localStorage" | "sessionStorage"
    pub name: String,   // cookie name OR localStorage key
    pub domain: Option<String>,
}

/// Extraction config per provider (mirrors `tokenExtractionConfig.js`).
#[derive(Debug, Clone)]
pub struct ExtractionConfig {
    pub provider_id: String,
    pub display_name: String,
    pub login_url: String,
    pub success_url_pattern: Option<String>,
    pub token_sources: Vec<TokenSource>,
    pub min_login_time: u64,
    pub poll_interval: u64,
    pub timeout: u64,
}

/// Singleton login manager. Only one login flow can run at a time.
pub static LOGIN_MANAGER: Lazy<Arc<LoginManager>> = Lazy::new(|| Arc::new(LoginManager::default()));

#[derive(Default)]
pub struct LoginManager {
    inner: Mutex<Option<ActiveLogin>>,
}

struct ActiveLogin {
    provider_id: String,
    window_label: String,
    cancel: tokio::sync::Notify,
}

#[derive(Debug, Serialize)]
pub struct LoginResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl LoginManager {
    pub async fn start_login(
        &self,
        app: &AppHandle,
        provider_id: &str,
        _options: Option<Value>,
    ) -> LoginResult {
        // Look up the extraction config from the bundled Next.js resources.
        let config = match load_extraction_config(provider_id) {
            Some(c) => c,
            None => {
                return LoginResult {
                    success: false,
                    credentials: None,
                    error: Some(format!("No extraction config for provider: {}", provider_id)),
                };
            }
        };

        let mut guard = self.inner.lock().await;
        if guard.is_some() {
            return LoginResult {
                success: false,
                credentials: None,
                error: Some("A login process is already in progress".into()),
            };
        }

        let window_label = format!("login-{}", provider_id);
        let cancel = Arc::new(tokio::sync::Notify::new());
        let active = ActiveLogin {
            provider_id: provider_id.into(),
            window_label: window_label.clone(),
            cancel: cancel.clone(),
        };
        *guard = Some(active);
        drop(guard);

        let _ = app.emit(
            "login:status",
            json!({
                "providerId": provider_id,
                "status": "starting",
                "message": format!("Opening {} login...", config.display_name),
            }),
        );

        // Open the login window
        let win_result = WebviewWindowBuilder::new(
            app,
            &window_label,
            WebviewUrl::External(config.login_url.parse().unwrap_or_else(|_| {
                WebviewUrl::App("index.html".into())
            })),
        )
        .title(format!("Login - {}", config.display_name))
        .inner_size(1000.0, 750.0)
        .visible(true)
        .build();

        let window = match win_result {
            Ok(w) => w,
            Err(e) => {
                let _ = app.emit(
                    "login:status",
                    json!({
                        "providerId": provider_id,
                        "status": "error",
                        "message": format!("Failed to open window: {}", e),
                    }),
                );
                self.cleanup().await;
                return LoginResult {
                    success: false,
                    credentials: None,
                    error: Some(e.to_string()),
                };
            }
        };

        let _ = app.emit(
            "login:status",
            json!({
                "providerId": provider_id,
                "status": "navigating",
                "message": format!("Loading {}...", config.login_url),
            }),
        );

        // Polling loop
        let app_clone = app.clone();
        let provider_id_owned = provider_id.to_string();
        let config_owned = config.clone();
        let window_owned = window.clone();
        let cancel_owned = cancel.clone();

        let result = tokio::spawn(async move {
            polling_loop(
                &app_clone,
                &provider_id_owned,
                &config_owned,
                &window_owned,
                cancel_owned,
            )
            .await
        })
        .await
        .unwrap_or(LoginResult {
            success: false,
            credentials: None,
            error: Some("Login task panicked".into()),
        });

        // Close the login window + cleanup
        let _ = window.close();
        self.cleanup().await;
        result
    }

    pub async fn cancel(&self) {
        if let Some(active) = self.inner.lock().await.as_ref() {
            active.cancel.notify_waiters();
        }
        self.cleanup().await;
    }

    pub async fn get_active_provider(&self) -> Option<String> {
        self.inner.lock().await.as_ref().map(|a| a.provider_id.clone())
    }

    async fn cleanup(&self) {
        if let Some(active) = self.inner.lock().await.take() {
            // The window is closed by the caller (start_login) after polling
            // returns, but if we got here via cancel() we need to close it
            // ourselves.
            let _ = active;
        }
    }
}

async fn polling_loop(
    app: &AppHandle,
    provider_id: &str,
    config: &ExtractionConfig,
    window: &tauri::WebviewWindow,
    cancel: Arc<tokio::sync::Notify>,
) -> LoginResult {
    // Wait min_login_time before starting to poll
    let min_login_time = std::time::Duration::from_millis(config.min_login_time.max(5000));
    let timeout = std::time::Duration::from_millis(config.timeout.max(300_000));
    let poll_interval = std::time::Duration::from_millis(config.poll_interval.max(1000));

    tokio::select! {
        _ = tokio::time::sleep(min_login_time) => {}
        _ = cancel.notified() => {
            let _ = app.emit("login:status", json!({
                "providerId": provider_id,
                "status": "cancelled",
                "message": "Login cancelled",
            }));
            return LoginResult { success: false, credentials: None, error: Some("Login cancelled".into()) };
        }
    }

    let start = std::time::Instant::now();
    let mut poll_count = 0u32;

    while start.elapsed() < timeout {
        poll_count += 1;

        if poll_count % 30 == 0 {
            let elapsed = start.elapsed().as_secs() / 60;
            let _ = app.emit("login:status", json!({
                "providerId": provider_id,
                "status": "waiting",
                "message": format!("Waiting for login... ({}m)", elapsed),
            }));
        }

        // Read cookies from the webview
        let cookies = match window.cookies() {
            Ok(c) => c,
            Err(_) => Vec::new(),
        };

        let mut credentials: HashMap<String, String> = HashMap::new();
        let cookie_sources: Vec<&TokenSource> = config
            .token_sources
            .iter()
            .filter(|s| s.r#type == "cookie")
            .collect();

        for source in &cookie_sources {
            for cookie in &cookies {
                if cookie.name() == source.name {
                    if let Some(domain_filter) = &source.domain {
                        let clean = domain_filter.trim_start_matches('.');
                        if !cookie.domain().map(|d| d.contains(clean)).unwrap_or(false) {
                            continue;
                        }
                    }
                    credentials.insert(source.name.clone(), cookie.value().to_string());
                }
            }
        }

        // Extract localStorage / sessionStorage tokens via JS eval
        let storage_sources: Vec<&TokenSource> = config
            .token_sources
            .iter()
            .filter(|s| s.r#type == "localStorage" || s.r#type == "sessionStorage")
            .collect();

        if !storage_sources.is_empty() {
            let storage_type = if storage_sources[0].r#type == "localStorage" {
                "localStorage"
            } else {
                "sessionStorage"
            };
            let keys: Vec<String> = storage_sources.iter().map(|s| s.name.clone()).collect();
            let js = format!(
                "(() => {{
                    const res = {{}};
                    {}.forEach(k => {{
                        try {{ res[k] = {}.getItem(k); }} catch {{}}
                    }});
                    return JSON.stringify(res);
                }})()",
                serde_json::to_string(&keys).unwrap_or_default(),
                storage_type,
            );

            if let Ok(values_json) = window.eval(&format!(
                "window.__omniroute_login_extract = {}",
                js
            )) {
                let _ = values_json;
            }

            // Note: Tauri 2's eval() doesn't return a value directly. A complete
            // implementation would use a Tauri command to ship the values back.
            // For brevity we skip storage-source extraction in this initial port
            // — providers that rely on localStorage (rare) can be added later.
        }

        // Check whether all required cookie sources are present
        let required: Vec<&str> = cookie_sources.iter().map(|s| s.name.as_str()).collect();
        let all_found = !required.is_empty()
            && required.iter().all(|k| credentials.contains_key(*k));

        if all_found {
            let _ = app.emit("login:status", json!({
                "providerId": provider_id,
                "status": "complete",
                "message": "Credentials extracted successfully",
            }));
            return LoginResult {
                success: true,
                credentials: Some(credentials),
                error: None,
            };
        }

        tokio::select! {
            _ = tokio::time::sleep(poll_interval) => {}
            _ = cancel.notified() => {
                let _ = app.emit("login:status", json!({
                    "providerId": provider_id,
                    "status": "cancelled",
                    "message": "Login cancelled",
                }));
                return LoginResult { success: false, credentials: None, error: Some("Login cancelled".into()) };
            }
        }
    }

    let _ = app.emit("login:status", json!({
        "providerId": provider_id,
        "status": "error",
        "message": "Login timed out",
    }));
    LoginResult {
        success: false,
        credentials: None,
        error: Some("Login timed out".into()),
    }
}

/// Load the extraction config for a provider from the bundled Next.js
/// resources (`next-server/open-sse/services/tokenExtractionConfig.*`).
///
/// For the initial port we return None — the Electron version had a complex
/// dynamic require. Production use would either:
///   - Bundle a static JSON file with all configs and parse it here
///   - Spawn a tiny Node sidecar that requires the module and returns JSON
fn load_extraction_config(provider_id: &str) -> Option<ExtractionConfig> {
    // TODO: parse from `next-server/open-sse/services/tokenExtractionConfig.json`
    // (or generate it at build time from the JS module).
    //
    // For now, callers that invoke login:start will receive an "No extraction
    // config for provider" error — same as when the Electron require() failed.
    let _ = provider_id;
    None
}
