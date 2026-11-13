// Run with: node --test npm/test/launcher.test.mjs   (or `npm test` in npm/)
// Covers the download at run time (issue #712): the package carries no binary, the launcher fetches it when it is
// missing, says where it comes from and where it goes, keeps stdout for the program, falls back to a cache directory
// when the package directory cannot be written, and the packed package stays small.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { assetFor } from "../bin/platform.js";

const here = dirname(fileURLToPath(import.meta.url));
const pkgRoot = join(here, "..");
const VERSION = "7.8.9";
const { name: ASSET } = assetFor();
const sha = (b) => createHash("sha256").update(b).digest("hex");

// The "binary" is a shell script that echoes its arguments, so the test can see that the launcher ran what it fetched.
const PROGRAM = Buffer.from('#!/bin/sh\necho "hello-from-conduit $@"\n');
const runnable = process.platform !== "win32" && ASSET !== undefined;

/** A package directory with the scripts and a checksums.json that knows `body`, and a server that serves `served`. */
async function setup({ served = PROGRAM, body = PROGRAM, nativeIsAFile = false, serverUp = true, redirect = false } = {}) {
  const pkg = mkdtempSync(join(tmpdir(), "conduit-launcher-"));
  mkdirSync(join(pkg, "bin"));
  for (const f of ["conduit.js", "download.js", "binary.js", "platform.js"]) {
    cpSync(join(pkgRoot, "bin", f), join(pkg, "bin", f));
  }
  writeFileSync(join(pkg, "package.json"), JSON.stringify({ version: VERSION, type: "module" }));
  writeFileSync(join(pkg, "bin", "checksums.json"), JSON.stringify({ [ASSET]: sha(body) }));
  // A regular file where the directory should be: the package directory cannot be written.
  if (nativeIsAFile) writeFileSync(join(pkg, "bin", "native"), "not a directory");

  const urls = [];
  const server = createServer((req, res) => {
    urls.push(req.url);
    // GitHub answers a release asset with a redirect to its own storage: the download must follow it
    if (redirect && req.url === `/${ASSET}`) {
      res.writeHead(302, { Location: `/storage/${ASSET}` });
      res.end();
      return;
    }
    res.end(served);
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  // A failed assertion skips `t.close()`: an unreferenced server must not keep the test process alive (it hung CI).
  server.unref();
  const base = `http://127.0.0.1:${server.address().port}`;
  if (!serverUp) await new Promise((r) => server.close(r));
  const cache = join(pkg, "cache");
  return {
    pkg,
    base,
    cache,
    urls,
    native: join(pkg, "bin", "native", ASSET),
    cached: join(cache, `v${VERSION}`, ASSET),
    close: () => server.listening && server.close(),
    run(script, args = [], extraEnv = {}) {
      return new Promise((resolve) => {
        const p = spawn(process.execPath, [join(pkg, "bin", script), ...args], {
          env: { ...process.env, CONDUIT_DOWNLOAD_BASE: base, CONDUIT_CACHE_DIR: cache, ...extraEnv },
          stdio: ["ignore", "pipe", "pipe"],
        });
        let stdout = "";
        let stderr = "";
        p.stdout.on("data", (d) => (stdout += d));
        p.stderr.on("data", (d) => (stderr += d));
        p.on("exit", (code) => resolve({ code, stdout, stderr }));
      });
    },
  };
}

test("a missing binary is downloaded at the first run; the source and the destination are shown on stderr", { skip: !runnable }, async () => {
  const t = await setup();
  const r = await t.run("conduit.js", ["a", "b"]);
  t.close();
  assert.equal(r.code, 0, r.stderr);
  // stdout is the program's alone: no message of the launcher may end up in it
  assert.equal(r.stdout, "hello-from-conduit a b\n");
  assert.ok(r.stderr.includes(`From: ${t.base}/${ASSET}`), r.stderr);
  assert.ok(r.stderr.includes(`To:   ${t.native}`), r.stderr);
  assert.ok(r.stderr.includes("SHA-256 verified"), r.stderr);
  assert.deepEqual(t.urls, [`/${ASSET}`]);
  assert.ok(existsSync(t.native));
});

test("a redirect to the storage is followed, and the source shown is the one that was asked for", { skip: !runnable }, async () => {
  const t = await setup({ redirect: true });
  const r = await t.run("conduit.js", ["via", "redirect"]);
  t.close();
  assert.equal(r.code, 0, r.stderr);
  assert.equal(r.stdout, "hello-from-conduit via redirect\n");
  assert.deepEqual(t.urls, [`/${ASSET}`, `/storage/${ASSET}`]);
  assert.ok(r.stderr.includes(`From: ${t.base}/${ASSET}`), r.stderr);
});

test("the second run downloads nothing", { skip: !runnable }, async () => {
  const t = await setup();
  await t.run("conduit.js");
  const second = await t.run("conduit.js", ["again"]);
  t.close();
  assert.equal(second.code, 0, second.stderr);
  assert.equal(second.stdout, "hello-from-conduit again\n");
  assert.ok(!second.stderr.includes("From:"), second.stderr);
  assert.equal(t.urls.length, 1);
});

test("a package directory that cannot be written falls back to the cache directory", { skip: !runnable }, async () => {
  const t = await setup({ nativeIsAFile: true });
  const first = await t.run("conduit.js", ["x"]);
  assert.equal(first.code, 0, first.stderr);
  assert.equal(first.stdout, "hello-from-conduit x\n");
  assert.ok(first.stderr.includes(`To:   ${t.cached}`), first.stderr);
  assert.ok(existsSync(t.cached));
  // the next run finds it in the cache
  const second = await t.run("conduit.js", ["y"]);
  t.close();
  assert.equal(second.stdout, "hello-from-conduit y\n");
  assert.equal(t.urls.length, 1);
});

test("a binary in the cache that does not match its checksum is not run: it is replaced by a verified download", { skip: !runnable }, async () => {
  // The cache is writable by any process of the user, so conduit must not run what it finds there unchecked (conduit
  // may be started by root with that user's HOME).
  const t = await setup({ nativeIsAFile: true });
  mkdirSync(join(t.cache, `v${VERSION}`), { recursive: true });
  writeFileSync(t.cached, "#!/bin/sh\necho planted\n", { mode: 0o755 });
  const r = await t.run("conduit.js", ["z"]);
  t.close();
  assert.equal(r.code, 0, r.stderr);
  assert.equal(r.stdout, "hello-from-conduit z\n");
  assert.ok(r.stderr.includes("does not match its checksum"), r.stderr);
  assert.deepEqual(t.urls, [`/${ASSET}`]);
  // and a genuine one in the cache is run without a download
  const again = await t.run("conduit.js", ["w"]);
  assert.equal(again.stdout, "hello-from-conduit w\n");
  assert.equal(t.urls.length, 1);
});

test("a tampered download is refused, removed, and not run", { skip: !runnable }, async () => {
  const t = await setup({ served: Buffer.from("#!/bin/sh\necho tampered\n") });
  const r = await t.run("conduit.js");
  t.close();
  assert.equal(r.code, 1);
  assert.equal(r.stdout, "");
  assert.ok(r.stderr.includes("checksum mismatch"), r.stderr);
  const native = join(t.pkg, "bin", "native");
  assert.deepEqual(existsSync(native) ? readdirSync(native) : [], []);
});

test("a source that cannot be reached fails the run and names the source and the destination", { skip: !runnable }, async () => {
  const t = await setup({ serverUp: false });
  const r = await t.run("conduit.js");
  assert.equal(r.code, 1);
  assert.ok(r.stderr.includes("Download failed"), r.stderr);
  assert.ok(r.stderr.includes(`from ${t.base}/${ASSET}`), r.stderr);
  assert.ok(r.stderr.includes(`to   ${t.native}`), r.stderr);
});

test("CONDUIT_SKIP_DOWNLOAD=1 never touches the network and says where the binary is looked for", { skip: !runnable }, async () => {
  const t = await setup();
  const r = await t.run("conduit.js", [], { CONDUIT_SKIP_DOWNLOAD: "1" });
  t.close();
  assert.equal(r.code, 1);
  assert.deepEqual(t.urls, []);
  assert.ok(r.stderr.includes(t.native), r.stderr);
  assert.ok(r.stderr.includes(t.cached), r.stderr);
});

test("the install script prefetches, and a failed prefetch does not fail the install", { skip: !runnable }, async () => {
  const up = await setup();
  const ok = await up.run("download.js");
  up.close();
  assert.equal(ok.code, 0, ok.stderr);
  assert.ok(existsSync(up.native));

  const down = await setup({ serverUp: false });
  const failed = await down.run("download.js");
  assert.equal(failed.code, 0);
  assert.ok(failed.stderr.includes("Download failed"), failed.stderr);
  assert.ok(failed.stderr.includes("downloads the binary the first time you run it"), failed.stderr);
});

test("the packed package carries no binary, even when bin/native exists, and is small", () => {
  const pkg = mkdtempSync(join(tmpdir(), "conduit-pack-"));
  mkdirSync(join(pkg, "bin", "native"), { recursive: true });
  for (const f of ["conduit.js", "download.js", "binary.js", "platform.js"]) cpSync(join(pkgRoot, "bin", f), join(pkg, "bin", f));
  cpSync(join(pkgRoot, "package.json"), join(pkg, "package.json"));
  cpSync(join(pkgRoot, "Readme.md"), join(pkg, "Readme.md"));
  writeFileSync(join(pkg, "bin", "checksums.json"), "{}");
  // a 5 MB stand-in for a binary: it must not be packed
  writeFileSync(join(pkg, "bin", "native", "conduit-x86_64-unknown-linux-gnu"), Buffer.alloc(5 * 1024 * 1024));
  const r = spawnSync("npm", ["pack", "--dry-run", "--json"], { cwd: pkg, encoding: "utf8", shell: process.platform === "win32" });
  if (r.error || r.status !== 0) {
    test.skip?.("npm is not available");
    return;
  }
  const info = JSON.parse(r.stdout.slice(r.stdout.indexOf("[")))[0];
  const paths = info.files.map((f) => f.path);
  assert.ok(!paths.some((p) => p.startsWith("bin/native")), paths.join(", "));
  assert.ok(info.unpackedSize < 200 * 1024, `unpacked ${info.unpackedSize} bytes`);
});
