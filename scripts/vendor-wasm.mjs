#!/usr/bin/env node
/**
 * Vendor the in-browser WASM runtimes so the UI serves them same-origin instead
 * of from cdn.jsdelivr.net. Removing the runtime CDN dependency lets the CSP drop
 * the third-party `script-src` token (see `default_csp_self_hosted` in
 * src/config/server.rs) and makes the Python/SQL tools work offline.
 *
 * Outputs (both gitignored via ui/public/wasm/.gitignore; Vite copies public/ ->
 * dist/ so they get embedded by the `embed-ui` rust-embed folder):
 *   ui/public/wasm/pyodide/  - Pyodide core + the matplotlib/data-science wheel closure
 *   ui/public/wasm/duckdb/   - DuckDB mvp + eh wasm bundles and their workers
 *
 * Build-time vendoring from jsDelivr is intentional and fine — it is the *runtime*
 * CDN dependency we are eliminating. Run automatically via the `prebuild` npm
 * script before `vite build`; re-runs are skipped when the marker version matches
 * (set FORCE_VENDOR=1 to re-download).
 *
 * Integrity: core files + pyodide-lock.json are verified against the committed
 * scripts/pyodide-314.sha256 manifest (generated on first run — commit it). Each
 * wheel is verified against the sha256 the lock file itself records.
 */
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync, copyFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const ROOT_DIR = dirname(SCRIPT_DIR);

const PYODIDE_VERSION = "314.0.0";
const PYODIDE_CDN = `https://cdn.jsdelivr.net/pyodide/v${PYODIDE_VERSION}/full/`;
const PYODIDE_OUT = join(ROOT_DIR, "ui/public/wasm/pyodide");
const DUCKDB_DIST = join(ROOT_DIR, "ui/node_modules/@duckdb/duckdb-wasm/dist");
const DUCKDB_OUT = join(ROOT_DIR, "ui/public/wasm/duckdb");
const MANIFEST = join(SCRIPT_DIR, `pyodide-${PYODIDE_VERSION.split(".")[0]}.sha256`);
const MARKER = join(PYODIDE_OUT, ".vendored-version");

// Pyodide core runtime files (not listed in pyodide-lock.json's package map).
const CORE_FILES = [
  "pyodide.mjs",
  "pyodide.asm.mjs",
  "pyodide.asm.wasm",
  "python_stdlib.zip",
  "pyodide-lock.json",
];

// Top-level packages to vendor. MUST stay in sync with the `cExtPackages` list in
// ui/src/services/pyodide/pyodideWorker.ts (plus micropip, used for pure-Python
// installs). Their full transitive dependency closure is resolved from the lock
// file — do not hand-list deps; a missing transitive wheel makes loadPackage 404.
const PACKAGES = ["numpy", "pandas", "scipy", "matplotlib", "scikit-learn", "pillow", "micropip"];

// DuckDB bundles. Skip the `coi` bundle: Hadrian sets no COOP/COEP headers, so the
// browser is never cross-origin-isolated and selectBundle never picks it.
const DUCKDB_FILES = [
  "duckdb-mvp.wasm",
  "duckdb-eh.wasm",
  "duckdb-browser-mvp.worker.js",
  "duckdb-browser-eh.worker.js",
];

const FORCE = process.env.FORCE_VENDOR === "1";
const sha256 = (buf) => createHash("sha256").update(buf).digest("hex");

async function fetchBuffer(url) {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`GET ${url} -> ${res.status} ${res.statusText}`);
  return Buffer.from(await res.arrayBuffer());
}

/** Resolve the transitive dependency closure of `names` from the lock package map. */
function resolveClosure(packages, names) {
  const byLower = new Map(Object.keys(packages).map((k) => [k.toLowerCase(), k]));
  const seen = new Set();
  const stack = [...names];
  while (stack.length) {
    const key = byLower.get(stack.pop().toLowerCase());
    if (!key || seen.has(key)) continue;
    seen.add(key);
    for (const dep of packages[key].depends || []) stack.push(dep);
  }
  return [...seen];
}

async function vendorPyodide() {
  if (!FORCE && existsSync(MARKER) && readFileSync(MARKER, "utf8").trim() === PYODIDE_VERSION) {
    console.log(`[vendor] Pyodide ${PYODIDE_VERSION} already vendored — skipping (FORCE_VENDOR=1 to redo).`);
    return;
  }
  mkdirSync(PYODIDE_OUT, { recursive: true });

  const committed = existsSync(MANIFEST)
    ? Object.fromEntries(
        readFileSync(MANIFEST, "utf8")
          .split("\n")
          .filter(Boolean)
          .map((line) => {
            const [hash, name] = line.trim().split(/\s+/);
            return [name, hash];
          })
      )
    : null;
  const generated = {};

  // 1. Core files (incl. the lock file). Verify against the committed manifest.
  console.log(`[vendor] Downloading Pyodide ${PYODIDE_VERSION} core from ${PYODIDE_CDN}`);
  for (const file of CORE_FILES) {
    const buf = await fetchBuffer(PYODIDE_CDN + file);
    const hash = sha256(buf);
    if (committed && committed[file] && committed[file] !== hash) {
      throw new Error(
        `[vendor] checksum mismatch for ${file}: expected ${committed[file]}, got ${hash}. ` +
          `Supply-chain check failed — refusing to vendor altered assets.`
      );
    }
    generated[file] = hash;
    writeFileSync(join(PYODIDE_OUT, file), buf);
  }

  // 2. Wheel closure. Integrity comes from the lock file's own per-package sha256
  //    (the lock itself is anchored by the committed manifest above).
  const lock = JSON.parse(readFileSync(join(PYODIDE_OUT, "pyodide-lock.json"), "utf8"));
  const closure = resolveClosure(lock.packages, PACKAGES).sort();
  console.log(`[vendor] Downloading ${closure.length} wheels (closure of ${PACKAGES.join(", ")})`);
  await Promise.all(
    closure.map(async (name) => {
      const pkg = lock.packages[name];
      const buf = await fetchBuffer(PYODIDE_CDN + pkg.file_name);
      const hash = sha256(buf);
      if (pkg.sha256 && pkg.sha256 !== hash) {
        throw new Error(`[vendor] wheel checksum mismatch for ${pkg.file_name}: lock says ${pkg.sha256}, got ${hash}`);
      }
      writeFileSync(join(PYODIDE_OUT, pkg.file_name), buf);
    })
  );

  // 3. First-run: write the integrity manifest so CI/later runs can verify.
  if (!committed) {
    const body = CORE_FILES.map((f) => `${generated[f]}  ${f}`).join("\n") + "\n";
    writeFileSync(MANIFEST, body);
    console.log(`[vendor] Wrote ${MANIFEST} — commit this file (supply-chain integrity anchor).`);
  }

  writeFileSync(MARKER, PYODIDE_VERSION + "\n");
  console.log(`[vendor] Pyodide ${PYODIDE_VERSION}: ${CORE_FILES.length} core files + ${closure.length} wheels ✓`);
}

function vendorDuckdb() {
  if (!existsSync(DUCKDB_DIST)) {
    throw new Error(`[vendor] ${DUCKDB_DIST} not found — run \`pnpm install\` in ui/ first.`);
  }
  mkdirSync(DUCKDB_OUT, { recursive: true });
  for (const file of DUCKDB_FILES) {
    copyFileSync(join(DUCKDB_DIST, file), join(DUCKDB_OUT, file));
  }
  console.log(`[vendor] DuckDB: copied ${DUCKDB_FILES.length} bundle files (mvp + eh) ✓`);
}

await vendorPyodide();
vendorDuckdb();
