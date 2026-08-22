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
  - [GET /rate-limits](#get-rate-limits)
  - [POST /ip-deny](#post-ip-deny)
  - [DELETE /ip-deny](#delete-ip-deny)
  - [POST /certs/reload](#post-certsreload)
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
    bind: "127.0.0.1:2019" # required — loopback only
    token: "$ADMIN_TOKEN" # strongly recommended in production
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

### Generating a token

Use any method that produces a cryptographically random string. Store the
result in an environment variable — never hardcode it in the config file.

```bash
# openssl (recommended — available on Linux, macOS, Windows via Git Bash)
openssl rand -hex 32
# → e.g. a3f8c2d1b4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1

# /dev/urandom (Linux / macOS)
cat /dev/urandom | head -c 32 | base64

# Python 3
python3 -c "import secrets; print(secrets.token_hex(32))"

# Node.js
node -e "console.log(require('crypto').randomBytes(32).toString('hex'))"

# PowerShell (Windows)
[Convert]::ToBase64String((1..32 | ForEach-Object { Get-Random -Max 256 }))
```

Set it as an environment variable and reference it from the config:

```bash
# Generate once and save
export ADMIN_TOKEN=$(openssl rand -hex 32)

# Or store in a .env file (loaded by systemd EnvironmentFile=)
echo "ADMIN_TOKEN=$(openssl rand -hex 32)" >> /etc/conduit/conduit.env
```

```yaml
# conduit.yaml — reference the env var
global:
  admin:
    bind: "127.0.0.1:2019"
    token: "$ADMIN_TOKEN"
```

### Using the token

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

| Field                    | Description                                   |
| ------------------------ | --------------------------------------------- |
| `status`                 | Always `"running"` when the server is up      |
| `inflight`               | Requests currently being processed            |
| `retry_inflight`         | Requests currently in a retry attempt         |
| `sites`                  | Number of configured virtual sites            |
| `configured_upstreams`   | Total upstream targets across all routes      |
| `healthy_upstreams`      | Upstreams currently passing health probes     |
| `total_probed_upstreams` | Upstreams that have been probed at least once |
| `config_path`            | Path to the loaded config file                |

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

**On success with feature warnings** (feature configured but not compiled in):

```json
{
  "status": "ok",
  "message": "config reloaded",
  "warnings": ["jwtAuth is configured but --features jwt was not compiled in"]
}
```

**On validation error (400):**

```json
{
  "status": "error",
  "message": "config error at proxy./api.retry.attempts: must be > 0"
}
```

**On cold field change (400):**

```json
{
  "status": "error",
  "message": "cold fields changed — restart required: sites[0].tls.cert"
}
```

Cold fields must be changed via a process restart (`systemctl restart conduit`),
not via `POST /reload`.

**What's hot-reloadable** (applied immediately, no restart):  
`proxy`, `static`, `routes`, `rateLimit`, `basicAuth`, `apiKey`, `jwtAuth`,
`forwardAuth`, `consumers`, `middleware`, `logging`, `cors`, `securityHeaders`,
`cache`, `outlierDetection`, `limits`, `requestTransform`, `responseTransform`,
`maskErrors`.

**What requires a cold restart:**  
`port`, `tls.cert/key`, `tls.versions/ciphers`, `workers`, `backlog`,
`global.admin.bind`.

> **Note:** `POST /reload` resets runtime upstream overrides (added via
> `/upstreams/add`, `/upstreams/remove`, `/upstreams/weight`) and clears
> in-memory rate-limiter counters. Dynamic IP deny entries (`POST /ip-deny`)
> are **not** reset on reload — they persist until the process restarts.

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
      "state": "healthy",
      "latency_ms": 12,
      "ewma_latency_ms": 11,
      "consecutive_failures": 0,
      "consecutive_successes": 5,
      "consecutive_5xx": 0,
      "active_connections": 2,
      "ejected": false,
      "responses": { "2xx": 9821, "4xx": 3, "5xx": 0 },
      "selected":  { "total": 9824, "last_secs": 1749123456 }
    },
    {
      "url": "http://api-2:4000",
      "healthy": false,
      "state": "ejected",
      "latency_ms": null,
      "ewma_latency_ms": 0,
      "consecutive_failures": 3,
      "consecutive_successes": 0,
      "consecutive_5xx": 5,
      "active_connections": 0,
      "ejected": true,
      "responses": { "2xx": 4100, "4xx": 1, "5xx": 5 },
      "selected":  { "total": 4106, "last_secs": 1749120000 }
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
          "latency_ms": 12,
          "consecutive_failures": 0,
          "consecutive_successes": 5
        },
        {
          "url": "http://api-2:4000",
          "weight": 1,
          "healthy": false,
          "latency_ms": null,
          "consecutive_failures": 3,
          "consecutive_successes": 0
        }
      ]
    }
  ]
}
```

The response has two sections:

- `upstreams` — flat list of all known upstream URLs with detailed health state
- `routes` — per-site, per-route view showing strategy and target weights

**Upstream state values:**

| `state`      | Meaning                                                             |
| ------------ | ------------------------------------------------------------------- |
| `healthy`    | Passing health checks, no active connections                        |
| `busy`       | Active connections ≥ 1 (may indicate high load)                     |
| `unhealthy`  | Failing health checks                                               |
| `half-open`  | Ejection expired; next probe request will decide full recovery      |
| `ejected`    | Blocked by outlier detection; will re-join after backoff expires    |

**New fields (v1.1+):**

| Field                 | Description                                                        |
| --------------------- | ------------------------------------------------------------------ |
| `state`               | Human-readable state string (see table above)                      |
| `ewma_latency_ms`     | Exponentially-weighted moving average latency (passive, all traffic) |
| `consecutive_5xx`     | Consecutive 5xx responses since last success                       |
| `active_connections`  | In-flight requests currently being processed by this upstream      |
| `responses`           | Lifetime totals: `2xx`, `4xx`, `5xx` response counts               |
| `selected.total`      | Total times this upstream was chosen by the load balancer          |
| `selected.last_secs`  | Unix timestamp of the most recent selection                        |

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

| Field    | Required | Description                                                                                              |
| -------- | -------- | -------------------------------------------------------------------------------------------------------- |
| `route`  | ✅       | Route path prefix, e.g. `"/api"`                                                                         |
| `target` | ✅       | Full upstream URL, e.g. `"http://api-3:4000"`                                                            |
| `weight` | —        | Weight for weighted-round-robin (default: `1`)                                                           |
| `site`   | —        | Scope to a specific site label, e.g. `"api.example.com:443"`. Omit to apply to all sites with this route |

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
{
  "status": "ok",
  "removed": true,
  "site": "*",
  "route": "/api",
  "target": "http://api-3:4000"
}
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
{
  "status": "ok",
  "site": "*",
  "route": "/api",
  "target": "http://api-1:4000",
  "weight": 3
}
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

### GET /rate-limits

Return accumulated pass/reject counters for every rate-limit bucket currently
in memory. Useful for diagnosing which clients or routes are being throttled.

```bash
curl http://localhost:2019/rate-limits
```

**Response:**

```json
{
  "app.example.com:8080": {
    "/api": {
      "passed":   12345,
      "rejected": 3
    }
  },
  "*": {
    "*": {
      "passed":   99000,
      "rejected": 12
    }
  }
}
```

The outer key is the site label (`host:port` or `"*"` for wildcard rules) and the
inner key is the route prefix. Both `passed` and `rejected` are monotonically
increasing counters that reset when the process restarts or `POST /reload` is called
(reload clears in-memory rate-limiter state).

---

### POST /ip-deny

Add a CIDR to the runtime deny-list. Takes effect immediately for all new
requests. In-memory only — persists across `POST /reload` but resets on
process restart.

```bash
curl -X POST http://localhost:2019/ip-deny \
     -H "Content-Type: application/json" \
     -d '{"cidr": "203.0.113.0/24"}'
