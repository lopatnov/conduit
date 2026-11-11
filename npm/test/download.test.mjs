// Run with: node --test npm/test/download.test.mjs
// Covers the checksum verification of the postinstall download (issue #571).
import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import { cpSync, mkdtempSync, mkdirSync, writeFileSync, existsSync, readdirSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { assetFor, isMusl } from "../bin/platform.js";

const here = dirname(fileURLToPath(import.meta.url));
const ASSETS = [
  "conduit-x86_64-unknown-linux-gnu",
  "conduit-aarch64-unknown-linux-gnu",
  "conduit-x86_64-apple-darwin",
  "conduit-aarch64-apple-darwin",
  "conduit-x86_64-pc-windows-msvc.exe",
  "conduit-x86_64-unknown-linux-musl",
  "conduit-aarch64-unknown-linux-musl",
  "conduit-aarch64-pc-windows-msvc.exe",
  "conduit-i686-unknown-linux-gnu",
  "conduit-armv7-unknown-linux-gnueabihf",
];
const sha = (b) => createHash("sha256").update(b).digest("hex");

async function run(served, expectedBody, seed = null, platformKey = null) {
  const pkg = mkdtempSync(join(tmpdir(), "conduit-npm-"));
  mkdirSync(join(pkg, "bin"));
  cpSync(join(here, "..", "bin", "download.js"), join(pkg, "bin", "download.js"));
  cpSync(join(here, "..", "bin", "platform.js"), join(pkg, "bin", "platform.js"));
  writeFileSync(join(pkg, "package.json"), JSON.stringify({ version: "0.0.0", type: "module" }));
  if (expectedBody !== null) {
    const sums = Object.fromEntries(ASSETS.map((a) => [a, sha(expectedBody)]));
    writeFileSync(join(pkg, "bin", "checksums.json"), JSON.stringify(sums));
  }
  if (seed !== null) {
    mkdirSync(join(pkg, "bin", "native"));
    for (const a of ASSETS) writeFileSync(join(pkg, "bin", "native", a), seed);
  }
  const urls = [];
  const server = createServer((req, res) => {
    urls.push(req.url);
    res.end(served);
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const base = `http://127.0.0.1:${server.address().port}`;
  const code = await new Promise((resolve) => {
    const preload = platformKey ? ["-r", join(here, "platform-preload.cjs")] : [];
    const p = spawn(process.execPath, [...preload, join(pkg, "bin", "download.js")], {
      env: { ...process.env, CONDUIT_DOWNLOAD_BASE: base, CONDUIT_TEST_PLATFORM: platformKey ?? "" },
      stdio: "ignore",
    });
    p.on("exit", resolve);
  });
  server.close();
  const native = join(pkg, "bin", "native");
  const first = existsSync(native) ? readdirSync(native).map((f) => readFileSync(join(native, f), "utf8")) : [];
  return { code, contents: first, files: existsSync(native) ? readdirSync(native) : [], urls };
}

const supported = ["linux-x64", "linux-arm64", "darwin-x64", "darwin-arm64", "win32-x64"]
  .includes(`${process.platform}-${process.arch}`);

test("a download matching checksums.json is installed", { skip: !supported }, async () => {
  const body = Buffer.from("genuine binary");
  const r = await run(body, body);
  assert.equal(r.code, 0);
  assert.equal(r.files.length, 1);
});

test("a tampered download fails the install and is removed", { skip: !supported }, async () => {
  const r = await run(Buffer.from("tampered"), Buffer.from("genuine binary"));
  assert.notEqual(r.code, 0);
  assert.deepEqual(r.files, []);
});

test("a package without checksums.json refuses to install", { skip: !supported }, async () => {
  const r = await run(Buffer.from("anything"), null);
  assert.notEqual(r.code, 0);
  assert.deepEqual(r.files, []);
});

test("an existing binary that does not match its checksum is replaced", { skip: !supported }, async () => {
  const body = Buffer.from("genuine binary");
  const r = await run(body, body, "stale or modified");
  assert.equal(r.code, 0);
  assert.ok(r.contents.includes("genuine binary"));
  assert.ok(!r.files.some((f) => f.endsWith(".part")));
});

// Each row of the platform table asks for its own asset and installs it under that name: a swap of two rows
// (linux-x64 and linux-arm64, say) fails here although the same set of names is still listed.
const PLATFORM_ASSETS = {
  "linux-x64": "conduit-x86_64-unknown-linux-gnu",
  "linux-arm64": "conduit-aarch64-unknown-linux-gnu",
  "linux-x64-musl": "conduit-x86_64-unknown-linux-musl",
  "linux-arm64-musl": "conduit-aarch64-unknown-linux-musl",
  "darwin-x64": "conduit-x86_64-apple-darwin",
  "darwin-arm64": "conduit-aarch64-apple-darwin",
  "win32-x64": "conduit-x86_64-pc-windows-msvc.exe",
  "win32-arm64": "conduit-aarch64-pc-windows-msvc.exe",
  "linux-ia32": "conduit-i686-unknown-linux-gnu",
  "linux-arm": "conduit-armv7-unknown-linux-gnueabihf",
};

for (const [key, asset] of Object.entries(PLATFORM_ASSETS)) {
  test(`${key} downloads and installs ${asset}`, async () => {
    const body = Buffer.from("genuine binary");
    const r = await run(body, body, null, key);
    assert.equal(r.code, 0);
    assert.deepEqual(r.urls, [`/${asset}`]);
    assert.deepEqual(r.files, [asset]);
  });
}

// The launcher resolves the same file: with no binary installed it names the path it looked for.
for (const [key, asset] of Object.entries(PLATFORM_ASSETS)) {
  test(`the launcher looks for ${asset} on ${key}`, async () => {
    const pkg = mkdtempSync(join(tmpdir(), "conduit-npm-"));
    mkdirSync(join(pkg, "bin"));
    for (const f of ["conduit.js", "platform.js"]) cpSync(join(here, "..", "bin", f), join(pkg, "bin", f));
    writeFileSync(join(pkg, "package.json"), JSON.stringify({ version: "0.0.0", type: "module" }));
    const preload = join(here, "platform-preload.cjs");
    const { code, stderr } = await new Promise((resolve) => {
      const p = spawn(process.execPath, ["-r", preload, join(pkg, "bin", "conduit.js")], {
        env: { ...process.env, CONDUIT_TEST_PLATFORM: key },
        stdio: ["ignore", "ignore", "pipe"],
      });
      let err = "";
      p.stderr.on("data", (d) => (err += d));
      p.on("exit", (c) => resolve({ code: c, stderr: err }));
    });
    assert.equal(code, 1);
    assert.ok(stderr.includes(join("native", asset)), stderr);
  });
}

// A musl libc gets the static musl build, a glibc one the glibc build; other systems have no musl row.
test("the table picks the musl build only on Linux with musl", () => {
  assert.equal(assetFor("linux", "x64", true).name, "conduit-x86_64-unknown-linux-musl");
  assert.equal(assetFor("linux", "arm64", true).name, "conduit-aarch64-unknown-linux-musl");
  assert.equal(assetFor("linux", "x64", false).name, "conduit-x86_64-unknown-linux-gnu");
  assert.equal(assetFor("darwin", "arm64", true).name, "conduit-aarch64-apple-darwin");
  assert.equal(assetFor("linux", "riscv64", true).name, undefined);
});

// Detection: any musl signal wins; without one, glibc must show itself in the runtime report. A musl host whose
// runtime report fails (no report, an exception) is still recognised by its loader or Alpine's release file.
const throwing = () => { throw new Error("no report"); };
const glibcReport = () => ({ glibcVersionRuntime: "2.36" });
const noFiles = () => false;
const only = (...present) => (path) => present.includes(path);

test("isMusl recognises a musl host by any of its files, even when the runtime report fails", () => {
  for (const marker of ["/lib/ld-musl-x86_64.so.1", "/lib/ld-musl-aarch64.so.1", "/etc/alpine-release"]) {
    assert.equal(isMusl("linux", { exists: only(marker), report: throwing }), true, marker);
    assert.equal(isMusl("linux", { exists: only(marker), report: glibcReport }), true, `${marker} beats a glibc report`);
  }
});

test("isMusl falls back to the runtime report: no glibc version means musl, a version means glibc", () => {
  assert.equal(isMusl("linux", { exists: noFiles, report: () => ({}) }), true);
  assert.equal(isMusl("linux", { exists: noFiles, report: glibcReport }), false);
});

test("isMusl answers glibc when nothing can be learned, and never on another OS", () => {
  assert.equal(isMusl("linux", { exists: noFiles, report: throwing }), false);
  assert.equal(isMusl("linux", { exists: noFiles, report: () => undefined }), false);
  assert.equal(isMusl("darwin", { exists: only("/etc/alpine-release"), report: () => ({}) }), false);
  assert.equal(isMusl("win32", { exists: only("/etc/alpine-release"), report: () => ({}) }), false);
});
