# OmniRoute — Tauri Edition

A 1:1 port of the [OmniRoute](https://github.com/diegosouzapw/OmniRoute) Electron desktop shell to **Tauri 2**. The Next.js frontend and Node.js backend are unchanged — the Tauri shell spawns the bundled Next.js standalone server as a sidecar and exposes the exact same `window.electronAPI` IPC surface, so the renderer needs zero changes.

> **This repo** contains only the Tauri shell (`src-tauri/`) — enough to build a native Windows `.exe` that demonstrates the port runs. To produce a fully self-contained installer that also bundles the OmniRoute Next.js server, drop the output of `npm run prepare:bundle` (from the upstream OmniRoute repo) into `src-tauri/resources/app/` before `npm run tauri:build`.

## Build status

![Build Windows](https://github.com/chalaha728-lab/omniroute-tauri/actions/workflows/build-windows.yml/badge.svg)

## Download the Windows build

1. Go to **[Actions → Build Windows](https://github.com/chalaha728-lab/omniroute-tauri/actions/workflows/build-windows.yml)**.
2. Click the latest successful run.
3. Scroll to **Artifacts** → download `OmniRoute-Windows-NSIS` (NSIS installer) or `OmniRoute-Windows-MSI` (MSI installer).
4. Unzip and double-click `OmniRoute_3.8.50_x64-setup.exe`.

Tagged releases (`v*`) also publish the installer to the [Releases page](https://github.com/chalaha728-lab/omniroute-tauri/releases).

## Build locally

### Prerequisites

- **Rust** (stable, ≥ 1.77) — <https://rustup.rs>
- **Node.js** (≥ 22) — for `@tauri-apps/cli`
- **Windows**: Microsoft Visual Studio C++ Build Tools + WebView2 runtime (preinstalled on Win 10/11)
- **macOS**: `xcode-select --install`
- **Linux**: `sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev pkg-config build-essential`

### Steps

```bash
npm install                 # pulls @tauri-apps/cli v2
npm run tauri:dev           # dev: opens the window against http://localhost:3000
npm run tauri:build         # production: builds NSIS / MSI / .app / .deb / AppImage
```

The output lives in `src-tauri/target/release/bundle/`.

## Layout

```
src-tauri/
├── Cargo.toml              # crate manifest + Tauri 2 deps + plugins
├── tauri.conf.json         # window, tray, bundle, updater config
├── build.rs                # tauri-build hook
├── .cargo/config.toml      # Linux-only link arg for libsystemd
├── capabilities/
│   └── default.json        # Tauri 2 capability ACL
├── icons/                  # icon.png / icon.ico / icon.icns + scaled PNGs
├── resources/
│   ├── placeholder/index.html   # loading screen shown before the server is ready
│   └── remoteServerPrompt.html  # "Connect to Remote Server" modal
└── src/
    ├── main.rs             # entry point — calls omniroute_lib::run()
    ├── lib.rs              # Tauri builder, plugin registration, lifecycle
    ├── state.rs            # shared AppState (port, remote URL, server handle)
    ├── server.rs           # Next.js sidecar spawn/stop/restart/change_port
    ├── tray.rs             # system-tray menu (Open / Port / Remote / Quit)
    ├── window.rs           # #[tauri::command] handlers (the IPC surface)
    ├── remote_server.rs    # remote-server URL prompt + persistence
    ├── env_bootstrap.rs    # JWT_SECRET / STORAGE_ENCRYPTION_KEY auto-gen
    ├── autostart.rs        # Linux ~/.config/autostart/*.desktop management
    └── preload_shim.js     # injected JS — exposes window.electronAPI
```

## What was ported (Electron → Tauri)

| Electron (main.js / preload.js)                | Tauri (Rust)                                            |
| ----------------------------------------------- | ------------------------------------------------------- |
| `new BrowserWindow({ webPreferences: { preload } })` | `WebviewWindowBuilder` + `initialization_script` (preload_shim.js) |
| `app.requestSingleInstanceLock()`               | `tauri_plugin_single_instance`                          |
| `new Tray(icon)` + `Menu.buildFromTemplate(...)`| `TrayIconBuilder` + `Menu::with_items(...)`             |
| `autoUpdater` (electron-updater)                | `tauri_plugin_updater`                                  |
| `app.setLoginItemSettings({ openAtLogin })`     | `tauri_plugin_autostart` + manual `.desktop` on Linux   |
| `ipcMain.handle("get-app-info", …)`             | `#[tauri::command] fn get_app_info(...)`                |
| `ipcRenderer.invoke(channel, ...args)`          | `window.__TAURI__.core.invoke(cmd, args)`               |
| `mainWindow.webContents.send("server-status")`  | `app.emit("server-status", payload)`                    |
| `spawn(nodeExe, [serverScript], …)`             | `tokio::process::Command::new(node).arg(script).spawn()`|
| `killProcessTree(proc, SIGTERM)`                | `ServerHandle::kill_tree()` (taskkill /T /F on Windows) |
| `waitForServer(url, 180000)`                    | `wait_for_server(url, Duration::from_secs(180))`        |
| `bootstrap-env.mjs` (JWT/STORAGE/API secrets)   | `env_bootstrap::bootstrap_server_env()`                 |
| `electron-preferences.json` (remote URL)        | `remote_server::read_preferences()` / `write_remote_server_url()` |
| `Notification`                                  | `tauri_plugin_notification`                             |
| `shell.openExternal(url)`                       | `tauri_plugin_shell::ShellExt::shell().open(url)`       |
| macOS `titleBarStyle: "hiddenInset"`            | `WebviewWindow::set_title_bar_style(TitleBarStyle::Overlay)` |
| macOS drag-region CSS (`-webkit-app-region`)    | same CSS, injected by preload_shim.js                   |
| `session.defaultSession.webRequest.onHeadersReceived` (CSP) | `app.security.csp` in tauri.conf.json      |

## What was NOT ported (deliberately)

- **Web-cookie login (`loginManager.js`)** — Electron's `BrowserWindow` exposes a session-cookie API that Tauri's webview doesn't. The `login:start` / `login:cancel` / `login:status` commands are exposed for renderer compatibility but return a clear "not yet implemented" error. OAuth/redirect flows handled entirely by the Next.js server are unaffected.

- **The full OmniRoute backend** (290-provider AI gateway, MITM proxy, native SQLite module, open-sse server) stays as Node.js — Tauri just spawns the bundled Next.js standalone server as a sidecar.

## Configuration

| Variable                | Default             | Purpose                                                                |
| ----------------------- | ------------------- | --------------------------------------------------------------------- |
| `OMNIROUTE_DEV_URL`     | `http://localhost:3000` | URL the Tauri window connects to in dev mode                      |
| `OMNIROUTE_REMOTE_URL`  | (unset)             | Connect to an already-running OmniRoute server instead of spawning one|
| `OMNIROUTE_NODE_PATH`   | `node`              | Absolute path to the Node.js binary used to spawn the server          |
| `OMNIROUTE_MEMORY_MB`   | 35% of RAM, clamped [512, 4096] | V8 `--max-old-space-size` for the spawned server          |
| `DATA_DIR`              | platform-default    | Where `server.env`, `electron-preferences.json`, `storage.sqlite` live|

## License

MIT, same as the upstream OmniRoute project.
