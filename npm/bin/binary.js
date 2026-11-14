/**
 * Conduit — finding and fetching the native binary.
 *
 * The npm package carries no binary (issue #712: the registry refused a package with the binaries of all 13 platforms,
 * 208.7 MB). The binary of this platform is downloaded from the GitHub release of the package's own version, verified
 * against `checksums.json` (the SHA-256 of every release asset, written into the package when it is published) and
 * kept in one of two places:
 *
 *   1. `bin/native/` inside the package, when that directory can be written (the usual `npm install`, and a global
 *      install run by root: root owns the package, so no other user can replace the binary);
 *   2. otherwise, on Linux and macOS and not for root, a per-user cache directory (a global install owned by root, run
 *      by an ordinary user): `$XDG_CACHE_HOME/conduit` or `~/.cache/conduit`, or macOS `~/Library/Caches/conduit`, in a
 *      `v<version>` subdirectory; `CONDUIT_CACHE_DIR` replaces the base.
 *
 * The cache is for the user who owns it, and only that user. A process with more privilege than the cache's owner must
 * not run what that user can replace (`sudo` on macOS keeps the user's HOME), so: root never uses the cache; Windows
 * never does either (it has no effective user id to tell an elevated process from an ordinary one, and UAC keeps the
 * user's profile), so there the binary goes to the package's folder only; and on Linux and macOS an entry (the two
 * directories above the file, and the file) is used only when it is not a symbolic link, is owned by the effective user
 * and cannot be written by group or others, and every directory above them belongs to root or that user and is not open
 * to others (nobody else can then swap a directory of the path); what was checked is what is run, the path with the
 * links above the cache resolved. A cached binary is also checked against its hash at
 * every run. Anything the cache does not pass is ignored, never removed.
 *
 * It is fetched after the install (download.js, a prefetch: npm hides the output of install scripts unless it is run
 * with `--foreground-scripts`), and again by the launcher (conduit.js) at any run where the binary is missing, so a
 * skipped or failed install script costs nothing. Every download says where it comes from and where it goes.
 * Messages go to stderr: stdout belongs to the program that is run.
 *
 * A download follows redirects only to https (http only when the source itself is http: the test hook), gives up when
 * no data arrives for 60 s or when the whole download takes more than 30 minutes, and refuses more than 200 MiB; it is
 * written to a new file with a random name that is renamed into place only after the hash matches.
 *
 * Environment: CONDUIT_SKIP_DOWNLOAD=1 never downloads; CONDUIT_CACHE_DIR; CONDUIT_DOWNLOAD_BASE and
 * CONDUIT_DOWNLOAD_IDLE_MS and CONDUIT_DOWNLOAD_DEADLINE_MS (test hooks: another base URL for the assets, with the
 * checksum still enforced, and other time limits).
 */

import {
  accessSync,
  chmodSync,
  constants,
  createWriteStream,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  renameSync,
  unlinkSync,
} from "node:fs";
import { get as httpGet } from "node:http";
import { get as httpsGet } from "node:https";
import { createHash, randomBytes } from "node:crypto";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { assetFor, SUPPORTED } from "./platform.js";

const here = dirname(fileURLToPath(import.meta.url));
const REPO = "lopatnov/conduit";
const MAX_REDIRECTS = 10;
/** The largest release asset is about 47 MB; a stream longer than this is not one. */
const MAX_BYTES = 200 * 1024 * 1024;
const idleLimit = () => Number(process.env.CONDUIT_DOWNLOAD_IDLE_MS) || 60_000;

/** The effective user id; null where there is none (Windows). */
const EUID = typeof process.geteuid === "function" ? process.geteuid() : null;

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

/**
 * The per-user cache is used only on a POSIX system and never by root (see the top of this file). On Windows there is
 * no effective user id to tell an elevated process from an ordinary one (UAC keeps the user's profile, so a process of
 * lower integrity can write the cache an elevated one reads), so the binary goes to the package's folder only.
 */
export function cacheAllowed(euid = EUID) {
  return euid !== null && euid !== 0;
}

/** Where the binary of `assetName` is looked for, in order: the package, then (not for root) the cache. */
export function locations(assetName) {
  const places = [join(NATIVE_DIR, assetName)];
  if (cacheAllowed()) places.push(join(cacheDir(), assetName));
  return places;
}

