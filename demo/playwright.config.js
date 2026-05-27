// @ts-check
import { defineConfig } from '@playwright/test';
import { resolve } from 'node:path';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const __dirname = fileURLToPath(new URL('.', import.meta.url));
const root = resolve(__dirname, '..');

// Resolve conduit binary: prefer CONDUIT_BIN env, then release, then debug.
// Appends .exe automatically on Windows.
function resolveConduit() {
  const ext = process.platform === 'win32' ? '.exe' : '';
  if (process.env.CONDUIT_BIN) return process.env.CONDUIT_BIN;
  const rel = resolve(root, `target/release/conduit${ext}`);
  const dbg = resolve(root, `target/debug/conduit${ext}`);
  return existsSync(rel) ? rel : dbg;
}

const conduitBin = resolveConduit();

export default defineConfig({
  testDir: './tests',
  timeout: 20_000,
  retries: 0,
  reporter: [['list'], ['html', { open: 'never' }]],

  use: {
    baseURL: 'http://localhost:8080',
  },

  // Start both servers automatically before running tests.
  // reuseExistingServer: true  — reuses already-running servers (dev workflow).
  webServer: [
    {
      command: 'node api/server.js',
      cwd: __dirname,
      port: 4001,
      reuseExistingServer: true,
      timeout: 8_000,
    },
    {
      command: `"${conduitBin}" -c demo/conduit.json`,
      cwd: root,
      port: 8080,
      reuseExistingServer: true,
      timeout: 15_000,
    },
  ],
});
