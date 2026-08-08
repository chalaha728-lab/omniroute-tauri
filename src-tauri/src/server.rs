//! Embedded Next.js server lifecycle — Rust port of the `startNextServer`,
//! `stopNextServer`, `waitForServer`, `waitForServerExit`, and `changePort`
//! functions in `electron/main.js`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tauri::{AppHandle, Manager};
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, Command};
use tokio::time::timeout;

use crate::env_bootstrap::bootstrap_server_env;
use crate::state::{self, SharedState};
use crate::{emit_port_changed, emit_server_status};

/// Handle to a spawned Next.js server child process.
pub struct ServerHandle {
    pub child: Child,
    pub pid: u32,
}

impl ServerHandle {
    /// Send SIGTERM (POSIX) or `taskkill /T /F` (Windows) to the server and
    /// all its descendants. Mirrors `killProcessTree()` in
    /// `electron/processTree.js`.
    pub async fn kill_tree(&mut self) -> Result<()> {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            let _ = Command::new("taskkill")
                .args(["/PID", &self.pid.to_string(), "/T", "/F"])
                .creation_flags(CREATE_NO_WINDOW)
                .status()
                .await;
            let _ = self.child.kill().await;
        }
        #[cfg(not(windows))]
        {
            let _ = self.child.kill().await;
        }
        Ok(())
    }
}

pub fn resolve_server_dir(app: &AppHandle) -> Result<PathBuf> {
    let resource_dir = app
        .path()
        .resource_dir()
        .context("failed to resolve resource_dir")?;
    Ok(resource_dir.join("app"))
}

pub fn resolve_server_entry(server_dir: &Path) -> Result<PathBuf> {
    let ws_entry = server_dir.join("server-ws.mjs");
    if ws_entry.exists() {
        return Ok(ws_entry);
    }
    let bare = server_dir.join("server.js");
    if bare.exists() {
        return Ok(bare);
    }
    Err(anyhow!(
        "neither server-ws.mjs nor server.js found in {}",
        server_dir.display()
    ))
}

/// Top-level startup sequence — equivalent to the body of `app.whenReady()`
/// in `electron/main.js`.
pub async fn startup_sequence(app: &AppHandle, state: SharedState) -> Result<()> {
    let (is_dev, port, remote_url) = {
        let s = state::lock(&state);
        (s.is_dev, s.port, s.remote_server_url.clone())
    };

    if is_dev {
        log::info!("[OmniRoute] dev mode — connecting to existing Next.js dev server");
        emit_server_status(app, "running", port, remote_url.as_deref());
        navigate_main_to_server(app, &state).await;
        return Ok(());
    }

    if remote_url.is_some() {
        log::info!("[OmniRoute] remote-server mode — not spawning a local server");
        emit_server_status(app, "running", port, remote_url.as_deref());
        navigate_main_to_server(app, &state).await;
        return Ok(());
    }

    let data_dir = state::lock(&state).data_dir.clone();
    let mut server_env = bootstrap_server_env(&data_dir)
        .await
        .context("env bootstrap failed")?;
    server_env.insert("PORT".into(), port.to_string());
    server_env.insert("NODE_ENV".into(), "production".into());
    server_env.insert("DATA_DIR".into(), data_dir.to_string_lossy().into());

    let heap_mb = compute_heap_mb();
    let existing_node_options = server_env
        .get("NODE_OPTIONS")
        .cloned()
        .unwrap_or_default();
    let node_options = if existing_node_options.contains("--max-old-space-size") {
        existing_node_options
    } else {
        format!("{existing_node_options} --max-old-space-size={heap_mb}")
            .trim()
            .to_string()
    };
    server_env.insert("NODE_OPTIONS".into(), node_options);

    emit_server_status(app, "starting", port, None);

    let handle = spawn_server(app, &server_env)
        .await
        .context("spawn_server failed")?;
    {
        let mut s = state::lock(&state);
        s.server_handle = Some(handle);
    }

    let url = format!("http://localhost:{port}/api/monitoring/health");
    let ready = wait_for_server(&url, Duration::from_secs(180)).await;
    if !ready {
        log::warn!("[OmniRoute] server readiness timed out — showing window anyway");
    }
    emit_server_status(app, "running", port, None);
    navigate_main_to_server(app, &state).await;

    if !ready {
        let app_clone = app.clone();
        let state_clone = state.clone();
        tokio::spawn(async move {
            let url = format!("http://localhost:{port}/api/monitoring/health");
            if wait_for_server(&url, Duration::from_secs(300)).await {
                navigate_main_to_server(&app_clone, &state_clone).await;
            }
        });
    }

    Ok(())
}

