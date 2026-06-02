# Live Demo

The repository includes a self-contained demo with **two virtual sites running
from a single Conduit process**: a public app with round-robin load balancing,
proxy caching, compression, redirects, and an admin panel protected by Basic Auth.

## Prerequisites

- **Node.js 18+** — runs the mock API backends
- **Conduit binary** — either built locally or installed via npm/cargo

```bash
# Verify
node --version   # v18+ required
conduit --version
```

> **Cache feature:** the demo uses in-memory response caching. With the
> standard binary (`npx @lopatnov/conduit` / `cargo install`) the cache is
> silently disabled — the demo still works but responses won't be cached.
> For caching, use the full binary or build with `--features cache`.

---

## Running the demo

```bash
# Terminal 1 — start two mock API instances on ports 4000 and 4001
node demo/api/server.js

# Terminal 2 — start Conduit with the demo config
conduit -c demo/conduit.json
```

**VS Code users:** run the _"Demo: Start (Conduit + API)"_ task
(`Terminal → Run Task…` or `Ctrl+Shift+B`) to launch both processes at once.

---

## What's running

| URL | Description |
| --- | --- |
| [http://localhost:8080](http://localhost:8080) | Public app — static files, proxied API, caching, compression, rate limiting |
| [http://localhost:8081](http://localhost:8081) | Admin panel — protected with Basic Auth (`admin` / `demo1234`) |

---

## What the demo shows

| Feature | Where to see it |
| ------- | --------------- |
| **Two virtual sites** from one process | Two ports, one binary |
| **Round-robin load balancing** | `/api/*` alternates between `:4000` and `:4001`; `servedBy` field shows which |
| **Proxy cache (10 s TTL)** | `/api/users` and `/api/products` — second request returns from cache |
| **Basic Auth** | `http://localhost:8081` — browser shows native login dialog |
| **Rate limiting** | Hit `http://localhost:8080` rapidly → `429 Too Many Requests` after 300 req/min |
| **Compression** | `Accept-Encoding: br` → Brotli-compressed static assets |
| **Redirects** | `GET /old-page` → 301 to `/`; `GET /docs/x` → 302 to `/` |
| **SPA fallback** | HTML requests return `index.html`; JSON requests return 404 JSON |
| **Security headers** | `X-Content-Type-Options`, `X-Frame-Options`, etc. on every response |
| **Health endpoint** | `GET /__health__` — includes upstream status |
| **Prometheus metrics** | `GET /__metrics__` — request counts, latencies, cache hits |
| **X-Response-Time** | Every response includes `X-Response-Time: <ms>` |

---

## Admin API

The Admin API runs on loopback at `127.0.0.1:2019`. While the demo is running:

```bash
# Server status (version, uptime, in-flight requests)
conduit status

# Upstream health and latency
conduit status --upstream

# Live upstream list
conduit upstreams

# Add a third backend at runtime (no restart needed)
conduit upstreams add --route /api --target http://127.0.0.1:4002

# Hot-reload the config
conduit reload

# Graceful shutdown
conduit shutdown
```

---

## Demo config at a glance

The full config is in [`demo/conduit.json`](../demo/conduit.json).
Key sections:

```json
{
  "global": {
    "workers": 2,
    "admin": { "bind": "127.0.0.1:2019" }
  },
  "sites": [
    {
      "port": 8080,
      "logging": "dev",
      "cors": true,
      "compression": true,
      "securityHeaders": true,
      "responseTime": true,
      "rateLimit": { "windowSecs": 60, "limit": 300 },
      "static": "./demo/dist",
      "proxy": {
        "/api": {
          "targets": ["http://127.0.0.1:4000", "http://127.0.0.1:4001"],
          "strategy": "round-robin",
          "stripPrefix": true,
          "retry": { "attempts": 2, "conditions": ["connection_error"] },
          "cache": { "store": "memory", "ttlSecs": 10 }
        }
      },
      "redirects": [
        { "from": "/old-page", "to": "/", "status": 301 }
      ],
      "healthCheck": { "path": "/__health__", "includeUpstreams": true },
      "metrics": { "path": "/__metrics__" },
      "fallback": {
        "byAccept": {
          "html": { "status": 200, "file": "./demo/dist/index.html" },
          "json": { "status": 404, "body": { "error": "Not Found" } }
        }
      }
    },
    {
      "port": 8081,
      "basicAuth": {
        "users": { "admin": "demo1234" },
        "realm": "Conduit Demo Admin"
      },
      "static": "./demo/admin",
      "proxy": { "/api": { "targets": ["http://127.0.0.1:4000"], "stripPrefix": true } }
    }
  ]
}
```

> Passwords in `basicAuth.users` should use environment variables in production:
> `{ "admin": "$ADMIN_PASSWORD" }`.

See [`demo/README.md`](../demo/README.md) for a full walkthrough of every feature.

---

## Middleware pipeline demo

> **Requires** `cargo build --features "rhai,wasm"`

A separate demo in `examples/middleware-demo/` shows a full four-stage
middleware pipeline — Rhai and WASM running together in request and response phases:

| # | File | Type | Phase | What it does |
|---|------|------|-------|--------------|
| 1 | `api-gate.rhai` | Rhai | request | API key check — `401`/`403` on bad/missing key |
| 2 | `header-injector.wasm` | WASM | request | Injects `X-Trace-Id` + `X-Wasm-Plugin` onto upstream request |
| 3 | `response-enricher.rhai` | Rhai | response | Adds `X-Served-By`, `X-Error-Category`; strips `Server`/`X-Powered-By` |
| 4 | `response-tagger.wasm` | WASM | response | Adds `X-Processed-By: wasm` to every response |

```bash
# Build with Rhai + WASM support
cargo build --features "rhai,wasm"

# Compile WAT → WASM (once)
wasm-tools parse examples/middleware-demo/header-injector.wat \
  -o examples/middleware-demo/header-injector.wasm
wasm-tools parse examples/middleware-demo/response-tagger.wat \
  -o examples/middleware-demo/response-tagger.wasm

# Start (proxies to httpbin.org — no local backends needed)
./target/debug/conduit -c examples/middleware-demo/conduit.yaml
```

```bash
# Missing API key → 401
curl -i http://localhost:8080/

# Valid key → 200 with all four injected headers
curl -i -H "X-Api-Key: demo-secret" http://localhost:8080/
# X-Trace-Id: <request-id>        ← WASM (step 2)
# X-Wasm-Plugin: header-injector  ← WASM (step 2)
# X-Served-By: demo-api           ← Rhai response (step 3)
# X-Processed-By: wasm            ← WASM response (step 4)
```

See [`examples/middleware-demo/README.md`](../examples/middleware-demo/README.md) for details.
