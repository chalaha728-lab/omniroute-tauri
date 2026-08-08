# OmniRoute Desktop — Tauri Edition

A 1:1 port of the OmniRoute Electron desktop shell to **Tauri 2**. The Electron
build was a thin wrapper around the OmniRoute Next.js server (it spawned
`server.js` / `server-ws.mjs` as a child process, opened a `BrowserWindow`
pointed at `http://localhost:{port}`, and exposed a small IPC surface
`window.electronAPI` to the renderer). This Tauri port preserves that contract
exactly — the Next.js frontend cannot tell whether it is running under
Electron or Tauri.

## Layout

```
src-tauri/
├── Cargo.toml                  # crate manifest + Tauri 2 deps
├── tauri.conf.json             # window, tray, bundle, updater config
├── build.rs                    # tauri-build hook
├── .cargo/config.toml          # link-arg for libsystemd (Linux)
├── capabilities/
│   └── default.json            # Tauri 2 capability ACL
├── icons/                      # icon.png / icon.ico / icon.icns + scaled PNGs
├── resources/
│   ├── placeholder/index.html  # loading screen shown before the server is ready
│   └── remoteServerPrompt.html # "Connect to Remote Server" modal
└── src/
    ├── main.rs                 # entry point — calls omniroute_lib::run()
    ├── lib.rs                  # Tauri builder, plugin registration, lifecycle
    ├── state.rs                # shared AppState (port, remote URL, server handle)
    ├── server.rs               # Next.js sidecar spawn/stop/restart/change_port
    ├── tray.rs                 # system-tray menu (Open / Port / Remote / Quit)
    ├── window.rs               # #[tauri::command] handlers (the IPC surface)
    ├── remote_server.rs        # remote-server URL prompt + persistence
    ├── env_bootstrap.rs        # JWT_SECRET / STORAGE_ENCRYPTION_KEY auto-gen
    ├── autostart.rs            # Linux ~/.config/autostart/*.desktop management
    └── preload_shim.js         # injected JS — exposes window.electronAPI
```

## What was ported (Electron → Tauri mapping)

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

- **Web-cookie login (`loginManager.js`)** — Electron's `BrowserWindow` exposes
  a session-cookie API that Tauri's webview doesn't. The `login:start` /
  `login:cancel` / `login:status` commands are exposed for renderer
  compatibility but return a clear "not yet implemented" error. OAuth/redirect
  flows handled entirely by the Next.js server are unaffected.

- **The full OmniRoute backend** (290-provider AI gateway, MITM proxy, native
  SQLite module, open-sse server) stays as Node.js — Tauri just spawns the
  bundled Next.js standalone server as a sidecar. Re-implementing the backend
  in Rust would be a separate, much larger effort.

## Prerequisites

### All platforms

- **Rust** (stable, ≥ 1.77) — <https://rustup.rs>
- **Node.js** (≥ 22.22.2) — for the Next.js dev server / build
- **`@tauri-apps/cli`** — installed automatically via `npm install` (declared
  in `devDependencies`).

### Linux additional system packages

```bash
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev \
  libgtk-3-dev \
  librsvg2-dev \
  libayatana-appindicator3-dev \
  libsoup-3.0-dev \
  libjavascriptcoregtk-4.1-dev \
  pkg-config \
  build-essential
```

### macOS additional system packages

```bash
xcode-select --install  # provides the CLT
```

### Windows additional system packages

- **Microsoft Visual Studio C++ Build Tools** (MSVC v143)
- **WebView2** runtime (preinstalled on Windows 10/11)

## Development

```bash
# from the repo root
npm install                 # installs @tauri-apps/cli
npm run tauri:dev           # starts Next.js (beforeDevCommand) + Tauri window
```

In dev mode (`cfg!(debug_assertions)` or `NODE_ENV=development`), the Rust
shell does **not** spawn the Next.js server — Tauri's `beforeDevCommand`
runs `npm run dev` for you, and the main window connects to
`http://localhost:3000` (override with `OMNIROUTE_DEV_URL`).

## Production build

```bash
npm run tauri:build
```

