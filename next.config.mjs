import createNextIntlPlugin from "next-intl/plugin";
import { createMDX } from "fumadocs-mdx/next";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { mitmManagerAliasFor } from "./scripts/build/mitm-stub-flag.mjs";
import { normalizeBasePath } from "./scripts/build/normalizeBasePath.mjs";

const withNextIntl = createNextIntlPlugin("./src/i18n/request.ts");
const distDir = process.env.NEXT_DIST_DIR || ".build/next";
const projectRoot = dirname(fileURLToPath(import.meta.url));
const scriptSrc =
  process.env.NODE_ENV === "development"
    ? "script-src 'self' 'unsafe-inline' 'unsafe-eval' blob: https://static.cloudflareinsights.com"
    : "script-src 'self' 'unsafe-inline' 'unsafe-eval' blob: https://static.cloudflareinsights.com";
const contentSecurityPolicy = [
  "default-src 'self'",
  "base-uri 'self'",
  "object-src 'none'",
  "frame-ancestors 'none'",
  "form-action 'self'",
  scriptSrc,
  "style-src 'self' 'unsafe-inline' https://fonts.googleapis.com",
  "font-src 'self' https://fonts.gstatic.com data:",
  "img-src 'self' data: blob: https:",
  "media-src 'self' data: blob:",
  // `ws:` is permitted scheme-wide (mirroring the bare `wss:` already allowed) so the
  // dashboard can open `ws://<lan-or-tailscale-host>:*` to its own Live WS server when
  // OmniRoute is reached from a non-loopback host. Same-origin HTTP fetches stay covered
  // by `'self'`; the loopback origins remain listed explicitly for clarity. (#5083)
  "connect-src 'self' http://localhost:* http://127.0.0.1:* ws://localhost:* ws://127.0.0.1:* https: ws: wss:",
  "worker-src 'self' blob:",
  "manifest-src 'self'",
].join("; ");
const securityHeaders = [
  {
    key: "Content-Security-Policy",
    value: contentSecurityPolicy,
  },
  {
    key: "X-Frame-Options",
    value: "DENY",
  },
  {
    key: "X-Content-Type-Options",
    value: "nosniff",
  },
  {
    key: "Referrer-Policy",
    value: "strict-origin-when-cross-origin",
  },
  {
    key: "Permissions-Policy",
    value: "camera=(), microphone=(), geolocation=(), payment=(), usb=(), serial=()",
  },
  {
    key: "Strict-Transport-Security",
    value: "max-age=63072000; includeSubDomains; preload",
  },
];

function isNextIntlExtractorDynamicImportWarning(warning) {
  const message = typeof warning === "string" ? warning : warning?.message || "";
  const resource = warning?.module?.resource || warning?.file || "";
  const target = "next-intl/dist/esm/production/extractor/format/index.js";
  return (
    resource.includes(target) &&
    (message.includes("import(t)") || message.includes("dependency is an expression"))
  );
}

// OMNIROUTE_BUILD_PROFILE=minimal physically removes four optional privileged
// modules (MITM cert install, Zed keychain import, Cloud Sync, 9router
// installer) from the built bundle by aliasing them to feature-disabled stubs.
// The resulting artifact is intended to be published as `omniroute-secure`
// for security-sensitive environments. See docs/security/SOCKET_DEV_FINDINGS.md.
const isMinimalBuild = process.env.OMNIROUTE_BUILD_PROFILE === "minimal";

const minimalBuildAliases = isMinimalBuild
  ? {
      "@/mitm/cert/install": "./src/mitm/cert/install.stub.ts",
      "@/lib/zed-oauth/keychain-reader": "./src/lib/zed-oauth/keychain-reader.stub.ts",
      "@/lib/cloudSync": "./src/lib/cloudSync.stub.ts",
      "@/lib/services/installers/ninerouter": "./src/lib/services/installers/ninerouter.stub.ts",
    }
  : {};

function readTimeoutMs(...values) {
  for (const value of values) {
    const normalized = typeof value === "string" ? value.trim() : value;
    if (normalized == null || normalized === "") continue;
    const parsed = Number(normalized);
    if (Number.isFinite(parsed) && parsed >= 0) return Math.floor(parsed);
  }
  return 600_000;
}

