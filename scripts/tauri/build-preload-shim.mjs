/**
 * Bundles src-tauri/preload-shim.ts into a single IIFE string and writes it
 * to src-tauri/preload-shim.iife.js — Tauri loads this file as an
 * initialization script (set in tauri.conf.json → app.windows[].initializationScript).
 *
 * Run as part of `npm run tauri:dev` / `npm run tauri:build` via the
 * beforeDevCommand / beforeBuildCommand hooks.
 */

import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { build } from "esbuild";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const ROOT = join(__dirname, "..", "..");
const SRC = join(ROOT, "src-tauri", "preload-shim.ts");
const OUT = join(ROOT, "src-tauri", "preload-shim.iife.js");

mkdirSync(dirname(OUT), { recursive: true });

await build({
  entryPoints: [SRC],
  bundle: true,
  format: "iife",
  platform: "browser",
  target: ["es2022", "chrome110", "safari16", "firefox110"],
  outfile: OUT,
  // Drop the `export {}` at the end — invalid in IIFE
  footer: { js: "" },
  logLevel: "info",
});

// esbuild emits `export {}` which is invalid in an IIFE context — strip it.
const compiled = readFileSync(OUT, "utf8");
const stripped = compiled.replace(/export\s*\{\s*\}\s*;?\s*$/m, "");
writeFileSync(OUT, stripped);

console.log(`[tauri:preload-shim] bundled → ${OUT} (${stripped.length} bytes)`);
