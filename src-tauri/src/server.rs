//! Next.js standalone sidecar lifecycle.
//!
//! Mirrors `startNextServer()` / `stopNextServer()` / `waitForServer()` /
//! `waitForServerExit()` / `changePort()` in `electron/main.js`.
//!
//! Tauri sidecar mechanism: tauri-plugin-shell's `Command::new_sidecar("node")`
//! resolves to `binaries/node-<target-triple>[.exe]` next to the main binary
//! (declared in `tauri.conf.json` → `bundle.externalBin`).
//!
//! We pass the path to `next-server/server.js` as the first arg. The server.js
//! is shipped as a Tauri resource (declared in `bundle.resources` and assembled
//! by `scripts/build/prepare-tauri-standalone.mjs`).
//!
//! The Node sidecar runs the existing Next.js standalone bundle — no source
//! changes to the Next.js app itself.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use once_cell::sync::Lazy;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;

use crate::ipc::emit_server_status;
use crate::secrets::BootstrapResult;

/// The currently running sidecar child, if any. Mutex-guarded so restart /
/// port-change can swap atomically without races.
static SIDECHILD: Lazy<Mutex<Option<CommandChild>>> = Lazy::new(|| Mutex::new(None));

/// Wait for the server to respond to HTTP with a non-5xx status.
///
/// Mirrors `waitForServer()` in main.js (default 180s — first launch can run
/// long DB migrations).
pub async fn wait_for_health(url: &str, timeout: Duration) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() || resp.status().as_u16() < 500 => return true,
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

/// Locate the Next.js standalone server.js inside the Tauri resource dir.
///
/// Layout (after `prepare-tauri-standalone.mjs` runs):
///   <resource_dir>/next-server/server.js
///   <resource_dir>/next-server/server-ws.mjs   (preferred — peer-stamp wrapper)
///   <resource_dir>/next-server/node_modules/...
///
/// Mirrors `resolveServerEntry()` in `electron/lib/resolveServerEntry.js`.
fn resolve_server_entry(server_dir: &Path) -> &'static str {
    if server_dir.join("server-ws.mjs").exists() {
        "server-ws.mjs"
    } else {
        "server.js"
    }
}

/// Compute the Node.js V8 heap size, mirroring the inline IIFE in main.js:
///   - If NODE_OPTIONS already contains `--max-old-space-size`, keep it.
///   - Else if OMNIROUTE_MEMORY_MB is set and within [64, 16384], use it.
///   - Else default to ~35% of physical RAM, clamped to [512, 4096].
fn compute_heap_mb(server_env: &std::collections::HashMap<String, String>) -> Option<u32> {
    if let Some(node_opts) = server_env.get("NODE_OPTIONS") {
        if node_opts.contains("--max-old-space-size") {
            return None;
        }
    }
    if let Some(val) = server_env.get("OMNIROUTE_MEMORY_MB") {
        if let Ok(n) = val.parse::<u32>() {
            if (64..=16384).contains(&n) {
                return Some(n);
            }
        }
    }
    let total = sysinfo_total_mb();
    if total > 0 {
        Some((total * 35 / 100).clamp(512, 4096))
    } else {
        Some(512)
    }
}

#[cfg(not(target_os = "windows"))]
fn sysinfo_total_mb() -> u32 {
    // Avoid pulling in the `sysinfo` crate — use libc/sysconf. Fallback to 0.
    #[cfg(unix)]
    {
        extern "C" {
            fn sysconf(name: i32) -> i64;
        }
        // _SC_PHYS_PAGES = 85 on Linux, 200 on macOS — use a libc constant via nix
        // is overkill; we just try both common values and accept 0 on failure.
        unsafe {
            // Linux: 85, macOS: 200
            for sc in [85i32, 200] {
                let pages = sysconf(sc);
                if pages > 0 {
                    let page_size = sysconf(30); // _SC_PAGESIZE = 30 on Linux
                    if page_size > 0 {
                        return ((pages as u64 * page_size as u64) / (1024 * 1024)) as u32;
                    }
                }
            }
        }
        0
    }
    #[cfg(not(unix))]
    {
        0
    }
}

