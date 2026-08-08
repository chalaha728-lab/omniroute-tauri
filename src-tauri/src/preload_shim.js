/**
 * OmniRoute Tauri Desktop — preload shim.
 *
 * This script is injected into the main webview via
 * `WebviewWindowBuilder::initialization_script(...)` (configured in
 * `lib.rs::run()`). It exposes a `window.electronAPI` object whose shape
 * EXACTLY matches `electron/preload.js` from the upstream Electron build,
 * so the Next.js renderer cannot tell whether it is running under Electron
 * or Tauri.
 *
 * Each method delegates to a Tauri command (registered in `src/window.rs`
 * and `src/remote_server.rs`) via `window.__TAURI__.core.invoke(...)`, and
 * each event listener delegates to `window.__TAURI__.event.listen(...)`.
 *
 * The channel whitelist is preserved as a defense-in-depth measure — even
 * though Tauri's capability system already gates which commands the
 * renderer may invoke, mirroring the Electron `VALID_CHANNELS` list keeps
 * the two desktop shells' behavior identical.
 */
(function () {
  if (window.electronAPI) return; // already injected (HMR / re-navigation)

  const TAURI = () => window.__TAURI__;
  const invoke = (cmd, args) => TAURI().core.invoke(cmd, args);
  const listen = (event, cb) => TAURI().event.listen(event, cb);

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
      "remote-server-prompt:get-initial-url",
    ],
    send: [
      "window-minimize",
      "window-maximize",
      "window-close",
      "remote-server-prompt:submit",
      "remote-server-prompt:cancel",
    ],
    receive: ["server-status", "port-changed", "update-status", "login:status"],
  };

  // ── macOS drag region (port of installMacDragRegion in preload.js) ───────
  // Tauri's webview also respects `-webkit-app-region: drag` on macOS, so
  // we install the same CSS shim the Electron build did. This makes the
  // header draggable on macOS while keeping buttons/inputs clickable.
  function installMacDragRegion() {
    if (window.navigator.platform.indexOf("Mac") !== 0) return;

    const MAC_DRAG_STYLE_ID = "omniroute-electron-drag-region-style";
    const MAC_DRAG_FALLBACK_ID = "omniroute-electron-drag-region";
    const MAC_DRAG_OBSERVER_KEY = "__omnirouteMacDragRegionObserver";

    const attach = () => {
      if (!document.head || !document.body) return;

      document.getElementById(MAC_DRAG_STYLE_ID)?.remove();
      document.getElementById(MAC_DRAG_FALLBACK_ID)?.remove();

      const style = document.createElement("style");
      style.id = MAC_DRAG_STYLE_ID;
      style.textContent = `
        header,
        .omniroute-electron-drag-region {
          app-region: drag;
          -webkit-app-region: drag;
          user-select: none;
        }
        header a,
        header button,
        header input,
        header select,
        header textarea,
        header [role="button"],
        header [role="link"],
        header [tabindex]:not([tabindex="-1"]) {
          app-region: no-drag;
          -webkit-app-region: no-drag;
        }
        .omniroute-electron-drag-region {
          position: fixed;
          top: 0;
          left: 96px;
          right: 180px;
          height: 46px;
          z-index: 9999;
        }
      `;

      const dragRegion = document.createElement("div");
      dragRegion.id = MAC_DRAG_FALLBACK_ID;
      dragRegion.className = "omniroute-electron-drag-region";
      dragRegion.setAttribute("aria-hidden", "true");

      document.head.appendChild(style);
      document.body.appendChild(dragRegion);

      const syncDragFallback = () => {
        const hasHeader = Boolean(document.querySelector("header"));
        dragRegion.hidden = hasHeader;
        if (hasHeader && observer) observer.disconnect();
      };
      const previousObserver = window[MAC_DRAG_OBSERVER_KEY];
      if (previousObserver) previousObserver.disconnect();

      const observer = new MutationObserver(syncDragFallback);
      observer.observe(document.body, { childList: true, subtree: true });
      window[MAC_DRAG_OBSERVER_KEY] = observer;
      window.setTimeout(() => observer.disconnect(), 5000);
      window.addEventListener("pagehide", () => observer.disconnect(), { once: true });
      syncDragFallback();
    };

    if (document.readyState === "loading") {
      window.addEventListener("DOMContentLoaded", attach, { once: true });
    } else {
      attach();
    }
  }

  installMacDragRegion();

  // ── Generic wrappers (mirror preload.js safeInvoke / safeSend / safeOn) ─
  function safeInvoke(channel, ...args) {
    if (!VALID_CHANNELS.invoke.includes(channel)) {
      return Promise.reject(new Error(`Blocked IPC invoke: ${channel}`));
    }
    // Map positional args into Tauri's { arg, arg2, ... } shape. The Rust
    // command signatures use snake_case parameter names; we pass them as a
    // flat object so the call sites stay 1:1 with the Electron preload.
    const argObj = {};
    args.forEach((value, idx) => {
      const key = idx === 0 ? "arg" : `arg${idx + 1}`;
      argObj[key] = value;
    });
    return invoke(channel, args.length === 0 ? undefined : argObj);
  }

  function safeSend(channel, ...args) {
    if (!VALID_CHANNELS.send.includes(channel)) return;
    // Tauri commands are async; we fire-and-forget to mirror the Electron
    // `ipcRenderer.send` semantics.
    const argObj = {};
    args.forEach((value, idx) => {
      const key = idx === 0 ? "arg" : `arg${idx + 1}`;
      argObj[key] = value;
    });
    invoke(channel, args.length === 0 ? undefined : argObj).catch((err) => {
      console.warn(`[electronAPI] send ${channel} failed:`, err);
    });
  }

  function safeOn(channel, callback) {
    if (!VALID_CHANNELS.receive.includes(channel)) return () => {};
    let unlisten = null;
    let cancelled = false;
    listen(channel, (event) => callback(event.payload))
      .then((fn) => {
        unlisten = fn;
        if (cancelled) {
          fn();
          unlisten = null;
        }
      })
      .catch((err) => console.warn(`[electronAPI] listen ${channel} failed:`, err));
    return () => {
      cancelled = true;
      if (unlisten) {
        unlisten();
        unlisten = null;
      }
    };
  }

  // ── Expose the same surface as electron/preload.js ──────────────────────
  window.electronAPI = {
    // ── Invoke ──────────────────────────────────────────────────────────
    getAppInfo: () => safeInvoke("get-app-info"),
    openExternal: (url) => safeInvoke("open-external", url),
    getDataDir: () => safeInvoke("get-data-dir"),
    restartServer: () => safeInvoke("restart-server"),
    getAppVersion: () => safeInvoke("get-app-version"),

    // ── Auto-Update ─────────────────────────────────────────────────────
    checkForUpdates: () => safeInvoke("check-for-updates"),
    downloadUpdate: () => safeInvoke("download-update"),
    installUpdate: () => safeInvoke("install-update"),

    // ── Autostart ───────────────────────────────────────────────────────
    getAutostartStatus: () => safeInvoke("get-autostart-status"),
    enableAutostart: () => safeInvoke("enable-autostart"),
    disableAutostart: () => safeInvoke("disable-autostart"),

    // ── Send (fire-and-forget window controls) ──────────────────────────
    minimizeWindow: () => safeSend("window-minimize"),
    maximizeWindow: () => safeSend("window-maximize"),
    closeWindow: () => safeSend("window-close"),

    // ── Receive (event listeners — return disposer functions) ───────────
    onServerStatus: (callback) => safeOn("server-status", callback),
    onPortChanged: (callback) => safeOn("port-changed", callback),
    onUpdateStatus: (callback) => safeOn("update-status", callback),

    // ── Web-cookie login ────────────────────────────────────────────────
    startLogin: (providerId, options) => safeInvoke("login:start", providerId, options),
    cancelLogin: () => safeInvoke("login:cancel"),
    getLoginStatus: () => safeInvoke("login:status"),
    onLoginStatus: (callback) => safeOn("login:status", callback),

    // ── Static properties (mirror preload.js) ───────────────────────────
    isElectron: true,
    platform: (function () {
      // Tauri exposes the OS via __TAURI__.os.platform(); fall back to
      // navigator.platform so the shim works before __TAURI__ loads.
      try {
        const t = TAURI();
        if (t && t.os && typeof t.os.platform === "function") {
          // Synchronous only in newer Tauri 2 builds; otherwise this returns
          // a Promise. The Electron preload exposed a sync `process.platform`
          // string, so we coerce: if we get a Promise, fall back to
          // navigator.platform.
          const p = t.os.platform();
          if (typeof p === "string") return p;
        }
      } catch {}
      const np = (window.navigator.platform || "").toLowerCase();
      if (np.startsWith("mac")) return "darwin";
      if (np.startsWith("win")) return "win32";
      if (np.includes("linux")) return "linux";
      return "unknown";
    })(),
  };

  // Also expose window.remoteServerPrompt for the remoteServerPrompt.html
  // window (mirrors remoteServerPromptPreload.js).
  window.remoteServerPrompt = {
    getInitialUrl: () => invoke("get_initial_url"),
    submit: (url) => invoke("submit_remote_url", { url }).catch(() => {}),
    cancel: () => invoke("cancel_remote_prompt").catch(() => {}),
  };

  // Mark that we're running under Tauri for any renderer-side feature
  // detection that wants to distinguish the two desktop shells.
  window.isTauri = true;
})();
