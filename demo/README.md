# Conduit Demo

A self-contained demo that shows Conduit's core features:
static file serving, reverse proxy, CORS, compression, rate limiting, health check, and more.

## Prerequisites

- **Node.js 18+** (for the mock API server)
- **Conduit** binary in your PATH, or built locally

## Quick start

### Option A — VS Code

Open the project in VS Code and run the **"Demo: Start (Conduit + API)"** task
(`Terminal → Run Task…` or `Ctrl+Shift+B`).

Then open [http://localhost:8080](http://localhost:8080) in your browser.

### Option B — Terminal (two windows)

**Window 1 — mock API server:**
```bash
node demo/api/server.js
```

**Window 2 — Conduit:**
```bash
# Using a locally built binary
cargo run --release -- -c demo/conduit.json

# Or, if conduit is installed globally
conduit -c demo/conduit.json
```

Open [http://localhost:8080](http://localhost:8080).

## What the demo shows

| Feature | Where |
| --- | --- |
| **Static files** | `demo/dist/` served at `/` |
| **Reverse proxy** | `/api/*` → `http://localhost:4000` with `stripPrefix: true` |
| **CORS** | Enabled for all origins |
| **Compression** | br / gzip for all text responses |
| **Security headers** | X-Content-Type-Options, X-Frame-Options, etc. |
| **X-Response-Time** | Added to every response |
| **Rate limiting** | 300 req / 60 s per IP |
| **Health check** | `/__health__` |
| **Prometheus metrics** | `/__metrics__` |
| **Redirect** | `/old-page` → `/` (301) |
| **SPA fallback** | HTML requests get `index.html`; JSON requests get 404 JSON |

## Config file

The demo uses `demo/conduit.json`. Edit it and run `conduit reload` (if the server is running)
or restart Conduit to apply changes.
