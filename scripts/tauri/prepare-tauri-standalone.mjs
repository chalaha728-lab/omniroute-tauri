#!/usr/bin/env node
/**
 * prepare-tauri-standalone.mjs
 *
 * Assembles the Next.js standalone bundle into the layout expected by the
 * Tauri Rust shell:
 *
 *   src-tauri/resources/next-server/
 *     ├── server.js                ← Next.js standalone server entry
 *     ├── server-ws.mjs            ← peer-stamp wrapper (if present)
 *     ├── node_modules/            ← standalone deps (no symlinks — materialized)
 *     ├── .next/                   ← static + server chunks
 *     └── public/                  ← static assets
 *
 * This is the Tauri equivalent of `scripts/build/prepare-electron-standalone.mjs`.
 * The Electron-specific ABI rebuild of better-sqlite3 is NOT needed here —
 * the Node sidecar runs the same Node.js ABI the bundle was compiled for.
 *
 * Run via `npm run tauri:build` (beforeBuildCommand).
 */

import { cpSync, existsSync, lstatSync, mkdirSync, rmSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const ROOT = join(__dirname, "..", "..");

const NEXT_DIST_DIR = process.env.NEXT_DIST_DIR || ".build/next";
const DIST_DIR = join(ROOT, NEXT_DIST_DIR);
const STANDALONE_DIR = join(DIST_DIR, "standalone");
const TAURI_RESOURCES = join(ROOT, "src-tauri", "resources", "next-server");

function resolveStandaloneBundleDir() {
  if (existsSync(join(STANDALONE_DIR, "server.js"))) {
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

function assertBundleIsPackagable(bundleDir) {
  const nodeModulesPath = join(bundleDir, "node_modules");
  if (!existsSync(nodeModulesPath)) return;
  if (lstatSync(nodeModulesPath).isSymbolicLink()) {
    throw new Error(
      [
        "Next standalone emitted app/node_modules as a symlink.",
        "Tauri's resource bundling preserves symlinks, which would make the packaged app",
        "depend on the original build machine path at runtime.",
        `Offending path: ${nodeModulesPath}`,
        "Use a real node_modules directory in the build worktree before packaging Tauri.",
      ].join("\n")
    );
  }
}

const bundleDir = resolveStandaloneBundleDir();
assertBundleIsPackagable(bundleDir);

console.log(`[tauri] staging Next.js standalone from ${bundleDir} → ${TAURI_RESOURCES}`);
rmSync(TAURI_RESOURCES, { recursive: true, force: true });
mkdirSync(dirname(TAURI_RESOURCES), { recursive: true });

// Copy the standalone server bundle
cpSync(bundleDir, TAURI_RESOURCES, { recursive: true, dereferenceSymlinks: true });

// Copy .next/static and public (Next.js standalone doesn't include these by default)
const staticSrc = join(DIST_DIR, "static");
const staticDst = join(TAURI_RESOURCES, ".next", "static");
if (existsSync(staticSrc)) {
  mkdirSync(dirname(staticDst), { recursive: true });
  cpSync(staticSrc, staticDst, { recursive: true });
}

const publicSrc = join(ROOT, "public");
const publicDst = join(TAURI_RESOURCES, "public");
if (existsSync(publicSrc)) {
  cpSync(publicSrc, publicDst, { recursive: true });
}

console.log(`[tauri] standalone bundle staged: ${TAURI_RESOURCES}`);
