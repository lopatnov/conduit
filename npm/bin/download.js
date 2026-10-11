#!/usr/bin/env node
/**
 * Conduit — postinstall downloader.
 *
 * Downloads the native binary for the current platform from GitHub Releases.
 * Runs automatically after `npm install`.
 *
 * Skipped when:
 *   - CONDUIT_SKIP_DOWNLOAD=1     (opt-out for custom installs)
 *   - npm lifecycle is "ci"       (avoid re-downloading in production installs
 *                                  where the binary is vendored or built locally)
 */

import { createWriteStream, existsSync, mkdirSync, chmodSync, unlinkSync, renameSync } from "node:fs";
import { get as httpsGet } from "node:https";
import { get as httpGet } from "node:http";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";

// --------------------------------------------------------------------------
// Skip conditions
// --------------------------------------------------------------------------
if (process.env.CONDUIT_SKIP_DOWNLOAD === "1") {
  process.exit(0);
}

// --------------------------------------------------------------------------
// Config
// --------------------------------------------------------------------------
const __dirname = dirname(fileURLToPath(import.meta.url));
const pkgPath = join(__dirname, "..", "package.json");
const pkg = JSON.parse(readFileSync(pkgPath, "utf8"));
const VERSION = pkg.version;
const REPO = "lopatnov/conduit";
const NATIVE_DIR = join(__dirname, "native");
// Test hook: serve the asset from another base URL. The checksum is still enforced.
const BASE_URL = process.env.CONDUIT_DOWNLOAD_BASE ||
  `https://github.com/${REPO}/releases/download/v${VERSION}`;

// --------------------------------------------------------------------------
// Platform → asset name
// --------------------------------------------------------------------------
function getAssetName() {
  const { platform, arch } = process;

  // Issue #239: npm ships the `full` feature bundle from the 2.0.0 line
  // onward (was `standard` through 1.x) — asset names gain a "-full" suffix,
  // matching release.yml's full-build job names exactly.
  const map = {
    "linux-x64":    "conduit-x86_64-unknown-linux-gnu-full",
    "linux-arm64":  "conduit-aarch64-unknown-linux-gnu-full",
    "darwin-x64":   "conduit-x86_64-apple-darwin-full",
    "darwin-arm64": "conduit-aarch64-apple-darwin-full",
    "win32-x64":    "conduit-x86_64-pc-windows-msvc-full.exe",
  };

  const key = `${platform}-${arch}`;
  const name = map[key];

  if (!name) {
    console.warn(
      `[conduit] Skipping binary download — unsupported platform: ${key}\n` +
      `Install from source: cargo install conduit-proxy`
    );
    process.exit(0);
  }

  return name;
}

// --------------------------------------------------------------------------
// Download helper (follows redirects, no external dependencies)
// --------------------------------------------------------------------------
const MAX_REDIRECTS = 10;

