//! Autostart helpers — Linux uses ~/.config/autostart/omniroute-desktop.desktop
//! (mirrors `enableLinuxDesktopAutostart()` in main.js).
//!
//! On macOS and Windows, `tauri-plugin-autostart` handles everything for us
//! (it uses LaunchAgent on macOS, registry on Windows).

use std::path::PathBuf;

pub fn linux_autostart_desktop_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config").join("autostart").join("omniroute-desktop.desktop"))
}

pub fn enable_linux_desktop_autostart(exec_path: &str) -> bool {
    if let Some(path) = linux_autostart_desktop_path() {
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return false;
            }
        }
        let content = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=OmniRoute\n\
             Comment=OmniRoute Desktop Client\n\
             Exec=\"{}\" --hidden\n\
             Terminal=false\n\
             Hidden=false\n\
             X-GNOME-Autostart-enabled=true\n",
            exec_path
        );
        return std::fs::write(&path, content).is_ok();
    }
    false
}

pub fn disable_linux_desktop_autostart() -> bool {
    if let Some(path) = linux_autostart_desktop_path() {
        if path.exists() {
            return std::fs::remove_file(&path).is_ok();
        }
        return true;
    }
    false
}

pub fn is_linux_desktop_autostart_enabled() -> bool {
    linux_autostart_desktop_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}
