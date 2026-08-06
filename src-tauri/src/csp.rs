//! Content Security Policy.
//!
//! In Electron this was applied via `session.defaultSession.webRequest.onHeadersReceived`.
//! In Tauri 2, CSP is declared statically in `tauri.conf.json` → `app.security.csp`.
//! We don't need to mutate it at runtime — Tauri handles header injection for us.
//!
//! This module exists only to:
//!   - Document the parity
//!   - Inject the macOS drag-region stylesheet that the Electron preload installed
//!     (see `installMacDragRegion()` in `electron/preload.js`)

use tauri::AppHandle;

/// Install the macOS drag-region CSS via an initialization script.
/// Mirrors `installMacDragRegion()` in preload.js.
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let script = r#"
(function () {
  if (process && process.platform !== 'darwin') return; // only on mac
  const STYLE_ID = 'omniroute-electron-drag-region-style';
  const FALLBACK_ID = 'omniroute-electron-drag-region';
  const OBSERVER_KEY = '__omnirouteMacDragRegionObserver';

  function attach() {
    if (!document.head || !document.body) return;
    document.getElementById(STYLE_ID)?.remove();
    document.getElementById(FALLBACK_ID)?.remove();

    const style = document.createElement('style');
    style.id = STYLE_ID;
    style.textContent = `
      header, .omniroute-electron-drag-region {
        -webkit-app-region: drag; app-region: drag; user-select: none;
      }
      header a, header button, header input, header select, header textarea,
      header [role="button"], header [role="link"],
      header [tabindex]:not([tabindex="-1"]) {
        -webkit-app-region: no-drag; app-region: no-drag;
      }
      .omniroute-electron-drag-region {
        position: fixed; top: 0; left: 96px; right: 180px;
        height: 46px; z-index: 9999;
      }
    `;
    const dragRegion = document.createElement('div');
    dragRegion.id = FALLBACK_ID;
    dragRegion.className = 'omniroute-electron-drag-region';
    dragRegion.setAttribute('aria-hidden', 'true');
    document.head.appendChild(style);
    document.body.appendChild(dragRegion);

    const sync = () => {
      const hasHeader = Boolean(document.querySelector('header'));
      dragRegion.hidden = hasHeader;
      if (hasHeader) obs.disconnect();
    };
    if (window[OBSERVER_KEY]) window[OBSERVER_KEY].disconnect();
    const obs = new MutationObserver(sync);
    obs.observe(document.body, { childList: true, subtree: true });
    window[OBSERVER_KEY] = obs;
    setTimeout(() => obs.disconnect(), 5000);
    window.addEventListener('pagehide', () => obs.disconnect(), { once: true });
    sync();
  }

  if (document.readyState === 'loading') {
    window.addEventListener('DOMContentLoaded', attach, { once: true });
  } else {
    attach();
  }
})();
"#;

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.eval(script);

        // Inject the preload shim (bundled IIFE). In dev this lives next to the
        // Tauri project; in a packaged build it ships as a resource. We try both.
        if let Some(shim) = read_preload_shim(app) {
            let _ = window.eval(&shim);
        }
    }

    Ok(())
}

/// Read the bundled preload-shim.iife.js. In dev it sits at
/// `<repo>/src-tauri/preload-shim.iife.js`; in a packaged build it ships
/// as a Tauri resource at `resources/preload-shim.iife.js`.
fn read_preload_shim(app: &AppHandle) -> Option<String> {
    use tauri::Manager;
    // 1. Try the resource dir (packaged build)
    if let Ok(resource_dir) = app.path().resource_dir() {
        let path = resource_dir.join("preload-shim.iife.js");
        if let Ok(content) = std::fs::read_to_string(&path) {
            return Some(content);
        }
    }
    // 2. Try the dev path (relative to the current working dir)
    let dev_path = std::path::PathBuf::from("src-tauri/preload-shim.iife.js");
    if let Ok(content) = std::fs::read_to_string(&dev_path) {
        return Some(content);
    }
    None
}
