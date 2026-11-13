/**
 * Conduit — finding and fetching the native binary.
 *
 * The npm package carries no binary (issue #712: the registry refused a package with the binaries of all 13 platforms,
 * 208.7 MB). The binary of this platform is downloaded from the GitHub release of the package's own version, verified
 * against `checksums.json` (the SHA-256 of every release asset, written into the package when it is published) and
 * kept in one of two places:
 *
 *   1. `bin/native/` inside the package, when that directory can be written (the usual `npm install`);
 *   2. otherwise a per-user cache directory (a global install owned by root and run by an ordinary user):
 *      Linux `$XDG_CACHE_HOME/conduit` or `~/.cache/conduit`, macOS `~/Library/Caches/conduit`,
 *      Windows `%LOCALAPPDATA%\conduit\Cache`, in a `v<version>` subdirectory; `CONDUIT_CACHE_DIR` replaces the base.
 *
 * It is fetched after the install (download.js, a prefetch: npm hides the output of install scripts unless it is run
 * with `--foreground-scripts`), and again by the launcher (conduit.js) at any run where the binary is missing, so a
 * skipped or failed install script costs nothing. Every download says where it comes from and where it goes.
 * Messages go to stderr: stdout belongs to the program that is run.
 *
 * Environment: CONDUIT_SKIP_DOWNLOAD=1 never downloads; CONDUIT_CACHE_DIR; CONDUIT_DOWNLOAD_BASE (a test hook: another
 * base URL for the assets; the checksum is still enforced).
 */

import {
  accessSync,
  chmodSync,
  constants,
  createWriteStream,
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  unlinkSync,
} from "node:fs";
import { get as httpGet } from "node:http";
import { get as httpsGet } from "node:https";
import { createHash } from "node:crypto";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { assetFor, SUPPORTED } from "./platform.js";

const here = dirname(fileURLToPath(import.meta.url));
const REPO = "lopatnov/conduit";
const MAX_REDIRECTS = 10;

export const VERSION = JSON.parse(readFileSync(join(here, "..", "package.json"), "utf8")).version;
/** Inside the package: where the install script puts the binary when it can. */
export const NATIVE_DIR = join(here, "native");

/**
 * What went wrong, for the caller to word and to decide the exit status: `fatal` is a binary that cannot be trusted (a
 * checksum that does not match, or none to compare with), `unsupported` a platform with no release asset.
 */
export class FetchError extends Error {
  constructor(message, { fatal = false, unsupported = false } = {}) {
    super(message);
    this.fatal = fatal;
    this.unsupported = unsupported;
  }
}

/** The per-user cache directory of this version. */
export function cacheDir(env = process.env, platform = process.platform) {
  let base;
  if (env.CONDUIT_CACHE_DIR) {
    base = env.CONDUIT_CACHE_DIR;
  } else if (platform === "win32") {
    base = join(env.LOCALAPPDATA || join(homedir(), "AppData", "Local"), "conduit", "Cache");
  } else if (platform === "darwin") {
    base = join(homedir(), "Library", "Caches", "conduit");
  } else {
    base = join(env.XDG_CACHE_HOME || join(homedir(), ".cache"), "conduit");
  }
  return join(base, `v${VERSION}`);
}

/** Where the binary of `assetName` is looked for, in order. */
export function locations(assetName) {
  return [join(NATIVE_DIR, assetName), join(cacheDir(), assetName)];
}