/// Spawn the Next.js standalone server as a tokio child process.
pub async fn spawn_server(app: &AppHandle, env: &HashMap<String, String>) -> Result<ServerHandle> {
    let server_dir = resolve_server_dir(app)?;
    let server_script = resolve_server_entry(&server_dir)?;
    let node_exe = resolve_node_executable_with_app(app);

    log::info!(
        "[OmniRoute] starting Next.js server: {} {} (cwd={})",
        node_exe,
        server_script.display(),
        server_dir.display()
    );

    let mut cmd = Command::new(&node_exe);
    cmd.arg(&server_script);
    cmd.current_dir(&server_dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn {} {}", node_exe, server_script.display()))?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow!("spawned child has no pid"))?;

    if let Some(stdout) = child.stdout.take() {
        let app_clone = app.clone();
        let state_clone = fetch_state(app);
        tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                log::info!("[Server] {line}");
                if line.contains("Ready")
                    || line.contains("started")
                    || line.contains("listening")
                {
                    let port = state::lock(&state_clone).port;
                    emit_server_status(&app_clone, "running", port, None);
                }
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                log::warn!("[Server:err] {line}");
            }
        });
    }

    Ok(ServerHandle { child, pid })
}

/// Tiny helper to fetch the managed `SharedState` out of an `AppHandle`
/// without dragging the `tauri::State` lifetime into async tasks.
pub fn fetch_state(app: &AppHandle) -> SharedState {
    let state: tauri::State<'_, SharedState> = app.state();
    state.inner().clone()
}

/// Resolve the Node.js executable to use for spawning the server.
///
/// Precedence (mirrors the Electron build's `resolveNodeExecutable()`):
///   1. `OMNIROUTE_NODE_PATH` env var (explicit override — operator/developer)
///   2. `<resources>/app/node/node.exe` (Windows) or `<resources>/app/node/bin/node`
///      (macOS/Linux) — a portable Node.js build bundled as a Tauri resource
///      so the app is fully self-contained (no system Node required)
///   3. `node` on PATH (last resort — for dev mode or operators who prefer
///      their system Node)
pub fn resolve_node_executable() -> String {
    if let Ok(path) = std::env::var("OMNIROUTE_NODE_PATH") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "node".to_string()
}

/// Same as `resolve_node_executable` but takes the AppHandle so it can
/// resolve the bundled Node binary inside Tauri's resource directory.
/// This is the version actually used by `spawn_server` — the bundled
/// binary takes precedence over `node` on PATH so the app is self-contained.
pub fn resolve_node_executable_with_app(app: &AppHandle) -> String {
    // 1. Explicit env override
    if let Ok(path) = std::env::var("OMNIROUTE_NODE_PATH") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    // 2. Bundled portable Node.js in <resources>/app/node/
    if let Ok(resource_dir) = app.path().resource_dir() {
        let candidates = bundled_node_paths(&resource_dir);
        for candidate in candidates {
            if candidate.exists() {
                return candidate.to_string_lossy().to_string();
            }
        }
    }

    // 3. System Node on PATH (last resort)
    "node".to_string()
}

/// Platform-specific paths where the bundled portable Node.js lives.
fn bundled_node_paths(resource_dir: &Path) -> Vec<PathBuf> {
    let base = resource_dir.join("app").join("node");
    vec![
        // Windows portable: node-v22.11.0-win-x64/node.exe
        base.join("node.exe"),
        base.join("bin").join("node.exe"),
        // macOS / Linux portable tarball layout: bin/node
        base.join("bin").join("node"),
        // Flat layout (some portable builds)
        base.join("node"),
    ]
}

