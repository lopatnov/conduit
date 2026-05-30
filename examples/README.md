# Configuration Examples

Each file is a ready-to-run Conduit configuration. Copy one that matches your use case
and adapt it, or use `conduit init` to generate one interactively.

```bash
conduit -c examples/minimal.json
conduit validate -c examples/spa-with-api.json
```

---

## Examples

### [minimal.json](minimal.json)

The smallest useful config: serve static files from `./dist` and proxy `/api` to a
local backend on port 4000.

**Key features:** `static`, `proxy` (single upstream)

---

### [spa-with-api.json](spa-with-api.json)

Production Single-Page Application with an API backend behind a load balancer.
Serves pre-compressed static assets with a long cache TTL, proxies `/api` across two
backends with least-connection balancing and a 60-second in-memory cache, and uses a
content-negotiation fallback so browsers get `index.html` while API clients get a
JSON 404.

**Key features:** `compression`, `staticOptions.preCompressed`, `proxy.strategy: least-conn`,
`cache`, `fallback.byAccept`, `healthCheck`, `metrics`

---

### [tls-h2.json](tls-h2.json)

HTTPS with manual certificates and HTTP/2 enabled. Redirects plain HTTP (port 80) to
HTTPS automatically. Demonstrates TLS configuration, H2 stream limits, security
headers, and static asset caching.

**Key features:** `tls.cert/key`, `tls.httpRedirectPort`, `http2`, `securityHeaders`,
`staticOptions.maxAge`

---

### [tls-acme.json](tls-acme.json)

Auto-TLS via Let's Encrypt — no manual certificate management. Conduit obtains and
renews certificates automatically using ACME HTTP-01 challenge. Includes dual API
backends with health checks and Prometheus metrics.

**Key features:** `tls.acme`, `proxy.healthCheck`, `compression`, `metrics`, `logging: json`

---

### [load-balanced.json](load-balanced.json)

Multi-strategy load balancing across three route groups on the same port:

| Route | Strategy | Purpose |
|---|---|---|
| `/api` | Weighted round-robin (3:1) | Route more traffic to the powerful server |
| `/auth` | IP-hash | Sticky sessions — same client always hits same backend |
| `/search` | Least connections | Spread search queries to the least-busy server |

Includes upstream health checks, automatic retry on `connection_error` and `5xx`, and
a Prometheus metrics endpoint.

**Key features:** `weighted-round-robin`, `ip-hash`, `least-conn`, `retry`, `healthCheck`

---

### [multi-site.json](multi-site.json)

Three virtual hosts from a single Conduit process using the `sites` array format:

- `app.example.com:443` — SPA with API proxy and two backends
- `admin.example.com:443` — Admin panel protected with Basic Auth
- `*:443` — Catch-all that returns 404 for unknown hostnames

**Key features:** virtual hosting, `basicAuth`, `tls`, `fallback`

---

### [with-cache.json](with-cache.json)

In-memory proxy cache with fine-grained control: 256 MB limit, 5-minute TTL, vary on
`Accept-Language` and `Accept-Encoding`, skip cache for auth paths and cookie-bearing
requests, cache only GET and HEAD.

**Key features:** `cache.store: memory`, `cache.varyHeaders`, `cache.skipIfCookie`,
`cache.skipPaths`

---

### [dev-hot-reload.json](dev-hot-reload.json)

Development server with browser hot reload. When HTML, CSS, JS, TS, or JSON files
change on disk, connected browsers automatically refresh — no build tool needed.
CORS is open and logging is in `dev` format (colorized, short).

**Key features:** `hotReload`, `cors: true`, `logging: dev`, `fallback` (SPA)

---

### [routes.json](routes.json)

Advanced routing with the `routes` array: match on path glob, HTTP method, and query
parameters. Separate read and write backends for the API, dedicated server for v2
traffic, IP filter on the admin port.

**Key features:** `routes[].match.method`, `routes[].match.path`, `ipFilter`

---

### [path-rewrite.json](path-rewrite.json)

Regex-based URL rewriting applied after `stripPrefix`. Strip version prefixes
(`/v1/`, `/v2/`) and remap legacy paths (`/users/*` → `/members/*`) before the
request reaches the upstream.

**Key features:** `proxy.rewrite`, regex capture groups (`$1`)

---

### [upstream-groups.json](upstream-groups.json)

Two-level load balancing: an outer strategy selects a server group (by IP hash, so
the same client always hits the same region), and an inner strategy spreads load
within that group (least connections).

| Outer (group selection) | Inner (within group) |
|---|---|
| `ip-hash` across `us-east` / `eu-west` | `least-conn` within each region |

**Key features:** `proxy.groups`, `groupStrategy`, nested load balancing

---

### [redis-rate-limit.json](redis-rate-limit.json)

Rate limiting backed by Redis instead of the default in-memory store. Enables
consistent rate limiting across multiple Conduit instances (e.g., behind a load
balancer). Falls back to in-memory if Redis is unavailable.

**Key features:** `rateLimit.store: redis://`, `rateLimit.keyBy: ip`, `skipPaths`

---

### [rhai-middleware.json](rhai-middleware.json)

Custom request middleware written in [Rhai](https://rhai.rs/) script. The script
runs in the filter pipeline — it can read request headers, set response headers, or
reject requests entirely.  Two bundled scripts in `scripts/` demonstrate API-key
enforcement and custom auth logic.

**Key features:** `middleware[].type: script`, `middleware[].path`, Rhai scripting

---

## Choosing a starting point

| I want to… | Start with |
|---|---|
| Quickly serve files + proxy | `minimal.json` |
| Deploy a React/Vue/Angular app | `spa-with-api.json` |
| Add HTTPS to an existing server | `tls-h2.json` or `tls-acme.json` |
| Scale across multiple servers | `load-balanced.json` |
| Run multiple apps on one machine | `multi-site.json` |
| Speed up a slow API with caching | `with-cache.json` |
| Work on a frontend locally | `dev-hot-reload.json` |
| Add custom auth or request logic | `rhai-middleware.json` |
