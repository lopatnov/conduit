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

const here = dirname(fileURLToPath(import.meta.url));
const ASSETS = [
  "conduit-x86_64-unknown-linux-gnu",
  "conduit-aarch64-unknown-linux-gnu",
  "conduit-x86_64-apple-darwin",
  "conduit-aarch64-apple-darwin",
  "conduit-x86_64-pc-windows-msvc.exe",
  "conduit-aarch64-pc-windows-msvc.exe",
];
const sha = (b) => createHash("sha256").update(b).digest("hex");

async function run(served, expectedBody, seed = null, platformKey = null) {
  const pkg = mkdtempSync(join(tmpdir(), "conduit-npm-"));
  mkdirSync(join(pkg, "bin"));
  cpSync(join(here, "..", "bin", "download.js"), join(pkg, "bin", "download.js"));
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

const supported = ["linux-x64", "linux-arm64", "darwin-x64", "darwin-arm64", "win32-x64", "win32-arm64"]
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
  "darwin-x64": "conduit-x86_64-apple-darwin",
  "darwin-arm64": "conduit-aarch64-apple-darwin",
  "win32-x64": "conduit-x86_64-pc-windows-msvc.exe",
  "win32-arm64": "conduit-aarch64-pc-windows-msvc.exe",
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