/** POSIX: `path` is not a symbolic link, belongs to the effective user and cannot be written by group or others. */
function trusted(path) {
  if (EUID === null) return false;
  try {
    const st = lstatSync(path);
    return !st.isSymbolicLink() && st.uid === EUID && (st.mode & 0o022) === 0;
  } catch {
    return false;
  }
}

/**
 * POSIX: every directory above `real`, a path whose links are already resolved, belongs to root or to the effective user and cannot be
 * written by group or others, so that nobody else can rename a directory of the path and put their own tree in its
 * place. A sticky directory such as /tmp may be writable by everyone: others cannot rename what is not theirs there.
 */
function ancestorsSafe(real) {
  for (let p = dirname(real); ; p = dirname(p)) {
    try {
      const st = lstatSync(p);
      const ownedByUsOrRoot = st.uid === 0 || st.uid === EUID;
      const openToOthers = (st.mode & 0o022) !== 0 && (st.mode & 0o1000) === 0;
      if (!ownedByUsOrRoot || openToOthers) return false;
    } catch {
      return false;
    }
    if (dirname(p) === p) return true;
  }
}

/**
 * The cache directory of this version with its links resolved ONCE, and what can be said of it: `dirTrusted` (the
 * directory and the one above it are the effective user's and closed to others, and so is everything above them) and
 * `fileTrusted` (the binary, if there is one, is a regular entry of the user's that is closed to others). Every check
 * and the run use these resolved paths and nothing else, so the path that was checked is, by construction, the path
 * that is run. null when the cache directory does not exist.
 */
