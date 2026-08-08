//! Shared application state — equivalent to the module-level `let` bindings
//! at the top of `electron/main.js` (`mainWindow`, `tray`, `nextServer`,
//! `serverPort`, `isServerStopped`, `remoteServerPromptWindow`).

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::server::ServerHandle;

/// The default port the embedded Next.js server listens on. Mirrors the
/// Electron `serverPort = 20128` initial value.
pub const DEFAULT_PORT: u16 = 20128;

/// All the process-global state the desktop shell mutates from multiple
/// threads. Guarded by a `parking_lot::Mutex` (cheaper than `std::sync::Mutex`
/// and never poisoned).
#[derive(Default)]
pub struct AppState {
    /// `true` when running from `tauri dev` (or `NODE_ENV=development`).
    /// Mirrors `isDev = process.env.NODE_ENV === "development" || !app.isPackaged`.
    pub is_dev: bool,

    /// The data directory used for `server.env`, `electron-preferences.json`,
    /// `storage.sqlite`, etc. Resolved the same way as
    /// `resolveDataDir()` in `electron/main.js`:
    ///   1. `DATA_DIR` env var (if set)
    ///   2. `%APPDATA%/omniroute` on Windows
    ///   3. `$XDG_CONFIG_HOME/omniroute` on Linux
    ///   4. `~/.omniroute` everywhere else
    pub data_dir: PathBuf,

    /// The port the embedded Next.js server listens on. Mutable because the
    /// tray menu lets the user switch between 20128 / 3000 / 8080.
    pub port: u16,

    /// When `Some`, the desktop shell connects to an already-running
    /// OmniRoute server (e.g. a Docker container) instead of spawning its own
    /// bundled Next.js server. Mirrors `remoteServerUrl` in `electron/main.js`.
    pub remote_server_url: Option<String>,

    /// Handle to the spawned Next.js server process (if any). `None` when
    /// running in dev mode (the user runs `npm run dev` separately) or in
    /// remote-server mode (no local server to spawn).
    pub server_handle: Option<ServerHandle>,

    /// Set to `true` by the tray "Quit" menu item and by `install-update` so
    /// the `close-requested` handler lets the window actually close instead of
    /// hiding to tray. Mirrors `app.isQuitting` in `electron/main.js`.
    pub is_quitting: bool,
}

impl AppState {
    /// Override the port that the embedded server will be spawned on next.
    pub fn set_port(&mut self, port: u16) {
        self.port = port;
    }

    /// Mark the app as quitting so the close-to-tray handler steps aside.
    pub fn mark_quitting(&mut self) {
        self.is_quitting = true;
    }
}

/// Initialize a fresh `AppState` with the resolved `data_dir` and default port.
/// Called once from `lib.rs::run()`'s `setup` hook.
pub fn initial_state() -> AppState {
    let mut state = AppState::default();
    state.port = DEFAULT_PORT;
    state.data_dir = resolve_data_dir();
    state
}

pub fn resolve_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("DATA_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            return PathBuf::from(app_data).join("omniroute");
        }
        if let Some(home) = dirs::home_dir() {
            return home.join("AppData").join("Roaming").join("omniroute");
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            let trimmed = xdg.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed).join("omniroute");
            }
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".omniroute");
        }
    }

    PathBuf::from(".omniroute")
}

/// Type alias for the managed state handle used throughout the crate.
pub type SharedState = Arc<Mutex<AppState>>;

/// Lock helper — keeps the locking call sites terse and centralizes the
/// `tauri::State` → `&AppState` dereference.
pub fn lock(state: &SharedState) -> parking_lot::MutexGuard<'_, AppState> {
    state.lock()
}
