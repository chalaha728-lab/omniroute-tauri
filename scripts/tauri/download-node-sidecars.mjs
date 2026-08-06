#!/usr/bin/env node
/**
 * download-node-sidecars.mjs
 *
 * Downloads official Node.js binaries for every Tauri target triple and
 * places them at the paths expected by `tauri.conf.json → bundle.externalBin`:
 *
 *   src-tauri/binaries/node-<target-triple>[.exe]
 *
 * Tauri appends the target-triple suffix automatically when resolving the
 * sidecar at runtime. We must NOT include a `.exe` extension on Windows
 * (Tauri adds it).
 *
 * Targets:
 *   - x86_64-pc-windows-msvc       (Windows x64)
 *   - aarch64-apple-darwin         (macOS Apple Silicon)
 *   - x86_64-apple-darwin          (macOS Intel)
 *   - x86_64-unknown-linux-gnu     (Linux x64)
 *   - aarch64-unknown-linux-gnu    (Linux ARM64)
 *
 * Run via `npm run tauri:sidecars` before `npm run tauri:build`.
 */

import { createWriteStream, existsSync, mkdirSync, renameSync, rmSync, createReadStream } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { pipeline } from "node:stream/promises";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const BINARIES_DIR = join(__dirname, "..", "..", "src-tauri", "binaries");

const NODE_VERSION = process.env.NODE_SIDECAR_VERSION || "v22.11.0";

const TARGETS = [
  // [target-triple, nodejs dist subdir, archive name template, binary path inside archive]
  {
    triple: "x86_64-pc-windows-msvc",
    distSubdir: "win-x64",
    archive: `node-${NODE_VERSION}-win-x64.zip`,
    binaryInside: `node-${NODE_VERSION}-win-x64/node.exe`,
    outName: "node-x86_64-pc-windows-msvc.exe",
  },
  {
    triple: "aarch64-apple-darwin",
    distSubdir: "darwin-arm64",
    archive: `node-${NODE_VERSION}-darwin-arm64.tar.gz`,
    binaryInside: `node-${NODE_VERSION}-darwin-arm64/bin/node`,
    outName: "node-aarch64-apple-darwin",
  },
  {
    triple: "x86_64-apple-darwin",
    distSubdir: "darwin-x64",
    archive: `node-${NODE_VERSION}-darwin-x64.tar.gz`,
    binaryInside: `node-${NODE_VERSION}-darwin-x64/bin/node`,
    outName: "node-x86_64-apple-darwin",
  },
  {
    triple: "x86_64-unknown-linux-gnu",
    distSubdir: "linux-x64",
    archive: `node-${NODE_VERSION}-linux-x64.tar.xz`,
    binaryInside: `node-${NODE_VERSION}-linux-x64/bin/node`,
    outName: "node-x86_64-unknown-linux-gnu",
  },
  {
    triple: "aarch64-unknown-linux-gnu",
    distSubdir: "linux-arm64",
    archive: `node-${NODE_VERSION}-linux-arm64.tar.xz`,
    binaryInside: `node-${NODE_VERSION}-linux-arm64/bin/node`,
    outName: "node-aarch64-unknown-linux-gnu",
  },
];

async function download(url, dest) {
  console.log(`  ↓ ${url}`);
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok || !res.body) {
    throw new Error(`HTTP ${res.status} ${res.statusText} for ${url}`);
  }
  await pipeline(res.body, createWriteStream(dest));
}

function extract(archivePath, binaryInside, outPath) {
  const tmpExtract = join(tmpdir(), `omniroute-node-extract-${Date.now()}`);
  mkdirSync(tmpExtract, { recursive: true });

  if (archivePath.endsWith(".zip")) {
    // unzip -o archive -d tmpExtract
    spawnSync("unzip", ["-o", archivePath, "-d", tmpExtract], { stdio: "inherit" });
  } else if (archivePath.endsWith(".tar.gz")) {
    spawnSync("tar", ["-xzf", archivePath, "-C", tmpExtract], { stdio: "inherit" });
  } else if (archivePath.endsWith(".tar.xz")) {
    spawnSync("tar", ["-xJf", archivePath, "-C", tmpExtract], { stdio: "inherit" });
  } else {
    throw new Error(`Unknown archive format: ${archivePath}`);
  }

  const extracted = join(tmpExtract, binaryInside);
  if (!existsSync(extracted)) {
    throw new Error(`Expected binary not found in archive: ${extracted}`);
  }
  renameSync(extracted, outPath);
  rmSync(tmpExtract, { recursive: true, force: true });
}

mkdirSync(BINARIES_DIR, { recursive: true });

// Parse --only <triple> (used by CI to download just the current platform)
const onlyIdx = process.argv.indexOf("--only");
const onlyTriple = onlyIdx >= 0 ? process.argv[onlyIdx + 1] : null;
// Parse --platform <name> alias (e.g. linux, macos, windows)
const platIdx = process.argv.indexOf("--platform");
const platName = platIdx >= 0 ? process.argv[platIdx + 1]?.toLowerCase() : null;

const targetsToBuild = TARGETS.filter((t) => {
  if (onlyTriple) return t.triple === onlyTriple;
  if (platName === "linux") return t.triple.includes("linux");
  if (platName === "macos" || platName === "mac") return t.triple.includes("darwin");
  if (platName === "windows" || platName === "win") return t.triple.includes("windows");
  return true; // no filter → all platforms
});

if (onlyTriple || platName) {
  console.log(`[tauri:sidecars] filtering to: ${targetsToBuild.map((t) => t.triple).join(", ")}`);
}

for (const target of targetsToBuild) {
  const outPath = join(BINARIES_DIR, target.outName);
  if (existsSync(outPath)) {
    console.log(`✓ ${target.triple} — already present at ${target.outName}`);
    continue;
  }

  console.log(`→ ${target.triple}`);
  const url = `https://nodejs.org/dist/${NODE_VERSION}/${target.archive}`;
  const archivePath = join(BINARIES_DIR, target.archive);

  if (!existsSync(archivePath)) {
    await download(url, archivePath);
  }
  extract(archivePath, target.binaryInside, outPath);
  rmSync(archivePath, { force: true });

  // Make executable on POSIX
  if (!target.outName.endsWith(".exe")) {
    spawnSync("chmod", ["+x", outPath]);
  }
  console.log(`  ✓ extracted → ${target.outName}`);
}

console.log(`\n✓ Node.js sidecars ready in ${BINARIES_DIR}`);