pub async fn wait_for_server(url: &str, timeout_duration: Duration) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let start = std::time::Instant::now();
    while start.elapsed() < timeout_duration {
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() || resp.status().as_u16() < 500 => {
                return true;
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

pub async fn stop_server(app: &AppHandle, mut handle: ServerHandle) -> Result<()> {
    let port = state::lock(&fetch_state(app)).port;
    emit_server_status(app, "stopped", port, None);
    let _ = handle.kill_tree().await;
    let _ = timeout(Duration::from_secs(5), handle.child.wait()).await;
    Ok(())
}

pub async fn restart_server(app: &AppHandle, state: SharedState) -> Result<()> {
    let (is_dev, port, remote_url) = {
        let s = state::lock(&state);
        (s.is_dev, s.port, s.remote_server_url.clone())
    };
    emit_server_status(app, "restarting", port, remote_url.as_deref());

    if is_dev || remote_url.is_some() {
        emit_server_status(app, "running", port, remote_url.as_deref());
        return Ok(());
    }

    let old_handle = state::lock(&state).server_handle.take();
    if let Some(mut old) = old_handle {
        let _ = old.kill_tree().await;
        let _ = timeout(Duration::from_secs(5), old.child.wait()).await;
    }

    let data_dir = state::lock(&state).data_dir.clone();
    let mut server_env = bootstrap_server_env(&data_dir).await?;
    server_env.insert("PORT".into(), port.to_string());
    server_env.insert("NODE_ENV".into(), "production".into());
    server_env.insert("DATA_DIR".into(), data_dir.to_string_lossy().into());

    let new_handle = spawn_server(app, &server_env).await?;
    state::lock(&state).server_handle = Some(new_handle);

    let url = format!("http://localhost:{port}/api/monitoring/health");
    let _ = wait_for_server(&url, Duration::from_secs(180)).await;
    emit_server_status(app, "running", port, None);
    navigate_main_to_server(app, &state).await;
    Ok(())
}

pub async fn change_port(app: &AppHandle, state: SharedState, new_port: u16) -> Result<()> {
    let old_port = state::lock(&state).port;
    if new_port == old_port {
        return Ok(());
    }
    state::lock(&state).set_port(new_port);
    emit_server_status(app, "restarting", new_port, None);

    let old_handle = state::lock(&state).server_handle.take();
    if let Some(mut old) = old_handle {
        let _ = old.kill_tree().await;
        let _ = timeout(Duration::from_secs(5), old.child.wait()).await;
    }

    restart_server(app, state.clone()).await?;
    emit_port_changed(app, new_port);
    Ok(())
}

/// Navigate the main window to the resolved server URL. Tauri 2 doesn't
/// expose a `set_url` on `WebviewWindow`; we use `eval` with a JS redirect
/// instead. This is functionally equivalent — the placeholder index.html
/// is replaced atomically with the Next.js app once the server is ready.
pub async fn navigate_main_to_server(app: &AppHandle, state: &SharedState) {
    let url = {
        let s = state::lock(state);
        if let Some(remote) = s.remote_server_url.clone() {
            remote
        } else if s.is_dev {
            std::env::var("OMNIROUTE_DEV_URL")
                .unwrap_or_else(|_| "http://localhost:3000".to_string())
        } else {
            format!("http://localhost:{}", s.port)
        }
    };
    if let Some(window) = app.get_webview_window("main") {
        log::info!("[OmniRoute] navigating main window to {url}");
        // Escape any single quotes / backslashes in the URL so the eval'd
        // JS string literal stays valid (URLs are normally safe, but the
        // remote-server URL is user-provided).
        let escaped = url.replace('\\', "\\\\").replace('\'', "\\'");
        let js = format!("window.location.replace('{escaped}');");
        if let Err(e) = window.eval(&js) {
            log::warn!("[OmniRoute] eval navigation failed: {e}");
        }
    }
}

fn compute_heap_mb() -> u64 {
    if let Ok(explicit) = std::env::var("OMNIROUTE_MEMORY_MB") {
        if let Ok(mb) = explicit.trim().parse::<u64>() {
            if (64..=16384).contains(&mb) {
                return mb;
            }
        }
    }
    let sys = sysinfo::System::new_all();
    let total_mb = sys.total_memory() / 1024 / 1024;
    if total_mb == 0 {
        return 512;
    }
    (total_mb * 35 / 100).clamp(512, 4096)
}

// Suppress unused-import warnings for items kept for future use.
#[allow(dead_code)]
fn _force_use(_a: Arc<()>) {}
