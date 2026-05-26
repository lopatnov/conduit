// @ts-check
/**
 * Conduit demo integration tests.
 *
 * These tests run against a live Conduit + demo API instance.
 * Playwright starts both servers automatically via playwright.config.js.
 * You can also run them against already-running servers:
 *
 *   node api/server.js &
 *   ../target/debug/conduit -c conduit.json &
 *   npx playwright test
 */
import { test, expect } from '@playwright/test';

// ── Static files ─────────────────────────────────────────────────────────────

test('GET / returns the demo HTML page', async ({ request }) => {
  const res = await request.get('/');
  expect(res.status()).toBe(200);
  expect(res.headers()['content-type']).toContain('text/html');
  const body = await res.text();
  expect(body).toContain('Conduit');
});

// ── Proxy ────────────────────────────────────────────────────────────────────

test('GET /api/users is proxied and returns valid JSON', async ({ request }) => {
  const res = await request.get('/api/users');
  expect(res.status()).toBe(200);
  expect(res.headers()['content-type']).toContain('application/json');

  const body = await res.json();
  expect(body).toHaveProperty('users');
  expect(body).toHaveProperty('servedBy');
  expect(Array.isArray(body.users)).toBe(true);
  expect(body.users.length).toBeGreaterThan(0);
  expect([4000, 4001]).toContain(body.servedBy);
});

test('GET /api/products is proxied and returns valid JSON', async ({ request }) => {
  const res = await request.get('/api/products');
  expect(res.status()).toBe(200);
  const body = await res.json();
  expect(body).toHaveProperty('products');
  expect(body).toHaveProperty('servedBy');
});

test('GET /api/info includes Conduit proxy headers', async ({ request }) => {
  const res = await request.get('/api/info');
  expect(res.status()).toBe(200);
  const body = await res.json();
  // Conduit must inject X-Forwarded-For and X-Site
  expect(body.receivedHeaders['x-forwarded-for']).toBeTruthy();
  expect(body.receivedHeaders['x-site']).toBe('public');
});

test('POST /api/echo echoes the request body', async ({ request }) => {
  const payload = { hello: 'conduit', n: 42 };
  const res = await request.post('/api/echo', { data: payload });
  expect(res.status()).toBe(200);
  const body = await res.json();
  expect(body.echo).toMatchObject(payload);
  expect([4000, 4001]).toContain(body.servedBy);
});

// ── Round-robin load balancing ────────────────────────────────────────────────

test('round-robin: POST /api/echo distributes across both backends', async ({ request }) => {
  // /api/echo is excluded from the proxy cache (skipPaths: ["/echo"]),
  // so every POST hits a real upstream — ideal for verifying round-robin.
  const backends = new Set();
  for (let i = 0; i < 6; i++) {
    const res = await request.post('/api/echo', { data: { i } });
    expect(res.status()).toBe(200);
    const body = await res.json();
    backends.add(body.servedBy);
  }
  // Both :4000 and :4001 should have been used at least once.
  expect(backends.size).toBe(2);
});

// ── Proxy cache ───────────────────────────────────────────────────────────────

test('proxy cache: repeated GET /api/users hits the same backend within TTL', async ({ request }) => {
  // First request fills the cache.
  const r1 = await request.get('/api/users');
  const b1 = await r1.json();

  // Second request within the 10 s TTL must be served from cache
  // (same servedBy port as the first request).
  const r2 = await request.get('/api/users');
  const b2 = await r2.json();

  expect(b1.servedBy).toBe(b2.servedBy);
});

// ── Health check ──────────────────────────────────────────────────────────────

test('GET /__health__ returns status ok', async ({ request }) => {
  const res = await request.get('/__health__');
  expect(res.status()).toBe(200);
  const body = await res.json();
  expect(body.status).toBe('ok');
});

// ── Prometheus metrics ────────────────────────────────────────────────────────

test('GET /__metrics__ returns Prometheus text', async ({ request }) => {
  const res = await request.get('/__metrics__');
  expect(res.status()).toBe(200);
  expect(res.headers()['content-type']).toContain('text/plain');
  const body = await res.text();
  expect(body).toContain('conduit_requests_total');
});

// ── Security headers ──────────────────────────────────────────────────────────

test('responses include security headers', async ({ request }) => {
  const res = await request.get('/');
  const h = res.headers();
  expect(h['x-content-type-options']).toBe('nosniff');
  expect(h['x-frame-options']).toBeTruthy();
  expect(h['referrer-policy']).toBeTruthy();
  expect(h['x-powered-by']).toBe('Conduit');
  expect(h['x-response-time']).toMatch(/\d+(\.\d+)?ms/);
});

// ── Redirects ─────────────────────────────────────────────────────────────────

test('GET /old-page redirects 301 to /', async ({ request }) => {
  const res = await request.get('/old-page', { maxRedirects: 0 });
  expect(res.status()).toBe(301);
  expect(res.headers()['location']).toBe('/');
});

test('GET /docs/getting-started redirects 302 to /', async ({ request }) => {
  const res = await request.get('/docs/getting-started', { maxRedirects: 0 });
  expect(res.status()).toBe(302);
  expect(res.headers()['location']).toBe('/');
});

// ── SPA fallback ──────────────────────────────────────────────────────────────

test('unknown HTML path falls back to index.html (SPA mode)', async ({ request }) => {
  const res = await request.get('/some/deep/route', {
    headers: { 'Accept': 'text/html' },
  });
  expect(res.status()).toBe(200);
  const body = await res.text();
  expect(body).toContain('Conduit');
});

test('unknown JSON path returns 404 JSON (SPA mode)', async ({ request }) => {
  const res = await request.get('/some/deep/route', {
    headers: { 'Accept': 'application/json' },
  });
  expect(res.status()).toBe(404);
  const body = await res.json();
  expect(body).toHaveProperty('error');
});

// ── Admin site (port 8081) ────────────────────────────────────────────────────

test('admin panel (port 8081) requires Basic Auth', async ({ request }) => {
  const res = await request.get('http://localhost:8081/');
  expect(res.status()).toBe(401);
  expect(res.headers()['www-authenticate']).toContain('Basic');
});

test('admin panel (port 8081) accepts correct credentials', async ({ request }) => {
  const res = await request.get('http://localhost:8081/', {
    headers: {
      // admin:demo1234 in base64
      'Authorization': 'Basic ' + Buffer.from('admin:demo1234').toString('base64'),
    },
  });
  expect(res.status()).toBe(200);
});

test('admin panel health check is public (no auth required)', async ({ request }) => {
  const res = await request.get('http://localhost:8081/__health__');
  expect(res.status()).toBe(200);
});
