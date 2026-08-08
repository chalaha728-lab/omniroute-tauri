#!/usr/bin/env node
/**
 * prepare-tauri-standalone.mjs
 *
 * Prepares a Next.js standalone bundle for the **Tauri** desktop shell.
 *
 * This is a slimmed-down version of OmniRoute's `prepare-electron-standalone.mjs`:
 *   - KEEP:   assembleStandalone with sanitizePaths, patchTurbopackChunks,
 *             copyNatives, materializeSymlinks (all needed for any packaged app)
 *   - SKIP:   rebuildBetterSqlite3ForElectron  (Tauri uses system Node, not
 *             ELECTRON_RUN_AS_NODE, so the Node-ABI build from `npm install`
 *             is already correct)
 *   - SKIP:   removeNativeModules(keytar)      (same reason — keep Node-ABI build)
 *   - SKIP:   assertNoStaleHashedNatives       (we *want* the natives in
 *             .next/node_modules — they're the correct Node-ABI builds)
 *
 * Usage (run from the OmniRoute repo root after `npm run build`):
 *   node /path/to/prepare-tauri-standalone.mjs
 *
 * Output: `.build/tauri-standalone/` — copy this directory to
 * `src-tauri/resources/app/` in the Tauri repo before `npm run tauri:build`.
 */

import { existsSync, lstatSync, readdirSync, rmSync } from "node:fs";
import { basename, dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

// We import assembleStandalone from the OmniRoute source tree (the workflow
// copies this script into the OmniRoute checkout before running it).
import { assembleStandalone } from "./scripts/build/assembleStandalone.mjs";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const ROOT = __dirname; // OmniRoute repo root (script is copied to repo root)

const NEXT_DIST_DIR = process.env.NEXT_DIST_DIR || ".build/next";
const DIST_DIR = join(ROOT, NEXT_DIST_DIR);
const STANDALONE_DIR = join(DIST_DIR, "standalone");
const TAURI_STANDALONE_DIR = join(ROOT, ".build", "tauri-standalone");

// --- Resolve the standalone bundle dir (same logic as the Electron script) --

function resolveStandaloneBundleDir() {
  const directServer = join(STANDALONE_DIR, "server.js");
  if (existsSync(directServer)) {
    return STANDALONE_DIR;
  }

  const nestedCandidates = [
    join(STANDALONE_DIR, "projects", "OmniRoute"),
    join(STANDALONE_DIR, basename(ROOT)),
  ];

  for (const candidate of nestedCandidates) {
    if (existsSync(join(candidate, "server.js"))) {
      return candidate;
    }
  }

  throw new Error(
    `Standalone server bundle not found in ${STANDALONE_DIR}. Run \`npm run build\` first.`
  );
}

// --- Symlink guard (same as Electron — Tauri's resource bundler also chokes
//     on symlinked node_modules that point at build-machine absolute paths) ---

function assertBundleIsPackagable(bundleDir) {
  const nodeModulesPath = join(bundleDir, "node_modules");
  if (!existsSync(nodeModulesPath)) return;

  if (lstatSync(nodeModulesPath).isSymbolicLink()) {
    throw new Error(
      [
        "Next standalone emitted app/node_modules as a symlink.",
        "Tauri's resource bundler preserves symlinks, which would make the packaged app",
        "depend on the original build machine path at runtime.",
        "",
        `Offending path: ${nodeModulesPath}`,
        "Use a real node_modules directory in the build worktree before packaging.",
      ].join("\n")
    );
  }
}

// --- Main ---

process.on("uncaughtException", (error) => {
  const message = error instanceof Error ? error.message : String(error);
  console.error(`[tauri] failed to prepare standalone bundle: ${message}`);
  process.exitCode = 1;
});

const bundleDir = resolveStandaloneBundleDir();
assertBundleIsPackagable(bundleDir);

// Clean the stage dir before assembly
rmSync(TAURI_STANDALONE_DIR, { recursive: true, force: true });

// Shared assembly: standalone copy + .next/static + public + abs-path
// sanitization + natives/@swc/helpers + symlink materialization.
//
// All options mirror the Electron build EXCEPT we keep copyNatives=true
// (which copies the Node-ABI better-sqlite3.node built during `npm install`)
// and we do NOT subsequently rebuild it against the Electron ABI.
console.log("[tauri] assembling standalone bundle...");
assembleStandalone({
  distDir: DIST_DIR,
  outDir: TAURI_STANDALONE_DIR,
  projectRoot: ROOT,
  sanitizePaths: true,
  patchTurbopackChunks: true,
  copyNatives: true,
  materializeSymlinks: true,
});

console.log(
  `[tauri] prepared standalone bundle: ${relative(ROOT, TAURI_STANDALONE_DIR) || "."}`
);
console.log(`[tauri] copy this directory to src-tauri/resources/app/ in the Tauri repo`);

// Quick sanity check: verify server.js / server-ws.mjs exists in the output
const serverJs = join(TAURI_STANDALONE_DIR, "server.js");
const serverWs = join(TAURI_STANDALONE_DIR, "server-ws.mjs");
if (!existsSync(serverJs) && !existsSync(serverWs)) {
  throw new Error(
    `[tauri] neither server.js nor server-ws.mjs found in ${TAURI_STANDALONE_DIR} — build may be incomplete`
  );
}
console.log(`[tauri] server entry: ${existsSync(serverWs) ? "server-ws.mjs" : "server.js"}`);

// Verify better-sqlite3 native binary is present (Tauri uses system Node,
// so this Node-ABI build is correct — no Electron rebuild needed)
const sqliteNative = join(
  TAURI_STANDALONE_DIR,
  "node_modules",
  "better-sqlite3",
  "build",
  "Release",
  "better_sqlite3.node"
);
if (!existsSync(sqliteNative)) {
  console.warn(
    `[tauri] WARNING: better-sqlite3 native binary not found at ${sqliteNative} — the server will fall back to sql.js (slower, higher memory)`
  );
} else {
  console.log("[tauri] better-sqlite3 native binary: OK");
}