function download(url, dest) {
  return new Promise((resolve, reject) => {
    const file = createWriteStream(dest);
    let redirectCount = 0;

    function request(url) {
      (url.startsWith("http:") ? httpGet : httpsGet)(url, { headers: { "User-Agent": `conduit-npm/${VERSION}` } }, (res) => {
        if (res.statusCode === 301 || res.statusCode === 302 || res.statusCode === 307 || res.statusCode === 308) {
          if (++redirectCount > MAX_REDIRECTS) {
            file.close();
            try { unlinkSync(dest); } catch { /* ignore */ }
            reject(new Error(`Too many redirects (>${MAX_REDIRECTS}) for ${url}`));
            return;
          }
          if (!res.headers.location) {
            file.close();
            try { unlinkSync(dest); } catch { /* ignore */ }
            reject(new Error(`Redirect with no Location header from ${url}`));
            return;
          }
          // Follow redirect (resolve relative URLs against current URL).
          request(new URL(res.headers.location, url).href);
          return;
        }

        if (res.statusCode !== 200) {
          file.close();
          try { unlinkSync(dest); } catch { /* ignore */ }
          reject(new Error(`HTTP ${res.statusCode} for ${url}`));
          return;
        }

        const total = parseInt(res.headers["content-length"] || "0", 10);
        let received = 0;
        let lastPct = -1;

        res.on("data", (chunk) => {
          received += chunk.length;
          if (total > 0) {
            const pct = Math.floor((received / total) * 100);
            if (pct !== lastPct && pct % 10 === 0) {
              process.stdout.write(`\r[conduit] Downloading... ${pct}%`);
              lastPct = pct;
            }
          }
        });

        res.on("aborted", () => reject(new Error("connection closed before the download finished")));
        res.pipe(file);
        file.on("finish", () => {
          file.close(() => {
            process.stdout.write("\r[conduit] Downloading... done    \n");
            resolve();
          });
        });
      }).on("error", (err) => {
        file.close();
        try { unlinkSync(dest); } catch { /* ignore */ }
        reject(err);
      });
    }

    request(url);
  });
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function fail(msg) {
  console.error(`\n[conduit] ${msg}`);
  process.exit(1);
}

function loadExpectedChecksum(assetName) {
  let sums;
  try {
    sums = JSON.parse(readFileSync(join(__dirname, "checksums.json"), "utf8"));
  } catch {
    fail("checksums.json is missing from this package, so the downloaded binary cannot be verified. Install from source: cargo install conduit-proxy");
  }
  const hex = sums[assetName];
  if (typeof hex !== "string" || !/^[0-9a-f]{64}$/.test(hex)) {
    fail(`no checksum for ${assetName} in checksums.json; refusing to install an unverified binary.`);
  }
  return hex;
}

// --------------------------------------------------------------------------
// Main
// --------------------------------------------------------------------------
async function main() {
  const assetName = getAssetName();
  const url = `${BASE_URL}/${assetName}`;
  const dest = join(NATIVE_DIR, assetName);

  // The release workflow writes the expected SHA-256 of every asset into
  // checksums.json before `npm publish` (#571). Without an entry there is
  // nothing to verify against, so refuse to install an unchecked binary.
  const expected = loadExpectedChecksum(assetName);

  // Idempotent: keep an existing binary only if it is the expected one.
  if (existsSync(dest)) {
    if (sha256(dest) === expected) {
      return;
    }
    console.warn(`[conduit] Existing ${assetName} does not match its checksum; downloading it again.`);
    try { unlinkSync(dest); } catch { /* ignore */ }
  }

  mkdirSync(NATIVE_DIR, { recursive: true });

  console.log(`[conduit] Downloading v${VERSION} for ${process.platform}/${process.arch}`);
  console.log(`[conduit] Source: ${url}`);

  // Download to a temporary name and rename only after verification, so a cut-off
  // or tampered download is never left under the final name (#585).
  const tmp = `${dest}.part`;
  try {
    await download(url, tmp);
  } catch (err) {
    try { unlinkSync(tmp); } catch { /* ignore */ }
    console.error(`\n[conduit] Download failed: ${err.message}`);
    console.error(`[conduit] You can install from source: cargo install conduit-proxy`);
    // A failed download is not a security event: leave the package without a
    // binary and let `conduit` report that, as before. Exit 0 so npm install
    // doesn't fail for the whole project. (A checksum mismatch does fail.)
    process.exit(0);
  }

  const actual = sha256(tmp);
  if (actual !== expected) {
    try { unlinkSync(tmp); } catch { /* ignore */ }
    fail(`checksum mismatch for ${assetName}: expected ${expected}, got ${actual}. The download was discarded.`);
  }
  renameSync(tmp, dest);

  // Make executable on Unix
  if (process.platform !== "win32") {
    chmodSync(dest, 0o755);
  }

  console.log(`[conduit] Binary installed to ${dest}`);
}

main();
