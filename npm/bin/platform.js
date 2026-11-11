/**
 * Conduit — which release asset belongs to this machine.
 *
 * One table for the launcher (conduit.js) and the downloader (download.js). The names are those of the release
 * assets of the full build (the plain names; see .github/workflows/release.yml), checked against the workflow by
 * scripts/test_release_matrix.py.
 */

import { existsSync } from "node:fs";

// "os-arch", and "linux-arch-musl" for a Linux with the musl libc (Alpine and kin).
const ASSETS = {
  "linux-x64":        "conduit-x86_64-unknown-linux-gnu",
  "linux-arm64":      "conduit-aarch64-unknown-linux-gnu",
  "linux-x64-musl":   "conduit-x86_64-unknown-linux-musl",
  "linux-arm64-musl": "conduit-aarch64-unknown-linux-musl",
  "darwin-x64":       "conduit-x86_64-apple-darwin",
  "darwin-arm64":     "conduit-aarch64-apple-darwin",
  "win32-x64":        "conduit-x86_64-pc-windows-msvc.exe",
  "win32-arm64":      "conduit-aarch64-pc-windows-msvc.exe",
  // 32-bit: Node reports `ia32` and `arm`. `arm` is both ARMv7 (the official Node builds) and ARMv6 (Raspberry Pi OS's
  // own Node on a Raspberry Pi 1 or Zero), and an ARMv7-only binary dies with an illegal instruction on ARMv6, so
  // `assetFor` tells them apart by Node's `arm_version`: "linux-arm" is the ARMv7 build, "linux-armv6" (not a Node
  // platform name, only a key of this table) the ARMv6 hard-float one, which also runs on ARMv7.
  // There are no musl builds for the 32-bit pair, so an Alpine system gets the "unsupported" message.
  "linux-ia32":       "conduit-i686-unknown-linux-gnu",
  "linux-arm":        "conduit-armv7-unknown-linux-gnueabihf",
  "linux-armv6":      "conduit-arm-unknown-linux-gnueabihf",
};

export const SUPPORTED = Object.keys(ASSETS);

/**
 * True on a Linux whose libc is musl. A glibc binary does not start there, while the musl builds are static and
 * run on glibc systems too, so a wrong "musl" answer is harmless and a wrong "glibc" answer is not: the loader file
 * is taken as proof of musl, and otherwise glibc must show itself in the runtime report.
 */
const MUSL_MARKERS = ["/lib/ld-musl-x86_64.so.1", "/lib/ld-musl-aarch64.so.1", "/etc/alpine-release"];

function runtimeReport() {
  return process.report?.getReport?.().header;
}

/**
 * The two probes are parameters so that a test can fake a host: `exists(path)` and `report()` (the Node runtime
 * report, whose header names the glibc version when there is one; it may throw or return nothing).
 */
export function isMusl(platform = process.platform, { exists = existsSync, report = runtimeReport } = {}) {
  if (platform !== "linux") return false;
  if (MUSL_MARKERS.some((path) => exists(path))) return true;
  try {
    const header = report();
    return Boolean(header) && !header.glibcVersionRuntime;
  } catch {
    return false;
  }
}

/** The table key and the asset name of this machine (name is undefined on an unsupported platform). */
export function assetFor(
  platform = process.platform,
  arch = process.arch,
  musl = isMusl(platform),
  armVersion = process.config?.variables?.arm_version,
) {
  let key = platform === "linux" && musl ? `${platform}-${arch}-musl` : `${platform}-${arch}`;
  // The ARMv7 build only where Node says it is an ARMv7 build; anything else (ARMv6, or no answer) gets the ARMv6
  // build, which runs on both.
  if (key === "linux-arm" && String(armVersion) !== "7") key = "linux-armv6";
  return { key, name: ASSETS[key] };
}
