//! Linux-specific autostart helpers — Rust port of the
//! `enableLinuxDesktopAutostart` / `disableLinuxDesktopAutostart` /
//! `isLinuxDesktopAutostartEnabled` functions in `electron/main.js`.
//!
//! `tauri-plugin-autostart` already manages a `.desktop` entry on Linux, but
//! the Electron build wrote its own file at
//! `~/.config/autostart/omniroute-desktop.desktop` with `Exec="<exe>" --hidden`.
//! We support both so settings survive a switch between the Electron and
//! Tauri desktop shells.

#![cfg(target_os = "linux")]

use std::fs;
use std::path::PathBuf;

fn autostart_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config").join("autostart"))
}

fn desktop_path() -> Option<PathBuf> {
    autostart_dir().map(|d| d.join("omniroute-desktop.desktop"))
}

/// Write the `~/.config/autostart/omniroute-desktop.desktop` file pointing
/// at the current executable with the `--hidden` flag. Mirrors the Electron
/// build's `enableLinuxDesktopAutostart()` exactly (same filename, same
/// `Exec=` quoting, same `--hidden` arg).
pub fn enable_linux_desktop_autostart() -> bool {
    let Some(dir) = autostart_dir() else {
        log::error!("[OmniRoute] could not resolve home dir for autostart");
        return false;
    };
    let Some(path) = desktop_path() else {
        return false;
    };
    let exe = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(e) => {
            log::error!("[OmniRoute] current_exe failed: {e}");
            return false;
        }
    };
    if let Err(e) = fs::create_dir_all(&dir) {
        log::error!("[OmniRoute] create_dir_all({}): {e}", dir.display());
        return false;
    }
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=OmniRoute\n\
         Comment=OmniRoute Desktop Client\n\
         Exec=\"{exe}\" --hidden\n\
         Terminal=false\n\
         Hidden=false\n\
         X-GNOME-Autostart-enabled=true\n"
    );
    if let Err(e) = fs::write(&path, content) {
        log::error!("[OmniRoute] write {}: {e}", path.display());
        return false;
    }
    true
}

pub fn disable_linux_desktop_autostart() {
    if let Some(path) = desktop_path() {
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
    }
}

pub fn is_linux_desktop_autostart_enabled() -> bool {
    desktop_path().map(|p| p.exists()).unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
pub fn enable_linux_desktop_autostart() -> bool { false }
#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
pub fn disable_linux_desktop_autostart() {}
#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
pub fn is_linux_desktop_autostart_enabled() -> bool { false }