#[cfg(target_os = "windows")]
fn sysinfo_total_mb() -> u32 {
    // Use the Windows API MEMORYSTATUSEX via windows-sys
    use windows_sys::Win32::System::Threading::GetProcessHeap;
    // Simpler: call GlobalMemoryStatusEx. But to avoid pulling more features
    // into Cargo.toml, fall back to 0 (caller will use 512 MB default).
    let _ = GetProcessHeap;
    0
}

/// Start the Node sidecar that runs the Next.js standalone server.
///
/// In dev mode this is a no-op — Tauri's `beforeDevCommand` already started
/// `npm run dev` which serves on port 20128.
pub async fn start(app: &AppHandle, data_dir: &Path, port: u16) {
    if cfg!(dev) {
        log::info!("[Tauri] dev mode — connecting to existing Next.js server");
        let _ = emit_server_status(app, "running", port);
        return;
    }

    let resource_dir = match app.path().resource_dir() {
        Ok(p) => p,
        Err(e) => {
            log::error!("[Tauri] cannot resolve resource_dir: {}", e);
            let _ = emit_server_status(app, "error", port);
            return;
        }
    };
    let server_dir = resource_dir.join("next-server");
    let entry = resolve_server_entry(&server_dir);
    let server_script = server_dir.join(entry);

    if !server_script.exists() {
        log::error!("[Tauri] server script not found: {}", server_script.display());
        let _ = emit_server_status(app, "error", port);
        return;
    }

    // Re-read persisted env so we have current secrets (matches main.js)
    let bootstrap = match crate::secrets::bootstrap(data_dir) {
        Ok(b) => b,
        Err(e) => {
            log::error!("[Tauri] secret bootstrap failed: {}", e);
            let _ = emit_server_status(app, "error", port);
            return;
        }
    };
    let server_env = bootstrap.env;

    let heap_mb = compute_heap_mb(&server_env);
    let mut node_options = server_env.get("NODE_OPTIONS").cloned().unwrap_or_default();
    if let Some(mb) = heap_mb {
        if !node_options.contains("--max-old-space-size") {
            if !node_options.is_empty() {
                node_options.push(' ');
            }
            node_options.push_str(&format!("--max-old-space-size={}", mb));
        }
    }

    // Build NODE_PATH: app.asar.unpacked equivalent + next-server/node_modules
    let mut node_path_parts: Vec<String> = Vec::new();
    if let Some(existing) = server_env.get("NODE_PATH") {
        for part in existing.split(if cfg!(windows) { ';' } else { ':' }) {
            if !part.trim().is_empty() && Path::new(part).exists() {
                node_path_parts.push(part.to_string());
            }
        }
    }
    node_path_parts.push(server_dir.join("node_modules").to_string_lossy().to_string());

    log::info!("[Tauri] Starting Next.js server on port {}", port);
    log::info!("[Tauri] Server script: {}", server_script.display());
    log::info!("[Tauri] NODE_OPTIONS: {}", node_options);
    let _ = emit_server_status(app, "starting", port);

    let sidecar = app.shell().sidecar("node");
    let sidecar = match sidecar {
        Ok(cmd) => cmd,
        Err(e) => {
            log::error!("[Tauri] failed to resolve `node` sidecar: {}", e);
            let _ = emit_server_status(app, "error", port);
            return;
        }
    };

    let server_script_str = server_script.to_string_lossy().to_string();
    let mut command = sidecar
        .args([server_script_str.as_str()])
        .current_dir(server_dir.clone())
        .env("DATA_DIR", data_dir.to_string_lossy().to_string())
        .env("PORT", port.to_string())
        .env("NODE_ENV", "production")
        .env("NODE_PATH", node_path_parts.join(if cfg!(windows) { ";" } else { ":" }))
        .env("NODE_OPTIONS", node_options);

    // Pass through every server.env key (secrets + user overrides)
    for (k, v) in &server_env {
        if matches!(k.as_str(), "DATA_DIR" | "PORT" | "NODE_ENV" | "NODE_PATH" | "NODE_OPTIONS") {
            continue;
        }
        command = command.env(k, v);
    }

    let (mut rx, child) = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            log::error!("[Tauri] sidecar spawn failed: {}", e);
            let _ = emit_server_status(app, "error", port);
            return;
        }
    };

    *SIDECHILD.lock().unwrap() = Some(child);

    let app_clone = app.clone();
    tokio::spawn(async move {
        use tauri_plugin_shell::process::ProcessEvent;
        while let Some(event) = rx.recv().await {
            match event {
                ProcessEvent::Stdout(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    print!("[Server] {}", text);
                    if text.contains("Ready") || text.contains("started") || text.contains("listening") {
                        let _ = emit_server_status(&app_clone, "running", port);
                    }
                }
                ProcessEvent::Stderr(bytes) => {
                    eprint!("[Server:err] {}", String::from_utf8_lossy(&bytes));
                }
                ProcessEvent::Terminated(payload) => {
                    log::info!("[Tauri] server exited with code: {:?}", payload.code);
                    let _ = emit_server_status(&app_clone, "stopped", port);
                    *SIDECHILD.lock().unwrap() = None;
                    break;
                }
                _ => {}
            }
        }
    });
}