function baseUrl() {
  return process.env.CONDUIT_DOWNLOAD_BASE || `https://github.com/${REPO}/releases/download/v${VERSION}`;
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function remove(path) {
  try {
    unlinkSync(path);
  } catch {
    /* already gone */
  }
}

/** True when `dir` can be created and written. */
function writable(dir) {
  try {
    mkdirSync(dir, { recursive: true });
    accessSync(dir, constants.W_OK);
    return true;
  } catch {
    return false;
  }
}

/** The SHA-256 the release workflow wrote into checksums.json for `assetName`; no entry, no install. */
function expectedChecksum(assetName) {
  let sums;
  try {
    sums = JSON.parse(readFileSync(join(here, "checksums.json"), "utf8"));
  } catch {
    throw new FetchError(
      "checksums.json is missing from this package, so the downloaded binary cannot be verified. " +
        "Install from source: cargo install lopatnov-conduit",
      { fatal: true },
    );
  }
  const hex = sums[assetName];
  if (typeof hex !== "string" || !/^[0-9a-f]{64}$/.test(hex)) {
    throw new FetchError(`no checksum for ${assetName} in checksums.json; refusing to install an unverified binary.`, {
      fatal: true,
    });
  }
  return hex;
}

const megabytes = (n) => (n / 1048576).toFixed(1);

/** Progress on a terminal rewrites one line; anywhere else (a pipe, a log) one line per 10 %. */
function progressPrinter(total, write) {
  const tty = Boolean(process.stderr.isTTY);
  let lastStep = -1;
  return {
    update(received) {
      if (total > 0) {
        const pct = Math.floor((received / total) * 100);
        if (tty) {
          write(`\r[conduit] Downloading ${pct}% (${megabytes(received)} of ${megabytes(total)} MB)`);
        } else if (Math.floor(pct / 10) !== lastStep) {
          lastStep = Math.floor(pct / 10);
          write(`[conduit] Downloading ${pct}% (${megabytes(received)} of ${megabytes(total)} MB)\n`);
        }
      } else if (Math.floor(received / 5242880) !== lastStep) {
        lastStep = Math.floor(received / 5242880);
        write(`${tty ? "\r" : ""}[conduit] Downloading ${megabytes(received)} MB\n`);
      }
    },
    done() {
      if (tty) write("\n");
    },
  };
}

/** Fetches `url` into `dest`, following redirects. Resolves when the file is complete, rejects with an Error. */
function download(url, dest, write) {
  return new Promise((resolve, reject) => {
    let redirects = 0;

    function request(target) {
      const get = target.startsWith("http:") ? httpGet : httpsGet;
      get(target, { headers: { "User-Agent": `conduit-npm/${VERSION}` } }, (res) => {
        if ([301, 302, 307, 308].includes(res.statusCode)) {
          res.resume();
          if (++redirects > MAX_REDIRECTS) return reject(new Error(`too many redirects (>${MAX_REDIRECTS}) for ${target}`));
          if (!res.headers.location) return reject(new Error(`a redirect with no Location header from ${target}`));
          return request(new URL(res.headers.location, target).href);
        }
        if (res.statusCode !== 200) {
          res.resume();
          return reject(new Error(`HTTP ${res.statusCode} for ${target}`));
        }

        const total = parseInt(res.headers["content-length"] || "0", 10);
        const progress = progressPrinter(total, write);
        let received = 0;
        const file = createWriteStream(dest);
        res.on("data", (chunk) => {
          received += chunk.length;
          progress.update(received);
        });
        // `complete` is false when the connection closed before the whole body arrived (the "aborted" event is gone
        // from newer Node versions).
        res.on("close", () => {
          if (!res.complete) {
            file.destroy();
            reject(new Error("the connection closed before the download finished"));
          }
        });
        res.on("error", (err) => {
          file.destroy();
          reject(err);
        });
        file.on("error", reject);
        file.on("finish", () => {
          progress.done();
          if (total > 0 && received !== total) return reject(new Error(`got ${received} of ${total} bytes`));
          resolve();
        });
        res.pipe(file);
      }).on("error", reject);
    }

    request(url);
  });
}

/**
 * The path of the binary of this platform, downloading it first when there is none.
 *
 * `verifyExisting` (the install script) re-checks a binary that is already there against its checksum and replaces it
 * when it differs; the launcher leaves an existing binary alone, so that a run costs no hashing.
 * `write` receives every message (default: stderr).
 *
 * Throws a FetchError. A failed download is not `fatal` (the caller decides: the install script goes on, the launcher
 * stops); a checksum that does not match is, and the file is removed.
 */
export async function ensureBinary({ verifyExisting = false, write = (s) => process.stderr.write(s) } = {}) {
  const say = (line) => write(`${line}\n`);
  const { key, name } = assetFor();
  if (!name) {
    throw new FetchError(
      `Unsupported platform: ${key}\nSupported: ${SUPPORTED.join(", ")}\nInstall from source: cargo install lopatnov-conduit`,
      { unsupported: true },
    );
  }

  const places = locations(name);
  const found = places.find((p) => existsSync(p));
  if (found) {
    // A binary in the package directory is as safe as the package (root owns a global install). One in the per-user
    // cache can be replaced by any process of that user, and conduit may be run by root with that user's HOME
    // (`sudo` on macOS keeps it): so a cached binary is checked against its hash at every run (about 0.1 s) and is
    // downloaded again when it differs.
    const inCache = found === places[1];
    if (!verifyExisting && !inCache) return { path: found, downloaded: false };
    if (sha256(found) === expectedChecksum(name)) return { path: found, downloaded: false };
    say(`[conduit] The existing ${found} does not match its checksum; downloading it again.`);
    remove(found);
  }

  if (process.env.CONDUIT_SKIP_DOWNLOAD === "1") {
    throw new FetchError(
      `No native binary for ${key}, and downloading is switched off (CONDUIT_SKIP_DOWNLOAD=1). It is looked for in:\n` +
        places.map((p) => `  ${p}`).join("\n"),
    );
  }

  // The checksum comes first: a package that cannot verify a download does not make one.
  const expected = expectedChecksum(name);
  const dir = [NATIVE_DIR, cacheDir()].find(writable);
  if (!dir) {
    throw new FetchError(
      `No native binary for ${key}, and neither ${NATIVE_DIR} nor ${cacheDir()} can be written.\n` +
        "Set CONDUIT_CACHE_DIR to a directory you can write, or install from source: cargo install lopatnov-conduit",
    );
  }
  const url = `${baseUrl()}/${name}`;
  const dest = join(dir, name);

  say(`[conduit] Downloading the native binary for ${process.platform}/${process.arch} (conduit v${VERSION}).`);
  say(`[conduit] From: ${url}`);
  say(`[conduit] To:   ${dest}`);

  // Download to a temporary name and rename only after the check: a cut-off or tampered download is never left under
  // the final name (#585). The process id keeps two first runs at the same time from writing one file.
  const tmp = `${dest}.${process.pid}.part`;
  try {
    await download(url, tmp, write);
  } catch (err) {
    remove(tmp);
    throw new FetchError(
      `Download failed: ${err.message}\n  from ${url}\n  to   ${dest}\n` +
        "Install from source: cargo install lopatnov-conduit",
    );
  }

  const actual = sha256(tmp);
  if (actual !== expected) {
    remove(tmp);
    throw new FetchError(
      `checksum mismatch for ${name}: expected ${expected}, got ${actual}. The download was discarded.`,
      { fatal: true },
    );
  }
  renameSync(tmp, dest);
  if (process.platform !== "win32") chmodSync(dest, 0o755);

  say(`[conduit] SHA-256 verified (${actual}).`);
  say(`[conduit] Installed: ${dest}`);
  return { path: dest, downloaded: true };
}
