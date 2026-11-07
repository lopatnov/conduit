#!/usr/bin/env node
/**
 * Conduit — platform wrapper.
 *
 * Resolves the native binary for the current OS/arch and spawns it,
 * passing all CLI arguments and stdio through transparently.
 */

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { assetFor, SUPPORTED } from "./platform.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

// --------------------------------------------------------------------------
// Platform → binary name mapping (matches GitHub Release asset names)
// --------------------------------------------------------------------------
function getPlatformBinary() {
  // The table is shared with download.js (platform.js); on Linux it picks the musl build on a musl libc.
  const { key, name } = assetFor();

  if (!name) {
    console.error(
      `[conduit] Unsupported platform: ${key}\n` +
      `Supported: ${SUPPORTED.join(", ")}\n` +
      `Install from source: cargo install lopatnov-conduit`
    );
    process.exit(1);
  }

  return name;
}

// --------------------------------------------------------------------------
// Locate binary
// --------------------------------------------------------------------------
const nativeDir = join(__dirname, "native");
const binaryName = getPlatformBinary();
const binaryPath = join(nativeDir, binaryName);

if (!existsSync(binaryPath)) {
  console.error(
    `[conduit] Native binary not found: ${binaryPath}\n` +
    `Try reinstalling: npm install @lopatnov/conduit\n` +
    `Or install from source: cargo install lopatnov-conduit`
  );
  process.exit(1);
}

// --------------------------------------------------------------------------
// Spawn
// --------------------------------------------------------------------------
const child = spawn(binaryPath, process.argv.slice(2), {
  stdio: "inherit",
  env: process.env,
});

child.on("error", (err) => {
  console.error(`[conduit] Failed to start binary: ${err.message}`);
  process.exit(1);
});

child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 0);
  }
});
