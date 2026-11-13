#!/usr/bin/env node
/**
 * Conduit — postinstall prefetch of the native binary.
 *
 * Runs after `npm install` and downloads the binary of this platform from the GitHub release of this version
 * (see binary.js: where it comes from, where it goes, how it is verified). It is only a prefetch: the launcher
 * (conduit.js) downloads the binary at the first run when it is missing, so an install with `--ignore-scripts`
 * (or one whose script failed) still works. npm hides the output of install scripts; `--foreground-scripts` shows it.
 *
 * Skipped with CONDUIT_SKIP_DOWNLOAD=1. A download that fails does not fail the install (the exit status is 0);
 * a binary that cannot be verified (a checksum that does not match, or no checksums.json) does.
 */

import { ensureBinary } from "./binary.js";

if (process.env.CONDUIT_SKIP_DOWNLOAD === "1") {
  process.exit(0);
}

try {
  await ensureBinary({ verifyExisting: true });
} catch (err) {
  console.error(`\n[conduit] ${err.message}`);
  if (err.fatal) {
    process.exit(1);
  }
  if (!err.unsupported) {
    console.error("[conduit] The install goes on: conduit downloads the binary the first time you run it.");
  }
  process.exit(0);
}
