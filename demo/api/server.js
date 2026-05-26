/**
 * Demo API server — mock backend for the Conduit demo.
 * Run with: node demo/api/server.js
 */

import { createServer } from 'node:http';

const PORT = 4000;

const users = [
  { id: 1, name: 'Alice',  email: 'alice@example.com',  role: 'admin'  },
  { id: 2, name: 'Bob',    email: 'bob@example.com',    role: 'editor' },
  { id: 3, name: 'Carol',  email: 'carol@example.com',  role: 'viewer' },
];

const products = [
  { id: 1, name: 'Widget Pro',  price: 29.99, stock: 42  },
  { id: 2, name: 'Gadget Plus', price: 49.99, stock: 17  },
  { id: 3, name: 'Doohickey',   price: 9.99,  stock: 200 },
];

function send(res, statusCode, body) {
  const json = JSON.stringify(body, null, 2);
  res.writeHead(statusCode, {
    'Content-Type': 'application/json',
    'Content-Length': Buffer.byteLength(json),
  });
  res.end(json);
}

const routes = {
  'GET /health': (_, res) => send(res, 200, { status: 'ok', service: 'demo-api' }),
  'GET /users':  (_, res) => send(res, 200, { users }),
  'GET /products': (_, res) => send(res, 200, { products }),
  'GET /info': (req, res) =>
    send(res, 200, {
      message: 'Conduit demo API',
      receivedHeaders: Object.fromEntries(
        ['host', 'x-forwarded-for', 'x-forwarded-proto', 'x-response-time']
          .map((h) => [h, req.headers[h] ?? null])
      ),
      timestamp: new Date().toISOString(),
    }),
  'POST /echo': async (req, res) => {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    let body;
    try { body = JSON.parse(Buffer.concat(chunks).toString()); }
    catch { body = { raw: Buffer.concat(chunks).toString() }; }
    send(res, 200, { echo: body });
  },
};

const server = createServer((req, res) => {
  const key = `${req.method} ${req.url.split('?')[0]}`;
  const handler = routes[key];
  if (handler) {
    Promise.resolve(handler(req, res)).catch((err) => {
      console.error(err);
      send(res, 500, { error: 'Internal server error' });
    });
  } else {
    send(res, 404, { error: `No route for ${key}` });
  }
});

server.listen(PORT, () => {
  console.log(`[demo-api] Listening on http://localhost:${PORT}`);
  console.log('  GET  /health    → health check');
  console.log('  GET  /users     → list users');
  console.log('  GET  /products  → list products');
  console.log('  GET  /info      → request info (shows proxy headers)');
  console.log('  POST /echo      → echo the request body');
});

server.on('error', (err) => {
  if (err.code === 'EADDRINUSE') {
    console.error(`[demo-api] Port ${PORT} is already in use. Is the server already running?`);
  } else {
    console.error('[demo-api]', err);
  }
  process.exit(1);
});
