#!/usr/bin/env node
/**
 * Conduit — platform wrapper.
 *
 * Resolves the native binary for the current OS/arch (downloading it first, with a message that says from where and
 * to where, when this machine has none: see binary.js) and spawns it, passing all CLI arguments and stdio through
 * transparently.
 */

import { spawn } from "node:child_process";
import { ensureBinary } from "./binary.js";

let binaryPath;
try {
  ({ path: binaryPath } = await ensureBinary());
} catch (err) {
  console.error(`[conduit] ${err.message}`);
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