```

**Request body:**

| Field  | Required | Description                                                      |
| ------ | -------- | ---------------------------------------------------------------- |
| `cidr` | ✅       | CIDR block or single IP, e.g. `"203.0.113.0/24"` or `"10.0.0.5"` |

**Response:**

```json
{ "status": "ok", "action": "added", "cidr": "203.0.113.0/24" }
```

To make the deny **permanent** (survives restarts), add the CIDR to
`ipFilter.deny` in the config file and run `POST /reload`.

> **Note:** there is no `GET /ip-deny` endpoint. To inspect the current
> runtime deny list, check the `ipFilter.deny` config and any CIDRs added
> at runtime — the dynamic list is not exposed via the API.

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

> Always returns `"action": "removed"` even if the CIDR was not in the list.

---

### POST /certs/reload

Validate a new TLS certificate + private key and write them atomically to the
file paths configured in `tls.cert` / `tls.key`. A **restart is required
afterwards** for the new certificate to take effect on new connections —
`conduit reload` (`POST /reload`) does **not** activate it (issue #190): this
endpoint only rewrites file *content* at the existing paths, so `/reload`'s
cold-field detection (which compares config *values*) never sees a change
and reports success without the new certificate ever being loaded.

> **Why a restart?** Pingora 0.8's rustls backend builds an immutable
> `ServerConfig` at startup and has no runtime cert-swap API. Writing the
> files here is the safe atomic step; applying them without downtime will be
> possible once Pingora exposes a `ResolvesServerCert` hook (planned for 0.9+).
> For Let's Encrypt, use `tls.acme` instead — renewals are fully automatic.

**Request body:**

```json
{
  "cert": "-----BEGIN CERTIFICATE-----\n...\n-----END CERTIFICATE-----\n",
  "key": "-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----\n"
}
```

Both fields accept a full PEM string (including any intermediate certificates
chained after the leaf certificate for `cert`).

```bash
# Read new cert and key from files, send to Admin API
CERT=$(cat /tmp/new-server.crt)
KEY=$(cat /tmp/new-server.key)

