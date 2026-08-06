/**
 * OmniRoute Desktop — Tauri preload shim
 *
 * This file bridges the gap between the existing Electron-style `window.electronAPI`
 * contract (consumed by src/shared/hooks/useElectron.ts and ~6 React components)
 * and the Tauri 2.x command/event APIs.
 *
 * Loaded as an initialization script via Tauri's `initialization_script` mechanism
 * (declared in tauri.conf.json → app.windows[].initializationScript, or injected
 * on the main window via webview.eval() in csp::install).
 *
 * ZERO changes to React code: every method on `window.electronAPI` keeps its
 * signature and return type — invoke returns Promise<T>, send is fire-and-forget,
 * on* returns a disposer function (matching preload.js Fix #6).
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauri } from "@tauri-apps/api/core";

// ── Channel whitelist (mirrors electron/preload.js) ──────────
const VALID_CHANNELS = {
  invoke: [
    "get-app-info",
    "open-external",
    "get-data-dir",
    "restart-server",
    "check-for-updates",
    "download-update",
    "install-update",
    "get-app-version",
    "get-autostart-status",
    "enable-autostart",
    "disable-autostart",
    "login:start",
    "login:cancel",
    "login:status",
  ],
  send: ["window-minimize", "window-maximize", "window-close"],
  receive: ["server-status", "port-changed", "update-status", "login:status"],
} as const;

// ── Map preload channel names → Tauri command names ──────────
const COMMAND_MAP: Record<string, string> = {
  "get-app-info": "get_app_info",
  "open-external": "open_external",
  "get-data-dir": "get_data_dir",
  "restart-server": "restart_server",
  "get-app-version": "get_app_version",
  "check-for-updates": "check_for_updates",
  "download-update": "download_update",
  "install-update": "install_update",
  "get-autostart-status": "get_autostart_status",
  "enable-autostart": "enable_autostart",
  "disable-autostart": "disable_autostart",
  "login:start": "login_start",
  "login:cancel": "login_cancel",
  "login:status": "login_status",
  "window-minimize": "window_minimize",
  "window-maximize": "window_maximize",
  "window-close": "window_close",
};

async function safeInvoke(channel: string, ...args: unknown[]): Promise<unknown> {
  if (!VALID_CHANNELS.invoke.includes(channel as never)) {
    throw new Error(`Blocked IPC invoke: ${channel}`);
  }
  const cmd = COMMAND_MAP[channel] || channel;
  return invoke(cmd, argsToObj(cmd, args));
}

function argsToObj(cmd: string, args: unknown[]): Record<string, unknown> {
  // Tauri commands expect named args; the Electron preload used positional args.
  // We map by known command signatures.
  switch (cmd) {
    case "open_external":
      return { url: args[0] };
    case "login_start":
      return { providerId: args[0], options: args[1] ?? null };
    default:
      return {};
  }
}

function safeSend(channel: string): void {
  if (!VALID_CHANNELS.send.includes(channel as never)) return;
  const cmd = COMMAND_MAP[channel] || channel;
  // Fire-and-forget — errors logged but not surfaced
  invoke(cmd).catch((err) => console.error(`[Tauri shim] send ${channel} failed:`, err));
}

function safeOn(channel: string, callback: (data: unknown) => void): () => void {
  if (!VALID_CHANNELS.receive.includes(channel as never)) return () => {};
  let unlisten: UnlistenFn | null = null;
  let disposed = false;
  // listen() is async — return a disposer that handles the race
  listen(channel, (event) => callback(event.payload))
    .then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    })
    .catch((err) => console.error(`[Tauri shim] listen ${channel} failed:`, err));
  return () => {
    disposed = true;
    if (unlisten) unlisten();
  };
}

// ── Expose API to renderer (matches electron/preload.js 1:1) ──
const electronAPI = {
  // ── Invoke (async, returns Promise) ──────────────────────
  getAppInfo: () => safeInvoke("get-app-info"),
  openExternal: (url: string) => safeInvoke("open-external", url),
  getDataDir: () => safeInvoke("get-data-dir"),
  restartServer: () => safeInvoke("restart-server"),
  getAppVersion: () => safeInvoke("get-app-version"),

  // ── Auto-Update ──────────────────────────────────────────
  checkForUpdates: () => safeInvoke("check-for-updates"),
  downloadUpdate: () => safeInvoke("download-update"),
  installUpdate: () => safeInvoke("install-update"),

  // ── Autostart ────────────────────────────────────────────
  getAutostartStatus: () => safeInvoke("get-autostart-status"),
  enableAutostart: () => safeInvoke("enable-autostart"),
  disableAutostart: () => safeInvoke("disable-autostart"),

  // ── Send (fire-and-forget) ───────────────────────────────
  minimizeWindow: () => safeSend("window-minimize"),
  maximizeWindow: () => safeSend("window-maximize"),
  closeWindow: () => safeSend("window-close"),

  // ── Receive (event listeners) — returns disposer ─────────
  onServerStatus: (cb: (data: { status: string; port: number }) => void) =>
    safeOn("server-status", cb as (data: unknown) => void),
  onPortChanged: (cb: (port: number) => void) =>
    safeOn("port-changed", cb as (data: unknown) => void),
  onUpdateStatus: (cb: (data: { status: string; [k: string]: unknown }) => void) =>
    safeOn("update-status", cb as (data: unknown) => void),

  // ── Web-Cookie Login ──────────────────────────────────────
  startLogin: (providerId: string, options?: unknown) =>
    safeInvoke("login:start", providerId, options),
  cancelLogin: () => safeInvoke("login:cancel"),
  getLoginStatus: () => safeInvoke("login:status"),
  onLoginStatus: (cb: (data: { providerId: string; status: string; message: string }) => void) =>
    safeOn("login:status", cb as (data: unknown) => void),

  // ── Static Properties ────────────────────────────────────
  isElectron: true, // KEPT as `true` so useIsElectron() works unchanged
  platform: (() => {
    if (!isTauri()) return "unknown";
    const platform = (window as unknown as { __TAURI_INTERNALS__?: { platform?: string } })
      .__TAURI_INTERNALS__?.platform;
    if (platform === "macos") return "darwin";
    if (platform === "windows") return "win32";
    return platform ?? "unknown";
  })(),
};

// ── Install onto window (idempotent — safe to run multiple times) ──
if (typeof window !== "undefined") {
  (window as unknown as { electronAPI: typeof electronAPI }).electronAPI = electronAPI;
}

export {};
