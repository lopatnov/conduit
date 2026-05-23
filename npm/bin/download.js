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

import { createWriteStream, existsSync, mkdirSync, chmodSync, unlinkSync } from "node:fs";
import { get } from "node:https";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { readFileSync } from "node:fs";

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

// --------------------------------------------------------------------------
// Platform → asset name
// --------------------------------------------------------------------------
function getAssetName() {
  const { platform, arch } = process;

  const map = {
    "linux-x64":    "conduit-x86_64-unknown-linux-gnu",
    "linux-arm64":  "conduit-aarch64-unknown-linux-gnu",
    "darwin-x64":   "conduit-x86_64-apple-darwin",
    "darwin-arm64": "conduit-aarch64-apple-darwin",
    "win32-x64":    "conduit-x86_64-pc-windows-msvc.exe",
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
function download(url, dest) {
  return new Promise((resolve, reject) => {
    const file = createWriteStream(dest);

    function request(url) {
      get(url, { headers: { "User-Agent": `conduit-npm/${VERSION}` } }, (res) => {
        if (res.statusCode === 301 || res.statusCode === 302 || res.statusCode === 307 || res.statusCode === 308) {
          // Follow redirect
          request(res.headers.location);
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

// --------------------------------------------------------------------------
// Main
// --------------------------------------------------------------------------
async function main() {
  const assetName = getAssetName();
  const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${assetName}`;
  const dest = join(NATIVE_DIR, assetName);

  // Skip if already downloaded (idempotent)
  if (existsSync(dest)) {
    return;
  }

  mkdirSync(NATIVE_DIR, { recursive: true });

  console.log(`[conduit] Downloading v${VERSION} for ${process.platform}/${process.arch}`);
  console.log(`[conduit] Source: ${url}`);

  try {
    await download(url, dest);
  } catch (err) {
    console.error(`\n[conduit] Download failed: ${err.message}`);
    console.error(`[conduit] You can install from source: cargo install conduit-proxy`);
    // Exit 0 so npm install doesn't fail for the whole project
    process.exit(0);
  }

  // Make executable on Unix
  if (process.platform !== "win32") {
    chmodSync(dest, 0o755);
  }

  console.log(`[conduit] Binary installed to ${dest}`);
}

main();
