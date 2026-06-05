# Conduit Demo

A self-contained demo running **two virtual sites from a single Conduit process** — just like
[express-reverse-proxy's demo](https://github.com/lopatnov/express-reverse-proxy) runs multiple
proxy instances, but here both sites share one binary.

| Site | Port | Description |
| --- | --- | --- |
| **Public app** | 8080 | Static files + round-robin proxy to two API backends |
| **Admin panel** | 8081 | Protected with Basic Auth (`admin / demo1234`) |

## Prerequisites

- **Node.js 18+** (for the mock API server)
- **Conduit** binary in your PATH, or built locally

## Quick start

### Option A — VS Code

Open the project in VS Code and run the **"Demo: Start (Conduit + API)"** task
(`Terminal → Run Task…` or `Ctrl+Shift+B`).

Then open:
- [http://localhost:8080](http://localhost:8080) — Public app
- [http://localhost:8081](http://localhost:8081) — Admin panel (login: `admin / demo1234`)

### Option B — Two terminals

**Terminal 1 — two mock API instances (ports 4000 and 4001):**

```bash
node demo/api/server.js
```

**Terminal 2 — Conduit (two virtual sites from one process):**

```bash
# Standard build is sufficient for this demo
cargo run --release -- -c demo/conduit.json
# or, if conduit is installed globally:
conduit -c demo/conduit.json
```

## What the demo shows

### Site 1 — Public app (port 8080)

| Feature | Details |
| --- | --- |
| **Static files** | `demo/dist/` served at `/` |
| **Round-robin proxy** | `/api/*` → `:4000` and `:4001` alternating; `servedBy` field shows which instance |
| **Proxy cache** | `/api/users` and `/api/products` cached in memory for 10 s; `/api/echo` excluded |
| **Compression** | br / gzip — auto-negotiated via `Accept-Encoding` |
| **CORS** | All origins allowed |
| **Security headers** | X-Content-Type-Options, X-Frame-Options, etc. |
| **X-Response-Time** | Added to every response |
| **Rate limiting** | 300 req / 60 s per IP (health/metrics exempt) |
| **Retry** | 2 attempts on `connection_error` |
| **Health check** | [`/__health__`](http://localhost:8080/__health__) with upstream status |
| **Prometheus metrics** | [`/__metrics__`](http://localhost:8080/__metrics__) |
| **Redirects** | `/old-page` → `/` (301), `/docs/:page` → `/` (302) |
| **SPA fallback** | HTML requests get `index.html`; JSON requests get 404 JSON |

### Site 2 — Admin panel (port 8081)

| Feature | Details |
| --- | --- |
| **Basic Auth** | `admin / demo1234` — browser shows native login dialog |
| **Static files** | `demo/admin/` served at `/` |
| **Proxy** | `/api/*` → `:4000` only (no load balancing) |
| **Health check** | [`/__health__`](http://localhost:8081/__health__) — bypasses auth |

### Admin API (port 2019)

The management API runs on loopback only. While the demo is running:

```bash
conduit status              # uptime, version, inflight requests
conduit upstreams           # health and latency of all backends
conduit upstreams add --route /api --target http://localhost:4002
                            # add a third backend — no restart needed
conduit reload              # reload conduit.json — no restart
conduit shutdown            # graceful shutdown
```

## Config file

The demo uses `demo/conduit.json` — a multi-site config with `global` + `sites` array.
Edit it while the server is running and apply with `conduit reload`.

## Directory layout

```text
demo/
├── conduit.json        multi-site config (two sites)
├── api/
│   └── server.js       two API instances on :4000 and :4001
├── dist/
│   └── index.html      public site UI (tabs: proxy, features, multi-site, config)
└── admin/
    └── index.html      admin panel (protected by Basic Auth)
```