curl -s -X POST http://localhost:2019/certs/reload \
     -H "Content-Type: application/json" \
     -H "Authorization: Bearer $TOKEN" \
     -d "{\"cert\": $(echo "$CERT" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))'), \
          \"key\":  $(echo "$KEY"  | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')}"
```

Or with `jq` (cleaner):

```bash
jq -n --rawfile cert /tmp/new-server.crt \
      --rawfile key  /tmp/new-server.key \
      '{cert: $cert, key: $key}' \
| curl -s -X POST http://localhost:2019/certs/reload \
       -H "Content-Type: application/json" \
       -H "Authorization: Bearer $TOKEN" \
       -d @-
```

**Success response (200):**

```json
{
  "status": "ok",
  "cert_path": "/etc/conduit/tls/server.crt",
  "key_path": "/etc/conduit/tls/server.key",
  "note": "certificate written to disk but NOT yet active — the running TLS listener was built once at startup and does not re-read cert/key files; POST /reload will not activate it either, since the config paths themselves haven't changed. Restart the process (or use --upgrade for a zero-downtime process swap) to actually serve the new certificate."
}
```

**Error responses:**

| Status | Cause                                                                        |
| ------ | ---------------------------------------------------------------------------- |
| `400`  | No site has `tls.cert`/`tls.key` configured                                  |
| `400`  | Cert and key do not form a valid pair (mismatch, corrupt PEM, no cert found) |
| `500`  | File write failed (permissions, disk full, …)                                |

**Typical workflow:**

```bash
# 1. Upload and validate the new certificate
jq -n --rawfile cert new.crt --rawfile key new.key '{cert: $cert, key: $key}' \
  | curl -sX POST http://localhost:2019/certs/reload \
         -H "Content-Type: application/json" \
         -H "Authorization: Bearer $ADMIN_TOKEN" \
         -d @-

# 2. Apply — POST /reload works here because the cert/key file PATHS in config
#    did not change (only the file contents changed), so no cold-field error.
curl -sX POST http://localhost:2019/reload \
     -H "Authorization: Bearer $ADMIN_TOKEN"

# If the cert/key paths themselves changed in conduit.yaml, a full restart is needed:
# systemctl restart conduit
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

| CLI command                 | Admin API call                        |
| --------------------------- | ------------------------------------- |
| `conduit reload`            | `POST /reload`                        |
| `conduit status`            | `GET /status`                         |
| `conduit status --upstream` | `GET /upstreams` (formatted as table) |
| `conduit shutdown`          | `POST /shutdown`                      |
| `conduit upstreams`         | `GET /upstreams`                      |
| `conduit upstreams add`     | `POST /upstreams/add`                 |
| `conduit upstreams remove`  | `POST /upstreams/remove`              |
| `conduit upstreams weight`  | `POST /upstreams/weight`              |

See [cli.md](cli.md) for all flags.

---

## Security

**Keep the Admin API on loopback.** Use `127.0.0.1` as the bind address —
it is only reachable from the same host. Never use `0.0.0.0` without a VPN
or SSH tunnel.

**Always set a token in production.** Without `global.admin.token`, anyone
with local access can reload configs, add upstreams, or shut down the server.

```yaml
global:
  admin:
    bind: "127.0.0.1:2019"
    token: "$ADMIN_TOKEN" # read from environment variable
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