This runs `npm run build` (Next.js standalone build → `.build/next/standalone/`)
and then `cargo build --release`. The resulting bundle is in
`src-tauri/target/release/bundle/`.

To produce a fully self-contained installer you must also bundle the Next.js
standalone build as a Tauri resource — copy `.build/electron-standalone/` (the
output of `npm run prepare:bundle` in `electron/`) into
`src-tauri/resources/app/` before `npm run tauri:build`. The Rust shell will
then spawn `<resources>/app/server-ws.mjs` (or `server.js`) on launch.

## Configuration

### Environment variables

| Variable                | Default             | Purpose                                                                |
| ----------------------- | ------------------- | --------------------------------------------------------------------- |
| `OMNIROUTE_DEV_URL`     | `http://localhost:3000` | URL the Tauri window connects to in dev mode                      |
| `OMNIROUTE_REMOTE_URL`  | (unset)             | Connect to an already-running OmniRoute server instead of spawning one|
| `OMNIROUTE_NODE_PATH`   | `node`              | Absolute path to the Node.js binary used to spawn the server          |
| `OMNIROUTE_MEMORY_MB`   | 35% of RAM, clamped [512, 4096] | V8 `--max-old-space-size` for the spawned server          |
| `DATA_DIR`              | platform-default    | Where `server.env`, `electron-preferences.json`, `storage.sqlite` live|

### Tray menu

- **Open OmniRoute** — show & focus the main window
- **Open Dashboard** — open the server URL in the system browser
- **Server Port** → `20128` / `3000` / `8080` — restart the embedded server on the new port
- **Remote Server** → **Connect to Remote Server…** — opens a small dialog
  to point the shell at a different OmniRoute instance (e.g. a Docker
  container); persisted to `<data_dir>/electron-preferences.json`
- **Check for Updates** — fetch the latest release manifest from GitHub
- **Quit OmniRoute** — stop the server and exit

## IPC surface (renderer API)

The Next.js renderer sees exactly the same `window.electronAPI` object as the
Electron build (see `src/preload_shim.js`):

```ts
window.electronAPI = {
  isElectron: true,
  platform: "darwin" | "win32" | "linux",
  // invoke
  getAppInfo: () => Promise<AppInfo>,
  openExternal: (url: string) => Promise<void>,
  getDataDir: () => Promise<string>,
  restartServer: () => Promise<void>,
  getAppVersion: () => Promise<string>,
  checkForUpdates: () => Promise<void>,
  downloadUpdate: () => Promise<void>,
  installUpdate: () => Promise<void>,
  getAutostartStatus: () => Promise<boolean>,
  enableAutostart: () => Promise<boolean>,
  disableAutostart: () => Promise<boolean>,
  // send (fire-and-forget)
  minimizeWindow: () => void,
  maximizeWindow: () => void,
  closeWindow: () => void,
  // receive (event listeners — return disposer functions)
  onServerStatus: (cb: (s: ServerStatus) => void) => () => void,
  onPortChanged: (cb: (p: { port: number }) => void) => () => void,
  onUpdateStatus: (cb: (s: UpdateStatus) => void) => () => void,
  // web-cookie login (stub — see "What was NOT ported" above)
  startLogin: (providerId: string, options?: unknown) => Promise<LoginResult>,
  cancelLogin: () => Promise<void>,
  getLoginStatus: () => Promise<{ active: boolean }>,
  onLoginStatus: (cb: (s: LoginStatus) => void) => () => void,
}
```

The shim also exposes `window.remoteServerPrompt` (for the prompt window) and
`window.isTauri = true` (for any renderer-side feature detection that wants to
distinguish the two desktop shells).

## Updater

The Tauri updater is configured in `tauri.conf.json` to fetch release
manifests from
`https://github.com/diegosouzapw/OmniRoute/releases/latest/download/latest.json`.
To sign updates, set the `pubkey` field in `tauri.conf.json` and the
`TAURI_SIGNING_PRIVATE_KEY` env var when running `npm run tauri:build` — see
<https://tauri.app/plugin/updater/> for details. The unsigned default works
for development but the updater will refuse to install unsigned updates in
production.

## License

MIT, same as the upstream OmniRoute project.
