/**
 * Demo API server — mock backends for the Conduit demo.
 *
 * Starts two identical HTTP servers on ports 4000 and 4001 so that the
 * round-robin load balancing on site 1 (port 8080) can be observed live.
 *
 * Run with: node demo/api/server.js
 */

import { createServer } from 'node:http';

const PORTS = [4000, 4001];

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
  // Do NOT set Content-Length. Conduit may compress the response and
  // rewrite the body length; an explicit upstream Content-Length would
  // mismatch the compressed size and corrupt the response.
  // Node automatically uses chunked transfer-encoding when no
  // Content-Length is provided, which is correct here.
  res.writeHead(statusCode, { 'Content-Type': 'application/json' });
  res.end(json);
}

function makeRoutes(port) {
  return {
    'GET /': (_, res) =>
      send(res, 200, {
        service: 'conduit-demo-api',
        port,
        note: 'This is a mock backend. Access it through Conduit at http://localhost:8080/api/*',
        routes: [
          'GET  /health',
          'GET  /users',
          'GET  /products',
          'GET  /info',
          'POST /echo',
        ],
      }),
    'GET /health':   (_, res) => send(res, 200, { status: 'ok', service: 'demo-api', port }),
    'GET /users':    (_, res) => send(res, 200, { users, servedBy: port }),
    'GET /products': (_, res) => send(res, 200, { products, servedBy: port }),
    'GET /info': (req, res) =>
      send(res, 200, {
        message: 'Conduit demo API',
        servedBy: port,
        receivedHeaders: Object.fromEntries(
          ['host', 'x-forwarded-for', 'x-forwarded-proto', 'x-response-time', 'x-site']
            .map((h) => [h, req.headers[h] ?? null]),
        ),
        timestamp: new Date().toISOString(),
      }),
    'POST /echo': async (req, res) => {
      const chunks = [];
      for await (const chunk of req) chunks.push(chunk);
      let body;
      try { body = JSON.parse(Buffer.concat(chunks).toString()); }
      catch { body = { raw: Buffer.concat(chunks).toString() }; }
      send(res, 200, { echo: body, servedBy: port });
    },
  };
}

for (const port of PORTS) {
  const routes = makeRoutes(port);

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

  server.listen(port, () => {
    console.log(`[demo-api] Instance :${port} ready`);
  });

  server.on('error', (err) => {
    if (err.code === 'EADDRINUSE') {
      console.error(`[demo-api] Port ${port} is already in use.`);
    } else {
      console.error(`[demo-api :${port}]`, err);
    }
    process.exit(1);
  });
}

console.log('');
console.log('[demo-api] Two instances started on ports 4000 and 4001');
console.log('[demo-api] Conduit round-robins across both — watch "servedBy" in responses');
console.log('');
console.log('  GET  /health    → health check');
console.log('  GET  /users     → list users');
console.log('  GET  /products  → list products');
console.log('  GET  /info      → request info (shows proxy headers)');
console.log('  POST /echo      → echo the request body');