function cacheEntry(assetName) {
  let dir;
  try {
    dir = realpathSync(cacheDir());
  } catch {
    return null;
  }
  const file = join(dir, assetName);
  return {
    dir,
    file,
    dirTrusted: trusted(dirname(dir)) && trusted(dir) && ancestorsSafe(dir),
    fileTrusted: !existsSync(file) || trusted(file),
  };
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

/** True when `dir` can be created and written (a new directory is made private when `mode` says so). */
function writable(dir, mode) {
  try {
    mkdirSync(dir, { recursive: true, ...(mode ? { mode } : {}) });
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

/**
 * Fetches `url` into `dest` (a file that must not exist), following redirects to https. Resolves when the file is
 * complete, rejects with an Error.
 */
function download(url, dest, write) {
  // The only http source is the test hook; a redirect may not leave https unless the source itself is http.
  const httpSource = url.startsWith("http:");
  const idle = idleLimit();

  // The idle limit stops a stall, not a server that trickles a byte at a time: the whole download, redirects included,
  // has a deadline too (30 minutes: the largest asset is about 47 MB, which a link of 30 kB/s still fetches).
  const deadline = Number(process.env.CONDUIT_DOWNLOAD_DEADLINE_MS) || 30 * 60_000;

  return new Promise((resolveOuter, rejectOuter) => {
    let redirects = 0;
    let inFlight = null; // the request, the response and the file that are open now
    const timer = setTimeout(() => {
      inFlight?.req.destroy();
      inFlight?.res?.destroy();
      inFlight?.file?.destroy();
      rejectOuter(new Error(`the download took longer than ${deadline / 1000} s`));
    }, deadline);
    const resolve = (value) => {
      clearTimeout(timer);
      resolveOuter(value);
    };
    const reject = (err) => {
      clearTimeout(timer);
      rejectOuter(err);
    };

    function request(target) {
      const get = target.startsWith("http:") ? httpGet : httpsGet;
      const req = get(target, { headers: { "User-Agent": `conduit-npm/${VERSION}` }, timeout: idle }, (res) => {
        inFlight.res = res;
        if ([301, 302, 307, 308].includes(res.statusCode)) {
          res.resume();
          if (++redirects > MAX_REDIRECTS) return reject(new Error(`too many redirects (>${MAX_REDIRECTS}) for ${target}`));
          if (!res.headers.location) return reject(new Error(`a redirect with no Location header from ${target}`));
          let next;
          try {
            next = new URL(res.headers.location, target);
          } catch {
            return reject(new Error(`a redirect to an invalid address from ${target}`));
          }
          if (next.protocol !== "https:" && !(httpSource && next.protocol === "http:")) {
            return reject(new Error(`refusing a redirect to ${next.protocol} (${next.href})`));
          }
          return request(next.href);
        }
        if (res.statusCode !== 200) {
          res.resume();
          return reject(new Error(`HTTP ${res.statusCode} for ${target}`));
        }

        const total = parseInt(res.headers["content-length"] || "0", 10);
        if (total > MAX_BYTES) {
          res.resume();
          return reject(new Error(`refusing ${total} bytes: more than the ${MAX_BYTES} a release asset can be`));
        }
        const progress = progressPrinter(total, write);
        let received = 0;
        // "wx": a file or a symbolic link that is already there makes the download fail instead of being followed.
        const file = createWriteStream(dest, { flags: "wx", mode: 0o700 });
        inFlight.file = file;
        const fail = (err) => {
          res.destroy();
          file.destroy();
          reject(err);
        };
        res.setTimeout(idle, () => fail(new Error(`no data for ${idle / 1000} s`)));
        res.on("data", (chunk) => {
          received += chunk.length;
          if (received > MAX_BYTES) return fail(new Error(`more than ${MAX_BYTES} bytes, which no release asset is`));
          progress.update(received);
        });
        // `complete` is false when the connection closed before the whole body arrived (the "aborted" event is gone
        // from newer Node versions).
        res.on("close", () => {
          if (!res.complete) fail(new Error("the connection closed before the download finished"));
        });
        res.on("error", fail);
        file.on("error", fail);
        // Resolve on "close", not on "finish": the descriptor is still open for writing at "finish", and running a file
        // that is open for writing fails with ETXTBSY (it showed on Node 24 in CI, in the test that runs a file the
        // launcher has just downloaded).
        let written = false;
        file.on("finish", () => {
          progress.done();
          if (total > 0 && received !== total) return reject(new Error(`got ${received} of ${total} bytes`));
          written = true;
        });
        file.on("close", () => {
          if (written) resolve();
        });
        res.pipe(file);
      });
      inFlight = { req };
      req.on("timeout", () => req.destroy(new Error(`no answer for ${idle / 1000} s`)));
      req.on("error", reject);
    }

    request(url);
  });
}

/**
 * The path of the binary of this platform, downloading it first when there is none.
 *
 * `verifyExisting` (the install script) re-checks a binary that is already there against its checksum and replaces it
 * when it differs; the launcher leaves a binary in the package alone, so that a run costs no hashing (one in the cache
 * is always checked: see the top of this file).
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
  const inPackage = places[0];
  let found;
  if (existsSync(inPackage)) {
    found = inPackage;
  } else if (cacheAllowed()) {
    const entry = cacheEntry(name);
    if (entry && existsSync(entry.file)) {
      if (entry.dirTrusted && entry.fileTrusted) {
        found = entry.file;
      } else {
        say(`[conduit] Ignoring ${entry.file}: it is not yours, or others can write it or the directories above it.`);
      }
    }
  }
  if (found) {
    // A binary in the package is as safe as the package. One in the cache can be replaced by the user's own processes,
    // so it is checked against its hash at every run and is downloaded again when it differs.
    const inCache = found !== inPackage;
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
  let dir;
  if (writable(NATIVE_DIR)) {
    dir = NATIVE_DIR;
  } else if (cacheAllowed() && writable(cacheDir(), 0o700)) {
    const entry = cacheEntry(name);
    if (entry?.dirTrusted) dir = entry.dir;
  }
  if (!dir) {
    throw new FetchError(
      `No native binary for ${key}, and it cannot be put in ${NATIVE_DIR}` +
        (cacheAllowed()
          ? ` or in ${cacheDir()} (that cache must be yours and not writable by others, and so must be the directories above it).\n`
          : " (the per-user cache is not used when running as root, nor on Windows).\n") +
        (cacheAllowed()
          ? "Set CONDUIT_CACHE_DIR to a directory of yours, or install from source: cargo install lopatnov-conduit"
          : "Install the package where you can write, or install from source: cargo install lopatnov-conduit"),
    );
  }
  const url = `${baseUrl()}/${name}`;
  const dest = join(dir, name);

  say(`[conduit] Downloading the native binary for ${process.platform}/${process.arch} (conduit v${VERSION}).`);
  say(`[conduit] From: ${url}`);
  say(`[conduit] To:   ${dest}`);

  // Download to a new file with a random name and rename it only after the check: a cut-off or tampered download is
  // never left under the final name (#585), and nobody can guess the name in advance.
  const tmp = `${dest}.${randomBytes(6).toString("hex")}.part`;
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
  if (process.platform !== "win32") chmodSync(tmp, 0o755);
  renameSync(tmp, dest);

  say(`[conduit] SHA-256 verified (${actual}).`);
  say(`[conduit] Installed: ${dest}`);
  return { path: dest, downloaded: true };
}
