# Admin API Reference

The Admin API is a local HTTP server for managing a running Conduit instance:
hot-reloading config, inspecting upstream health, adjusting traffic routing, and
graceful shutdown — all without restarting the process.

---

## Table of Contents

- [Setup](#setup)
- [Authentication](#authentication)
- [Endpoints](#endpoints)
  - [GET /status](#get-status)
  - [POST /reload](#post-reload)
  - [POST /shutdown](#post-shutdown)
  - [GET /upstreams](#get-upstreams)
  - [POST /upstreams/add](#post-upstreamsadd)
  - [POST /upstreams/remove](#post-upstreamsremove)
  - [POST /upstreams/weight](#post-upstreamsweight)
  - [DELETE /cache/purge](#delete-cachepurge)
  - [POST /ip-deny](#post-ip-deny)
  - [DELETE /ip-deny](#delete-ip-deny)
- [CLI shortcuts](#cli-shortcuts)
- [Security](#security)

---

## Setup

The Admin HTTP server **only starts when `global.admin` is explicitly configured**.
Without it, no port is opened and the API is completely inaccessible.

Internal background tasks (upstream health checks, rate-limiter cleanup,
hot-reload file watcher) run regardless of admin config.

```yaml
# conduit.yaml
global:
  admin:
    bind: "127.0.0.1:2019"   # required — loopback only
    token: "$ADMIN_TOKEN"     # strongly recommended in production
```

```json
// conduit.json
{ "global": { "admin": { "bind": "127.0.0.1:2019", "token": "$ADMIN_TOKEN" } } }
```

> **Security:** Keep `bind` on loopback (`127.0.0.1`). Never bind to `0.0.0.0`
> without a VPN or SSH tunnel. Without a `token`, anyone with access to that
> address can reload configs, add upstreams, or shut down the server.

---

## Authentication

The Admin API uses **Bearer token authentication only** — no cookies, no
Basic Auth, no JWT.

When `global.admin.token` is set, every request must include:
```
Authorization: Bearer <token>
```
Requests without the correct token receive `401 Unauthorized`.

```bash
# Without token (works when token is not configured)
curl http://localhost:2019/status

# With token
curl -H "Authorization: Bearer $ADMIN_TOKEN" http://localhost:2019/status

# Using CONDUIT_ADMIN env var for address + inline token
export CONDUIT_ADMIN=127.0.0.1:2019
curl -H "Authorization: Bearer $ADMIN_TOKEN" http://$CONDUIT_ADMIN/status
```

The `CONDUIT_ADMIN` env var sets the **address** used by CLI shortcuts.
The token must always be passed explicitly via the `Authorization` header
(or via conduit CLI commands that read it from `CONDUIT_ADMIN_TOKEN`).

---

## Endpoints

### GET /status

Returns server status, version, and upstream health summary.

```bash
curl http://localhost:2019/status
```

**Response:**
```json
{
  "status": "running",
  "inflight": 42,
  "retry_inflight": 3,
  "sites": 2,
  "configured_upstreams": 4,
  "healthy_upstreams": 3,
  "total_probed_upstreams": 4,
  "config_path": "/etc/conduit/conduit.yaml"
}
```

| Field | Description |
| ----- | ----------- |
| `status` | Always `"running"` when the server is up |
| `inflight` | Requests currently being processed |
| `retry_inflight` | Requests currently in a retry attempt |
| `sites` | Number of configured virtual sites |
| `configured_upstreams` | Total upstream targets across all routes |
| `healthy_upstreams` | Upstreams currently passing health probes |
| `total_probed_upstreams` | Upstreams that have been probed at least once |
| `config_path` | Path to the loaded config file |

---

### POST /reload

Re-reads the config file from disk, validates it, and applies all
hot-reloadable changes without restarting.

```bash
curl -X POST http://localhost:2019/reload
```

**On success:**
```json
{ "status": "ok", "message": "config reloaded" }
```

**On validation error:**
```json
{
  "status": "error",
  "message": "config error at proxy./api.retry.attempts: must be > 0"
}
```

**What's hot-reloadable** (applied immediately, no restart):  
`proxy`, `static`, `routes`, `rateLimit`, `basicAuth`, `apiKey`, `jwtAuth`,
`forwardAuth`, `consumers`, `middleware`, `logging`, `cors`, `securityHeaders`,
`cache`, `outlierDetection`, `limits`, `requestTransform`, `responseTransform`,
`maskErrors`.

**What requires a cold restart:**  
`port`, `tls.cert/key`, `tls.versions/ciphers`, `workers`, `backlog`,
`global.admin.bind`.

> **Note:** `POST /reload` resets all runtime upstream overrides added via
> `/upstreams/add`, `/upstreams/remove`, and `/upstreams/weight`.

---

### POST /shutdown

Initiates a graceful shutdown: stops accepting new connections, waits for
all in-flight requests to complete, then exits.

```bash
curl -X POST http://localhost:2019/shutdown
```

**Response:**
```json
{ "status": "shutting_down" }
```

The shutdown timeout is controlled by `global.shutdownTimeoutSecs`.

---

### GET /upstreams

Returns health, latency, and routing information for all upstream targets.

```bash
curl http://localhost:2019/upstreams
```

**Response:**
```json
{
  "upstreams": [
    {
      "url": "http://api-1:4000",
      "healthy": true,
      "latency_ms": 12,
      "consecutive_failures": 0,
      "consecutive_successes": 5
    },
    {
      "url": "http://api-2:4000",
      "healthy": false,
      "latency_ms": null,
      "consecutive_failures": 3,
      "consecutive_successes": 0
    }
  ],
  "routes": [
    {
      "site": "api.example.com:443",
      "path": "/v1",
      "strategy": "least-conn",
      "targets": [
        {
          "url": "http://api-1:4000",
          "weight": 1,
          "healthy": true,
          "latency_ms": 12
        },
        {
          "url": "http://api-2:4000",
          "weight": 1,
          "healthy": false,
          "latency_ms": null
        }
      ]
    }
  ]
}
```

The response has two sections:
- `upstreams` — flat list of all known upstream URLs with their current health state
- `routes` — per-site, per-route view showing strategy and target weights

---

### POST /upstreams/add

Add an upstream target to a route at runtime. In-memory only — reset on
`POST /reload`.

```bash
curl -X POST http://localhost:2019/upstreams/add \
     -H "Content-Type: application/json" \
     -d '{"route": "/api", "target": "http://api-3:4000"}'
```

**Request body:**

| Field | Required | Description |
| ----- | -------- | ----------- |
| `route` | ✅ | Route path prefix, e.g. `"/api"` |
| `target` | ✅ | Full upstream URL, e.g. `"http://api-3:4000"` |
| `weight` | — | Weight for weighted-round-robin (default: `1`) |
| `site` | — | Scope to a specific site label, e.g. `"api.example.com:443"`. Omit to apply to all sites with this route |

**Response:**
```json
{
  "status": "ok",
  "site": "*",
  "route": "/api",
  "target": "http://api-3:4000",
  "weight": 1
}
```

---

### POST /upstreams/remove

Remove an upstream target from a route at runtime.

```bash
curl -X POST http://localhost:2019/upstreams/remove \
     -H "Content-Type: application/json" \
     -d '{"route": "/api", "target": "http://api-3:4000"}'
```

**Request body:** `route` and `target` (required), `site` (optional).

**Response:**
```json
{ "status": "ok", "removed": true, "site": "*", "route": "/api", "target": "http://api-3:4000" }
```

`"removed": false` when the target was not found for the given route.

---

### POST /upstreams/weight

Change the weight of an upstream target (effective for `weighted-round-robin`
strategy only).

```bash
# Give api-1 three times more traffic than api-2
curl -X POST http://localhost:2019/upstreams/weight \
     -H "Content-Type: application/json" \
     -d '{"route": "/api", "target": "http://api-1:4000", "weight": 3}'
```

**Request body:** `route`, `target`, `weight` (all required), `site` (optional).

**Response:**
```json
{ "status": "ok", "site": "*", "route": "/api", "target": "http://api-1:4000", "weight": 3 }
```

This is the HTTP equivalent of the `conduit upstreams weight` CLI command.

---

### DELETE /cache/purge

Invalidate a specific URL from the in-memory proxy cache.

```bash
curl -X DELETE "http://localhost:2019/cache/purge?url=https://api.example.com/v1/products"
```

**Query parameter:** `url` — the full URL to purge (scheme + host + path + query).

**Response:**
```json
{ "status": "ok", "purged": true, "url": "https://api.example.com/v1/products" }
```

`"purged": false` when no matching entry was found in the cache.

> Only the in-memory cache is supported. Redis cache purge is not yet
> implemented.

---

### POST /ip-deny

Add a CIDR to the runtime deny-list. Takes effect immediately for all new
requests. In-memory only — does not survive a restart or `POST /reload`.

```bash
curl -X POST http://localhost:2019/ip-deny \
     -H "Content-Type: application/json" \
     -d '{"cidr": "203.0.113.0/24"}'
```

**Request body:**

| Field | Required | Description |
| ----- | -------- | ----------- |
| `cidr` | ✅ | CIDR block or single IP, e.g. `"203.0.113.0/24"` or `"10.0.0.5"` |

**Response:**
```json
{ "status": "ok", "action": "added", "cidr": "203.0.113.0/24" }
```

To make the deny permanent, add the CIDR to `ipFilter.deny` in the config
and run `POST /reload`.

---

### DELETE /ip-deny

Remove a CIDR from the runtime deny-list.

```bash
curl -X DELETE http://localhost:2019/ip-deny \
     -H "Content-Type: application/json" \
     -d '{"cidr": "203.0.113.0/24"}'
```

**Response:**
```json
{ "status": "ok", "action": "removed", "cidr": "203.0.113.0/24" }
```

---

## CLI shortcuts

The `conduit` binary has built-in commands that wrap the Admin API:

```bash
# Default address (127.0.0.1:2019) — no flag needed
conduit reload
conduit status
conduit shutdown

# Custom address via flag
conduit reload   --admin 10.0.0.5:2019
conduit status   --admin 10.0.0.5:2019
conduit shutdown --admin 10.0.0.5:2019

# Custom address via environment variable (useful in scripts)
export CONDUIT_ADMIN=10.0.0.5:2019
conduit reload
conduit status

# Upstream management
conduit upstreams
conduit upstreams add    --route /api --target http://api-3:4000
conduit upstreams add    --route /api --target http://api-3:4000 --weight 2
conduit upstreams remove --route /api --target http://api-3:4000
conduit upstreams weight --route /api --target http://api-1:4000 --weight 3

# Scope a change to one specific site only
conduit upstreams add --route /api --target http://api-3:4000 --site api.example.com:443

# Upstream health table (human-readable)
conduit status --upstream
```

| CLI command | Admin API call |
| ----------- | -------------- |
| `conduit reload` | `POST /reload` |
| `conduit status` | `GET /status` |
| `conduit status --upstream` | `GET /upstreams` (formatted as table) |
| `conduit shutdown` | `POST /shutdown` |
| `conduit upstreams` | `GET /upstreams` |
| `conduit upstreams add` | `POST /upstreams/add` |
| `conduit upstreams remove` | `POST /upstreams/remove` |
| `conduit upstreams weight` | `POST /upstreams/weight` |

See [cli.md](cli.md) for all flags.

---

## Security

**Keep the Admin API on loopback.** The default bind address `127.0.0.1:2019`
is only reachable from the same host. Never expose it directly to a network
interface.

**Always set a token in production.** Without `global.admin.token`, anyone
with local access can reload configs, add upstreams, or shut down the server.

```yaml
global:
  admin:
    bind: "127.0.0.1:2019"
    token: "$ADMIN_TOKEN"   # read from environment variable
```

**Zero-downtime config update workflow:**

```bash
# 1. Edit conduit.yaml
vim /etc/conduit/conduit.yaml

# 2. Validate before applying (exits 1 if invalid)
conduit validate -c /etc/conduit/conduit.yaml

# 3. Apply — no restart, no dropped connections
conduit reload --admin 127.0.0.1:2019
```