/** @type {import('next').NextConfig} */
const nextConfig = {
  // Opt-in subpath deployment behind a reverse proxy (e.g. nginx/Caddy serving
  // OmniRoute under https://host/omniroute/). Empty by default so root-path
  // deployments are unaffected. Next.js strips this prefix from `pathname`
  // before route matching, so authz classification (classifyRoute/isLocalOnlyPath)
  // keeps operating on un-prefixed paths — see src/server/authz/pipeline.ts for
  // the two redirect call sites that re-add it via `request.nextUrl.basePath`.
  basePath: normalizeBasePath(process.env.OMNIROUTE_BASE_PATH),
  // Client-visible mirror of basePath for fetch/EventSource rewriting under reverse
  // proxies (installBasePathFetch), and for client display helpers (useDisplayBaseUrl)
  // that append the subpath to window.location.origin when building curl/endpoint
  // examples. Empty by default (root deploys unchanged).
  env: {
    NEXT_PUBLIC_OMNIROUTE_BASE_PATH: normalizeBasePath(process.env.OMNIROUTE_BASE_PATH),
  },
  distDir,
  // Turbopack config: redirect native modules to stubs at build time
  turbopack: {
    root: projectRoot,
    resolveAlias: {
      // @/mitm/manager → stub ONLY where the runtime can't run the MITM stack
      // (Docker sets OMNIROUTE_MITM_STUB=1 — #3390 graceful degradation). The
      // alias used to be unconditional, which was fine while Docker was the
      // only Turbopack consumer — but the v3.8.45 bundler-default flip shipped
      // the stub to every npm/Electron/VPS artifact and broke Agent Bridge
      // start for all non-Docker users (#6344). See scripts/build/mitm-stub-flag.mjs.
      ...mitmManagerAliasFor(process.env),
      ...minimalBuildAliases,
    },
    // src/lib/agentSkills/generator.ts builds its fs base path from a runtime
    // `outputDir` parameter (`path.join(process.cwd(), outputDir)`), which is
    // NOT a compile-time literal, so Turbopack's build-time file-tracing
    // analyzer can't statically narrow the several dynamic readdirSync/rmSync/
    // readFileSync/writeFileSync call sites a few lines below and falls back
    // to an "Overly broad patterns... matches N files" warning — once per
    // Next.js entry point that imports the module (/api/agent-skills/generate,
    // /api/cli-tools/pi-settings). The fs access is legitimate and bounded
    // (skills/<id>/SKILL.md, ~48 known IDs), so this is a known-benign,
    // expected diagnostic — suppress it here rather than fight the analyzer,
    // mirroring the isNextIntlExtractorDynamicImportWarning precedent below
    // for the webpack path. (#6582)
    // open-sse/services/compression/ruleLoader.ts and
    // .../engines/rtk/filterLoader.ts both define an identical
    // getModuleDir() helper that walks up directories via
    // path.resolve(anchor) + fs.existsSync(...) in a loop with a
    // non-literal argument — the same dynamic-path fs access pattern as
    // the agentSkills case above, but not covered by that narrower
    // allowlist glob, so the "Overly broad patterns..." warning kept
    // firing (610 times, once per entry point transitively importing the
    // compression module). Same known-benign, bounded fs access;
    // suppressed here rather than fought. (#7051, follow-up to #6582)
    ignoreIssue: [
      {
        path: "**/src/lib/agentSkills/**",
        description: /Overly broad patterns can lead to build performance issues/,
      },
      {
        path: "**/open-sse/services/compression/**",
        description: /Overly broad patterns can lead to build performance issues/,
      },
    ],
  },
  output: "export",
  compress: true,
  productionBrowserSourceMaps: false,
  // OmniRoute is a proxy for AI APIs — request bodies routinely include
  // multi-MB payloads (vision models, image edits, base64-encoded files,
  // long chat histories with embedded images). Next.js's Server Action
  // handler intercepts POSTs with multipart/form-data or
  // x-www-form-urlencoded content-types and enforces a 1 MB cap that
  // surfaces as a 413 with a confusing "Server Actions" hint, even on
  // pure route handlers. 50 MB matches what most upstream LLM providers
  // accept for image-bearing requests; tune via env if a deployment needs
  // more.
  experimental: {
    serverActions: {
      bodySizeLimit: process.env.OMNIROUTE_SERVER_ACTIONS_BODY_LIMIT || "50mb",
    },
    // Reduce peak heap during production builds (Next.js 15+).
    webpackMemoryOptimizations: true,
    // Run webpack in a separate Node worker, lowering main-process memory.
    webpackBuildWorker: true,
    // Next.js proxy (middleware) has a default 10MB body clone limit. File
    // uploads (OpenAI-compatible /v1/files) routinely exceed this. Match the
    // 512 MB server-side cap; tune via env if needed.
    proxyClientMaxBodySize: process.env.NEXT_PROXY_BODY_LIMIT || "512mb",
    // Next's internal router proxy defaults to 30s when this is unset. OmniRoute
    // can legitimately hold non-streaming chat requests open for minutes while an
    // upstream provider finishes, so reuse the existing request-timeout knobs.
    proxyTimeout: readTimeoutMs(process.env.REQUEST_TIMEOUT_MS, process.env.FETCH_TIMEOUT_MS),
    // PR-2 of diegosouzapw/OmniRoute#3932: tree-shake barrel re-exports so
    // route bundles don't pull in 14 locale files, every lucide-react icon,
    // or the full date-fns surface when only one helper is used.
    //
    // NOTE: this list must only contain EXTERNAL barrel libraries. Do NOT add
    // the internal `@omniroute/open-sse` workspace here: optimizePackageImports
    // makes Next.js resolve every export of the package's barrel at build time,
    // and open-sse's `index.ts` re-exports the entire streaming engine
    // (executors/translators/services/handlers/mcp-server — thousands of
    // modules). Combined with the #3501 god-file splits (which multiplied the
    // re-export edges), this drove the webpack production pass into a heap
    // runaway that OOM'd even at a 28 GB --max-old-space-size (RSS pinned at the
    // ceiling in a GC death-spiral). Removing it keeps the build's heap bounded.
    // optimizePackageImports is designed for external libs, not workspaces.
    optimizePackageImports: [
      "lobehub/icons",
      "@lobehub/icons",
      "lucide-react",
      "date-fns",
      "lodash",
      "lodash-es",
      "material-symbols",
      "next-intl",
    ],
  },
  outputFileTracingRoot: projectRoot,
  outputFileTracingIncludes: {
    // Migration SQL and compression rule/filter JSON files are read via fs at
    // runtime and are NOT always auto-traced by webpack/turbopack.
    "/*": [
      "./src/lib/db/migrations/**/*",
      "./src/mitm/server.cjs",
      "./open-sse/services/compression/engines/rtk/filters/**/*.json",
      "./open-sse/services/compression/rules/**/*.json",
      "./open-sse/lib/sha3_wasm_bg.wasm",
      "./open-sse/lib/deepseek-pow-solver.cjs",
      // sql.js WASM is loaded at runtime by the sqljsAdapter fallback tier
      // (better-sqlite3 → node:sqlite → sql.js). Next traces sql-wasm.js but can
      // omit the runtime sql-wasm.wasm asset from the standalone bundle.
      "./node_modules/sql.js/dist/sql-wasm.wasm",
    ],
  },
  outputFileTracingExcludes: {
    // Planning/task docs are not runtime assets and can break standalone copies
    // when broad fs/path tracing pulls the whole repository into the NFT graph.
    "/*": [
      "./.git/**/*",
      "./_tasks/**/*",
      "./_references/**/*",
      "./_ideia/**/*",
      "./_mono_repo/**/*",
      "./coverage/**/*",
      "./test-results/**/*",
      "./playwright-report/**/*",
      "./app.__qa_backup/**/*",
      "./tests/**/*",
      "./logs/**/*",
    ],
  },
  serverExternalPackages: [
    "pino",
    "pino-pretty",
    "thread-stream",
    "pino-abstract-transport",
    "better-sqlite3",
    // sql.js WASM is resolved at runtime via createRequire(); Next's static
    // analysis can't follow _require.resolve("sql.js/package.json") and spams
    // build warnings.  Externalizing silences them without changing behaviour.
    "sql.js",
    // sqlite-vec ships a native vec0.so loaded at runtime via createRequire().
    // Turbopack otherwise tries to bundle the .so and fails with "Unknown module
    // type"; externalizing it keeps the require at runtime (like better-sqlite3).
    // See issue #3066.
    "sqlite-vec",
    "node-machine-id",
    "keytar",
    "wreq-js",
    "zod",
    "tls-client-node",
    "koffi",
    "tough-cookie",
    "@ngrok/ngrok",
    "@huggingface/transformers",
    // copilot-m365-web.ts imports 'ws' as a client-side WebSocket. When bundled,
    // ws cannot resolve its 'bufferutil' native addon (frame masking) and throws
    // TypeError: b.mask is not a function on the first outgoing frame, causing
    // every chat request to time out at the stream-readiness watchdog. (#6062)
    "ws",
    "bufferutil",
    "utf-8-validate",
    "child_process",
    "fs",
    "path",
    "os",
    "crypto",
    "net",
    "tls",
    "http",
    "https",
    "stream",
    "buffer",
    "util",
    "process",
  ],
  transpilePackages: ["@omniroute/open-sse", "@lobehub/icons", "fumadocs-ui", "fumadocs-core"],
  allowedDevOrigins: ["localhost", "127.0.0.1", "192.168.0.250"],
  typescript: {
    // TODO: Re-enable after fixing all sub-component useTranslations scope issues
    ignoreBuildErrors: true,
  },
  webpack(config, { webpack }) {
    config.ignoreWarnings = [
      ...(config.ignoreWarnings || []),
      isNextIntlExtractorDynamicImportWarning,
    ];
    config.optimization = config.optimization || {};
    config.optimization.splitChunks = {
      ...config.optimization.splitChunks,
      cacheGroups: {
        ...(config.optimization.splitChunks?.cacheGroups || {}),
        recharts: {
          test: /[\\/]node_modules[\\/]recharts[\\/]/,
          name: "vendor-recharts",
          chunks: "all",
          priority: 20,
        },
        lobeIcons: {
          test: /[\\/]node_modules[\\/]@lobehub[\\/]icons[\\/]/,
          name: "vendor-lobe-icons",
          chunks: "all",
          priority: 20,
        },
        monaco: {
          test: /[\\/]node_modules[\\/]monaco-editor[\\/]/,
          name: "vendor-monaco",
          chunks: "all",
          priority: 20,
        },
        xyflow: {
          test: /[\\/]node_modules[\\/]@xyflow[\\/]/,
          name: "vendor-xyflow",
          chunks: "all",
          priority: 20,
        },
        mermaid: {
          test: /[\\/]node_modules[\\/]mermaid[\\/]/,
          name: "vendor-mermaid",
          chunks: "all",
          priority: 20,
        },
        // PR-2 of diegosouzapw/OmniRoute#3932: isolate the heavy long-tail
        // vendor chunks that only some routes actually need, so dashboard
        // pages don't pay for the docs bundle (or vice versa).
        nextIntl: {
          test: /[\\/]node_modules[\\/]next-intl[\\/]/,
          name: "vendor-next-intl",
          chunks: "all",
          priority: 25,
        },
        fumadocs: {
          test: /[\\/]node_modules[\\/](fumadocs-ui|fumadocs-core|fumadocs-mdx)[\\/]/,
          name: "vendor-fumadocs",
          chunks: "all",
          priority: 20,
        },
        comboGraph: {
          test: /[\\/]node_modules[\\/]@?dagre[\\/]|[\\/]node_modules[\\/]@?elkjs[\\/]/,
          name: "vendor-combo-graph",
          chunks: "all",
          priority: 20,
        },
      },
    };

    if (isMinimalBuild) {
      // Mirror the turbopack.resolveAlias entries for webpack-built artifacts.
      // NormalModuleReplacementPlugin swaps the real module for a stub before
      // webpack resolves it, so the privileged source files are never compiled
      // into the standalone output.
      const replacements = [
        [/^@\/mitm\/cert\/install$/, "./src/mitm/cert/install.stub.ts"],
        [/^@\/lib\/zed-oauth\/keychain-reader$/, "./src/lib/zed-oauth/keychain-reader.stub.ts"],
        [/^@\/lib\/cloudSync$/, "./src/lib/cloudSync.stub.ts"],
        [
          /^@\/lib\/services\/installers\/ninerouter$/,
          "./src/lib/services/installers/ninerouter.stub.ts",
        ],
      ];
      for (const [pattern, stubPath] of replacements) {
        config.plugins.push(
          new webpack.NormalModuleReplacementPlugin(pattern, (resource) => {
            resource.request = stubPath;
          })
        );
      }
    }

    return config;
  },
  images: {
    unoptimized: true,
  },

};

const withMDX = createMDX();

export default withMDX(withNextIntl(nextConfig));