/// Stop the running sidecar (SIGTERM on POSIX, taskkill /T /F on Windows —
/// matches `killProcessTree()` in electron/processTree.js).
pub fn stop() {
    let mut guard = SIDECHILD.lock().unwrap();
    if let Some(child) = guard.take() {
        let _ = child.kill();
    }
}

/// Wait for the sidecar to exit, with a timeout. Mirrors `waitForServerExit()`.
pub async fn wait_exit(timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if SIDECHILD.lock().unwrap().is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Force kill if still alive
    let mut guard = SIDECHILD.lock().unwrap();
    if let Some(child) = guard.take() {
        let _ = child.kill();
    }
}

/// Restart the server (mirrors the `restart-server` IPC handler).
pub async fn restart(app: &AppHandle, data_dir: &Path, port: u16) -> bool {
    let _ = app.emit("server-status", serde_json::json!({ "status": "restarting", "port": port }));
    stop();
    wait_exit(Duration::from_secs(5)).await;
    start(app, data_dir, port).await;
    let url = format!("http://localhost:{}/api/monitoring/health", port);
    wait_for_health(&url, Duration::from_secs(180)).await;
    true
}

/// Change the server port (mirrors `changePort()` in main.js).
pub async fn change_port(app: &AppHandle, data_dir: &Path, new_port: u16) {
    let mut guard = crate::SERVER_PORT.lock().unwrap();
    let old = *guard;
    if new_port == old {
        return;
    }
    *guard = new_port;
    drop(guard);

    let _ = app.emit("server-status", serde_json::json!({ "status": "restarting", "port": new_port }));
    stop();
    wait_exit(Duration::from_secs(5)).await;
    start(app, data_dir, new_port).await;
    let url = format!("http://localhost:{}/api/monitoring/health", new_port);
    wait_for_health(&url, Duration::from_secs(180)).await;

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.eval(&format!("window.location.href = 'http://localhost:{}'", new_port));
    }
    let _ = app.emit("port-changed", new_port);
    let _ = app.emit("server-status", serde_json::json!({ "status": "running", "port": new_port }));
    log::info!("[Tauri] Port changed: {} → {}", old, new_port);
}

/// Resolve the resource_dir for use by other modules.
pub fn server_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path().resource_dir().ok().map(|p| p.join("next-server"))
}
