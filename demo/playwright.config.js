// @ts-check
import { defineConfig } from '@playwright/test';

// Path to the Conduit binary. Override via CONDUIT_BIN env var in CI.
const conduitBin = process.env.CONDUIT_BIN ?? '../target/debug/conduit';

export default defineConfig({
  testDir: './tests',
  timeout: 15_000,
  retries: 0,

  use: {
    baseURL: 'http://localhost:8080',
    // API tests — no browser needed, just HTTP.
    extraHTTPHeaders: { 'Accept': 'application/json' },
  },

  // Start both servers automatically before tests.
  // Set reuseExistingServer: true so running `conduit` + API manually also works.
  webServer: [
    {
      command: 'node api/server.js',
      port: 4001,
      reuseExistingServer: true,
      timeout: 5_000,
    },
    {
      command: `${conduitBin} -c conduit.json`,
      port: 8080,
      reuseExistingServer: true,
      timeout: 10_000,
      // Working directory is demo/ so conduit.json resolves correctly.
      cwd: new URL('.', import.meta.url).pathname.replace(/^\/([A-Z]:)/, '$1'),
    },
  ],
});
