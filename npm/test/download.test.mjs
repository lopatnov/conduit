// Run with: node --test npm/test/download.test.mjs(#571)
import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import { cpSync, mkdtempSync, mkdirSync, writeFileSync, existsSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const ASSETS = [
  "conduit-x86_64-unknown-linux-gnu-full",
  "conduit-aarch64-unknown-linux-gnu-full",
  "conduit-x86_64-apple-darwin-full",
  "conduit-aarch64-apple-darwin-full",
  "conduit-x86_64-pc-windows-msvc-full.exe",
];
const sha = (b) => createHash("sha256").update(b).digest("hex");

async function run(served, expectedBody) {
  const pkg = mkdtempSync(join(tmpdir(), "conduit-npm-"));
  mkdirSync(join(pkg, "bin"));
  cpSync(join(here, "..", "bin", "download.js"), join(pkg, "bin", "download.js"));
  writeFileSync(join(pkg, "package.json"), JSON.stringify({ version: "0.0.0", type: "module" }));
  if (expectedBody !== null) {
    const sums = Object.fromEntries(ASSETS.map((a) => [a, sha(expectedBody)]));
    writeFileSync(join(pkg, "bin", "checksums.json"), JSON.stringify(sums));
  }
  const server = createServer((_, res) => res.end(served));
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const base = `http://127.0.0.1:${server.address().port}`;
  const code = await new Promise((resolve) => {
    const p = spawn(process.execPath, [join(pkg, "bin", "download.js")], {
      env: { ...process.env, CONDUIT_DOWNLOAD_BASE: base },
      stdio: "ignore",
    });
    p.on("exit", resolve);
  });
  server.close();
  const native = join(pkg, "bin", "native");
  return { code, files: existsSync(native) ? readdirSync(native) : [] };
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
