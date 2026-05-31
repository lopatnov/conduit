# Configuration Reference

All options are optional unless noted. Fields accept environment variable references —
`"$VAR"` is replaced with the value of `VAR` at startup.

Conduit reads `conduit.json` by default. Pass `-c path/to/file.json` to use another file.
YAML is also supported: `-c conduit.yaml` or `-c conduit.yml`.

---

## Table of Contents

- [`port` / `host`](#port--host)
- [`tls`](#tls)
- [`http2`](#http2)
- [`logging`](#logging)
- [`compression`](#compression)
- [`responseTime`](#responsetime)
- [`securityHeaders`](#securityheaders)
- [`cors`](#cors)
- [`ipFilter`](#ipfilter)
- [`limits`](#limits)
- [`rateLimit`](#ratelimit)
- [`basicAuth`](#basicauth)
- [`apiKey`](#apikey)
- [`redirects`](#redirects)
- [`static` / `staticOptions`](#static--staticoptions)
- [`proxy`](#proxy)
- [`routes` (advanced routing)](#routes-advanced-routing)
- [Load balancing](#load-balancing)
- [Proxy cache](#proxy-cache)
- [`healthCheck`](#healthcheck)
- [`upload`](#upload)
- [`hotReload`](#hotreload)
- [`middleware`](#middleware)
- [`metrics`](#metrics)
- [`fallback`](#fallback)
- [Multi-site (`global` + `sites`)](#multi-site-global--sites)
- [`jwtAuth`](#jwtauth) — JWT bearer-token validation
- [`forwardAuth`](#forwardauth) — external auth service
- [`requestTransform` / `responseTransform`](#requesttransform--responsetransform)
- [`maskErrors`](#maskerrors) — hide upstream 5xx details
- [`outlierDetection`](#outlierdetection) — passive health via 5xx tracking
- [`proxy.*.retry.budgetPercent`](#proxyretrybudgetpercent)
- [`proxy.*.mirror`](#proxymirror) — shadow traffic
- [`proxy.*.rateLimit`](#proxyratelimit-per-route) — per-route rate limiting
- [`proxy.*.upstreamTls`](#proxyupstreamtls) — upstream TLS verification
- [`global.admin.token`](#globaladmintoken) — Admin API auth
- [`logging.skipPaths`](#loggingskippaths)
- [`healthCheck.maxConnectionsPerUpstream`](#healthcheckmaxconnectionsperupstream) — circuit breaker
- [Header Transform V2 — JWT templates](#header-transform-v2--jwt-templates)
- [`global.otlp`](#globalotlp) — OpenTelemetry OTLP tracing
- [Prometheus Metrics](#prometheus-metrics)

---

### `port` / `host`

```json
{ "port": 8080 }
```

```json
{ "host": "app.example.com", "port": 443 }
```

`host` is used for virtual hosting — only requests matching the `Host` header are handled
by this site. Omit `host` to match any hostname (catch-all).

Default port: `3000`.

---

### `tls`

**Manual certificates:**

```json
{
  "port": 443,
  "tls": {
    "cert": "./certs/cert.pem",
    "key": "./certs/key.pem",
    "httpRedirectPort": 80
  }
}
```

**Auto-TLS via Let's Encrypt** (no cert/key needed):

```json
{
  "port": 443,
  "tls": {
    "acme": {
      "email": "admin@example.com",
      "storage": "./certs",
      "challenge": "http-01"
    }
  }
}
```

Conduit automatically obtains and renews certificates. `conduit validate` reports expiry status.

> Conduit uses **rustls** — not OpenSSL.

> **Single certificate per port (rustls limitation):** When multiple HTTPS sites
> share the same port, Conduit serves the *first* registered `tls.cert`/`tls.key`
> for *all* hostnames on that port — the rustls backend does not perform
> per-SNI certificate selection. To serve different certificates per hostname,
> assign each site to a different port. This limitation does not affect ACME
> sites that each have their own port.

---

### `http2`

```json
{
  "port": 443,
  "tls": { "cert": "./certs/cert.pem", "key": "./certs/key.pem" },
  "http2": true
}
```

| Field                  | Default | Description                         |
| ---------------------- | ------- | ----------------------------------- |
| `maxConcurrentStreams` | `100`   | Max parallel streams per connection |
| `initialWindowSize`    | `65535` | Flow control window (bytes)         |

---

### `logging`

Accepts `false`, `true`, a format string, or an object.

```json
{ "logging": "dev" }
```

```json
{ "logging": { "format": "combined", "file": "./logs/access.log" } }
```

| Format     | Description                                             |
| ---------- | ------------------------------------------------------- |
| `dev`      | Colorized, short — for development                      |
| `combined` | Apache Combined Log Format — for production             |
| `common`   | Apache Common Log Format                                |
| `short`    | Short, without timestamps                               |
| `json`     | Structured JSON — for log aggregation (ELK, Loki, etc.) |

---

### `compression`

Accepts `false`, `true`, or an object.

```json
{ "compression": true }
```

```json
{
  "compression": {
    "algorithms": ["br", "gzip"],
    "level": 6,
    "minBytes": 1024
  }
}
```

Conduit negotiates the best algorithm based on the client's `Accept-Encoding` header.
Brotli is preferred over gzip when the client supports both.

---

### `responseTime`

Adds `X-Response-Time: 1.23ms` to every response.

```json
{ "responseTime": true }
```

```json
{ "responseTime": { "digits": 3 } }
```

---

### `securityHeaders`

```json
{ "securityHeaders": true }
```

Headers added with `true`:

| Header                   | Value                             |
| ------------------------ | --------------------------------- |
| `X-Content-Type-Options` | `nosniff`                         |
| `X-Frame-Options`        | `SAMEORIGIN`                      |
| `Referrer-Policy`        | `strict-origin-when-cross-origin` |
| `X-XSS-Protection`       | `1; mode=block`                   |

Object form for HSTS and CSP:

```json
{
  "securityHeaders": {
    "contentSecurityPolicy": "default-src 'self'; img-src *",
    "hsts": "max-age=31536000; includeSubDomains",
    "frameOptions": "DENY"
  }
}
```

---

### `cors`

Accepts `false`, `true`, or an object.

```json
{ "cors": true }
```

For production, restrict to specific origins:

```json
{
  "cors": {
    "origins": ["https://app.example.com"],
    "methods": ["GET", "POST", "PUT", "DELETE", "OPTIONS"],
    "allowedHeaders": ["Content-Type", "Authorization"],
    "credentials": true,
    "maxAgeSecs": 86400
  }
}
```

CORS preflight (`OPTIONS`) requests bypass auth and rate limiting — browsers send them without
credentials.

---

### `ipFilter`

Applied before auth and rate limiting.

**Whitelist** — allow only these IPs/ranges:

```json
{ "ipFilter": { "allow": ["10.0.0.0/8", "192.168.0.0/16"] } }
```

**Blacklist** — block specific IPs:

```json
{ "ipFilter": { "deny": ["1.2.3.4", "5.6.7.0/24"] } }
```

**Behind another proxy:**

```json
{ "ipFilter": { "deny": ["1.2.3.4"], "trustProxy": true } }
```

Blocked requests receive `403 Forbidden`. IPv4, IPv6, and IPv4-mapped IPv6 are all supported.

---

### `limits`

```json
{
  "limits": {
    "maxBodyBytes": 1048576,
    "maxHeaderBytes": 8192,
    "timeoutSecs": 30
  }
}
```

| Field            | Description                  | Status code                           |
| ---------------- | ---------------------------- | ------------------------------------- |
| `maxBodyBytes`   | Max request body size        | `413 Request Entity Too Large`        |
| `maxHeaderBytes` | Max total header size        | `431 Request Header Fields Too Large` |
| `timeoutSecs`    | Per-request timeout fallback | applied to all proxy peer timeouts    |

---

### `rateLimit`

Token-bucket rate limiter.

```json
{
  "rateLimit": {
    "windowSecs": 60,
    "limit": 100
  }
}
```

**Key by a header** (API key, user ID, etc.):

```json
{
  "rateLimit": {
    "windowSecs": 60,
    "limit": 1000,
    "keyBy": "header:X-API-Key"
  }
}
```

**Redis-backed** — shared across multiple Conduit instances:

```json
{
  "rateLimit": {
    "windowSecs": 60,
    "limit": 100,
    "store": "redis://localhost:6379"
  }
}
```

Falls back to in-memory if Redis is unavailable.

| Field        | Default          | Description                                |
| ------------ | ---------------- | ------------------------------------------ |
| `windowSecs` | required         | Time window in seconds                     |
| `limit`      | required         | Max requests per window                    |
| `algorithm`  | `"token-bucket"` | Rate limit algorithm                       |
| `keyBy`      | `"ip"`           | `"ip"` or `"header:X-Name"`                |
| `skipPaths`  | `[]`             | Paths exempt from limiting (glob patterns) |
| `store`      | `"memory"`       | `"memory"` or `"redis://..."`              |

---

### `basicAuth`

```json
{
  "basicAuth": {
    "users": { "alice": "secret123", "bob": "$BOB_PASSWORD" },
    "challenge": true,
    "realm": "My App",
    "skipPaths": ["/__health__", "/public/**"]
  }
}
```

Use `$VAR` references to avoid storing passwords in the config file.

---

### `apiKey`

```json
{
  "apiKey": {
    "keys": ["$API_KEY_1", "$API_KEY_2"],
    "header": "X-API-Key",
    "skipPaths": ["/__health__", "/public/**"]
  }
}
```

---

### `redirects`

First matching rule wins. Supports `:param` captures and query string preservation.

```json
{
  "redirects": [
    { "from": "/old-page", "to": "/new-page", "status": 301 },
    { "from": "/blog/:slug", "to": "/posts/:slug", "status": 308 },
    { "from": "/docs", "to": "https://docs.example.com", "status": 302 }
  ]
}
```

| Status | Meaning                               |
| ------ | ------------------------------------- |
| `301`  | Moved Permanently                     |
| `302`  | Found — temporary redirect            |
| `307`  | Temporary Redirect (method preserved) |
| `308`  | Permanent Redirect (method preserved) |

---

### `static` / `staticOptions`

**Simple:**

```json
{ "static": "./dist" }
```

**Multiple directories** — searched in order:

```json
{ "static": ["./dist", "./public"] }
```

**Map URL prefixes to directories:**

```json
{ "static": { "/": "./dist", "/assets": "./assets" } }
```

**Options:**

```json
{
  "static": "./dist",
  "staticOptions": {
    "etag": true,
    "lastModified": true,
    "maxAge": "7d",
    "index": ["index.html"],
    "dotFiles": "ignore",
    "preCompressed": true
  }
}
```

`preCompressed: true` serves `.br` / `.gz` variants directly without re-compressing on the fly.

| Field           | Default          | Description                                      |
| --------------- | ---------------- | ------------------------------------------------ |
| `etag`          | `true`           | Generate ETag headers (enables 304 Not Modified) |
| `lastModified`  | `true`           | Set Last-Modified header                         |
| `maxAge`        | `"0"`            | Cache-Control max-age (`"1h"`, `"7d"`, `"1y"`)   |
| `index`         | `["index.html"]` | Directory index filenames                        |
| `dotFiles`      | `"ignore"`       | `"ignore"` \| `"allow"` \| `"deny"`              |
| `preCompressed` | `false`          | Serve `.br`/`.gz` sidecar files                  |

---

### `proxy`

**Simple — proxy everything:**

```json
{ "proxy": "http://localhost:4000" }
```

**Route-based:**

```json
{
  "proxy": {
    "/api": "http://api-server:4000",
    "/images": "http://image-server:5000"
  }
}
```

**Round-robin across multiple backends:**

```json
{
  "proxy": {
    "/api": ["http://b1:4000", "http://b2:4000", "http://b3:4000"]
  }
}
```

**Full form — with all options:**

```json
{
  "proxy": {
    "/api": {
      "targets": ["http://b1:4000", "http://b2:4000"],
      "strategy": "least-conn",
      "stripPrefix": true,
      "http2": false,
      "timeout": { "connectMs": 2000, "readMs": 30000 },
      "healthCheck": { "path": "/health", "intervalSecs": 10 },
      "retry": { "attempts": 3, "conditions": ["connection_error", "5xx"] },
      "cache": { "store": "memory", "ttlSecs": 300 }
    }
  }
}
```

**`stripPrefix`** — `GET /api/users` is forwarded as `GET /users` to the backend.

**Path rewrite** — regex-based, first match wins, applied after `stripPrefix`:

```json
{
  "proxy": {
    "/api": {
      "targets": ["http://backend:4000"],
      "rewrite": [{ "from": "^/v[0-9]+/(.+)$", "to": "/$1" }]
    }
  }
}
```

**Upstream groups** — two-level load balancing:

```json
{
  "proxy": {
    "/api": {
      "groups": [
        {
          "name": "us-east",
          "targets": ["http://us1:4000", "http://us2:4000"],
          "strategy": "least-conn"
        },
        {
          "name": "eu-west",
          "targets": ["http://eu1:4000", "http://eu2:4000"],
          "strategy": "least-conn"
        }
      ],
      "groupStrategy": "ip-hash"
    }
  }
}
```

**Retry conditions:**

| Condition          | Description                     |
| ------------------ | ------------------------------- |
| `connection_error` | Upstream is down or unreachable |
| `5xx`              | Upstream returns a 5xx response |
| `timeout`          | Read or write timeout           |

---

### `routes` (advanced routing)

Explicit route table evaluated in order; first match wins.

```json
{
  "routes": [
    {
      "match": {
        "path": "/api/**",
        "method": ["POST", "PUT", "PATCH", "DELETE"]
      },
      "proxy": {
        "targets": ["http://write-backend:4000"],
        "strategy": "least-conn"
      }
    },
    {
      "match": { "path": "/api/**" },
      "proxy": "http://read-backend:4000"
    },
    {
      "match": { "path": "/public/**" },
      "static": "./public"
    }
  ]
}
```

**Match criteria** (all present fields must match):

| Field     | Type                 | Description                                              |
| --------- | -------------------- | -------------------------------------------------------- |
| `path`    | glob string          | `*` — one segment, `**` — any depth. Default: match all. |
| `method`  | `string[]`           | HTTP methods (case-insensitive). Default: match all.     |
| `headers` | `{ name: pattern }`  | Header values (exact string or regex).                   |
| `query`   | `{ param: pattern }` | Query param values (exact or regex).                     |

Backward compatibility: top-level `proxy` and `static` are automatically converted to routes.

---

### Load balancing

Controlled by the `strategy` field inside a `proxy` route.

| Strategy             | Value                  | Description                                                |
| -------------------- | ---------------------- | ---------------------------------------------------------- |
| Round-robin          | `round-robin`          | Default. Rotate evenly across all healthy backends.                  |
| Weighted round-robin | `weighted-round-robin` | Respects the `weight` field.                                         |
| Random               | `random`               | Pick a backend at random each request.                               |
| Least connections    | `least-conn`           | Send to the backend with the fewest active connections.              |
| Least response time  | `least-response-time`  | Send to the backend with the lowest recent latency.                  |
| IP hash              | `ip-hash`              | Sticky sessions — same client IP always hits same backend.           |
| Consistent hash      | `consistent-hash`      | Ketama ring — minimal reshuffling when backends change.              |
| P2C                  | `p2c`                  | Power of Two Choices — sample 2 random backends, pick less-loaded. Better tail latency than least-conn at scale. |

**Sticky sessions via cookie** — route by cookie value instead of IP:

```json
{
  "proxy": {
    "/api": {
      "targets": ["http://b1:4000", "http://b2:4000"],
      "sticky": { "cookie": "srv_id" }
    }
  }
}
```

The cookie value is used as a consistent-hash key — the same cookie always maps
to the same backend.  The server-side `Set-Cookie` is the responsibility of the
upstream application.

**Failover (primary + backup)**:

```json
{
  "proxy": {
    "/api": {
      "targets": ["http://primary:4000"],
      "backup": "http://fallback:4000",
      "healthCheck": { "path": "/health", "intervalSecs": 10 }
    }
  }
}
```

When all primary `targets` are unhealthy, traffic is routed to `backup`.

**Weighted round-robin** requires explicit weights:

```json
{
  "proxy": {
    "/api": {
      "targets": [
        { "url": "http://powerful:4000", "weight": 3 },
        { "url": "http://normal:4000", "weight": 1 }
      ],
      "strategy": "weighted-round-robin"
    }
  }
}
```

`hashKey` for `ip-hash` / `consistent-hash`: `"ip"` (default), `"url"`, or `"header:X-My-Key"`.

---

### Proxy cache

Cache upstream responses in memory, Redis, or on disk.

```json
{
  "proxy": {
    "/api": {
      "targets": ["http://backend:4000"],
      "cache": {
        "store": "memory",
        "maxSizeMb": 256,
        "ttlSecs": 300,
        "varyHeaders": ["Accept-Language"],
        "skipPaths": ["/api/auth/**", "/api/user/**"],
        "skipIfCookie": true,
        "methods": ["GET", "HEAD"]
      }
    }
  }
}
```

| `store` value    | Description                          |
| ---------------- | ------------------------------------ |
| `"memory"`       | In-process LRU — fastest, non-shared |
| `"redis://..."`  | Redis — shared across instances      |
| `"disk:./cache"` | Local filesystem — survives restarts |

---

### `healthCheck`

```json
{ "healthCheck": true }
```

Default path: `/__health__`. Always returns `200 OK`:

```json
{ "status": "ok", "uptime": 3600, "version": "1.0.0" }
```

Include upstream health:

```json
{ "healthCheck": { "path": "/health", "includeUpstreams": true } }
```

The health endpoint **bypasses auth, rate limiting, and IP filtering**.

---

### `upload`

Accept `multipart/form-data` file uploads.

```json
{
  "upload": {
    "path": "/upload",
    "dir": "./uploads",
    "maxFileSizeBytes": 10485760,
    "maxTotalSizeBytes": 52428800,
    "maxFiles": 5,
    "allowedMimeTypes": ["image/jpeg", "image/png", "application/pdf"]
  }
}
```

Uploaded files are saved with UUID-based names and the original extension.

---

### `hotReload`

Browser hot reload via SSE — useful for frontend development.

```json
{ "hotReload": true }
```

```json
{
  "hotReload": {
    "extensions": [".html", ".css", ".js", ".ts"],
    "path": "/__hot-reload__"
  }
}
```

Add to your HTML to auto-reload when files change:

```html
<script src="/__hot-reload__/client.js"></script>
```

---

### `middleware`

Custom request pipeline — executed after built-in guards (IP filter, rate limit, auth)
and before routing. Entries run in declared order; both Rhai scripts and WASM plugins
can be mixed freely.

```json
{
  "middleware": [
    { "type": "script", "path": "./scripts/custom-auth.rhai" },
    { "type": "wasm",   "path": "./plugins/jwt-validator.wasm" },
    { "type": "wasm",   "path": "./plugins/geo-block.wasm", "config": { "allow": ["EU"] } }
  ]
}
```

**`type: "script"` — Rhai scripting**

Scripts receive a `request` object (`.method`, `.path`, `.query`, `.header(name)`)
and a `response` object (`.status`, `.body`, `.header(name, value)`).
Return `false` to reject the request:

```rhai
// Require Authorization header
if request.header("Authorization") == "" {
    response.status = 401;
    response.header("WWW-Authenticate", "Bearer");
    false   // reject
} else {
    true    // continue
}
```

**`type: "wasm"` — WASM plugin** *(requires `--features wasm`)*

Plugins can be written in any language that compiles to WASM (Rust, Go, C,
AssemblyScript, …).  The plugin imports functions from the `"conduit"` namespace
and exports `on_request() -> i32` (0 = Continue, 1 = Abort).

```rust
// Minimal Rust plugin (compile with target = wasm32-unknown-unknown)
extern {
    fn conduit_get_header(name: *const u8, nlen: usize, buf: *mut u8, blen: usize) -> isize;
    fn conduit_set_response_status(status: i32);
}

#[no_mangle]
pub fn on_request() -> i32 {
    let name = b"x-api-key";
    let mut buf = [0u8; 256];
    let n = unsafe { conduit_get_header(name.as_ptr(), name.len(), buf.as_mut_ptr(), buf.len()) };
    if n < 0 {
        unsafe { conduit_set_response_status(401) };
        return 1; // Abort
    }
    0 // Continue
}
```

The optional `config` field is forwarded to the plugin as JSON bytes via
`conduit_get_plugin_config(buf, buf_len) -> i32`.

Both middleware types are **fail-open**: compile errors, missing files, and runtime
traps log a warning and let the request pass through.

| Field  | Required | Description |
|--------|----------|-------------|
| `type` | ✅ | `"script"` or `"wasm"` |
| `path` | ✅ | Path to `.rhai` script or `.wasm` binary |
| `config` | — | JSON object forwarded to WASM plugins as bytes |

---

### `faultInjection`

Inject artificial errors or delays for chaos engineering and resilience testing.
**Should not be used in production.**

```json
{
  "faultInjection": {
    "abort": { "percent": 5, "status": 503, "body": "Injected fault" },
    "delay": { "percent": 10, "ms": 500 }
  }
}
```

| Field | Description |
|---|---|
| `abort.percent` | Percentage of requests to abort (0–100) |
| `abort.status` | HTTP status code to return (default: 503) |
| `abort.body` | Response body text (default: `"Fault injected"`) |
| `delay.percent` | Percentage of requests to delay (0–100) |
| `delay.ms` | Delay in milliseconds |

Aborts are evaluated before delays — a request can only be aborted or delayed,
not both.

---

### `metrics`

Prometheus metrics endpoint.

```json
{ "metrics": { "path": "/__metrics__", "token": "$METRICS_TOKEN" } }
```

Metrics exposed:

| Metric                             | Type      | Description                      |
| ---------------------------------- | --------- | -------------------------------- |
| `conduit_requests_total`           | counter   | Total requests, by method/status |
| `conduit_request_duration_seconds` | histogram | Request latency                  |
| `conduit_cache_hits_total`         | counter   | Proxy cache hits                 |
| `conduit_cache_misses_total`       | counter   | Proxy cache misses               |

---

### `fallback`

Return a response when nothing else matched.

**SPA fallback:**

```json
{ "fallback": { "status": 200, "file": "./dist/index.html" } }
```

**Content-type aware:**

```json
{
  "fallback": {
    "byAccept": {
      "html": { "status": 200, "file": "./dist/index.html" },
      "json": { "status": 404, "body": { "error": "Not Found" } },
      "*": { "status": 200, "file": "./dist/index.html" }
    }
  }
}
```

---

### Multi-site (`global` + `sites`)

Run multiple virtual hosts from one Conduit process.

```json
{
  "global": {
    "workers": 4,
    "shutdownTimeoutSecs": 30,
    "admin": { "bind": "127.0.0.1:2019" }
  },
  "sites": [
    {
      "host": "app.example.com",
      "port": 443,
      "tls": { "cert": "$CERT", "key": "$KEY", "httpRedirectPort": 80 },
      "static": "./dist",
      "proxy": { "/api": "http://api:4000" }
    },
    {
      "host": "admin.example.com",
      "port": 443,
      "tls": { "cert": "$CERT", "key": "$KEY" },
      "basicAuth": { "users": { "admin": "$ADMIN_PASS" }, "challenge": true },
      "static": "./admin-ui"
    },
    {
      "host": "*",
      "port": 443,
      "tls": { "cert": "$CERT", "key": "$KEY" },
      "fallback": { "status": 404, "body": "Unknown host" }
    }
  ]
}
```

**Config forms** — three equivalent ways:

```jsonc
// Single site (most common)
{ "port": 3000, "static": "./dist" }

// Array of sites
[
  { "host": "a.com", "port": 443 },
  { "host": "b.com", "port": 443 }
]

// Full form with global settings
{ "global": { "workers": 4 }, "sites": [...] }
```

---

### `jwtAuth`

JWT bearer-token authentication. Validates `Authorization: Bearer <token>` on every
request unless the path is in `skipPaths`.

```jsonc
{
  // HS256 with a shared secret
  "jwtAuth": { "secret": "$JWT_SECRET" }

  // RS256/ES256 via JWKS endpoint (e.g. Auth0, Google, AWS Cognito)
  "jwtAuth": {
    "jwksUrl": "https://accounts.example.com/.well-known/jwks.json",
    "jwksRefreshSecs": 3600,      // re-fetch interval (default 3600)
    "audience": ["my-app"],
    "issuer": "https://accounts.example.com",
    "skipPaths": ["/__health__", "/public/**"]
  }
}
```

---

### `forwardAuth`

Delegate authentication to an external HTTP service.

```jsonc
{
  "forwardAuth": {
    "url": "http://auth-service:9000/verify",
    // Headers to forward from the original request to the auth service:
    "requestHeaders": ["Authorization", "Cookie", "X-Tenant-ID"],
    // Headers to copy from the auth service response to the upstream request:
    "responseHeaders": ["X-User-ID", "X-Role", "X-Scope"],
    "timeoutMs": 5000,
    "skipPaths": ["/__health__"]
  }
}
```

The auth service decision:
- **2xx** → request allowed; `responseHeaders` are injected into the upstream request.
- **4xx / 5xx** → that status is returned to the client.
- **Unreachable** → 401 (fail closed).

---

### `requestTransform` / `responseTransform`

Inject or remove headers from every upstream request or response.

```jsonc
{
  "requestTransform": {
    "setHeaders": { "X-Service-Name": "my-api", "X-Env": "production" },
    "removeHeaders": ["X-Internal-Token", "X-Debug"]
  },
  "responseTransform": {
    "setHeaders": { "X-Served-By": "conduit", "Cache-Control": "no-store" },
    "removeHeaders": ["X-Powered-By", "Server"]
  }
}
```

---

### `maskErrors`

Replace upstream 5xx response bodies with a generic JSON error to prevent
internal details from leaking to clients.

```jsonc
{ "maskErrors": true }
// → {"error":"Internal Server Error","status":500}
```

Set to `false` in development to see the actual upstream error body.

---

### `outlierDetection`

Passive health checking via consecutive 5xx tracking. Temporarily ejects
misbehaving upstreams from the pool without requiring an active probe.

```jsonc
{
  "outlierDetection": {
    "consecutive5xx": 5,            // eject after 5 consecutive 5xx
    "baseEjectionTimeSecs": 30,     // first ejection = 30s
    "maxEjectionTimeSecs": 300,     // max ejection = 5 min
    "maxEjectionPercent": 10        // never eject > 10% of the cluster
  }
}
```

Ejection duration uses exponential backoff: `base × 2^ejection_count`.

---

### `proxy.*.retry.budgetPercent`

Limits the number of in-flight retries to prevent retry storms.

```jsonc
{
  "proxy": {
    "/api": {
      "targets": ["http://b1:4000", "http://b2:4000"],
      "retry": {
        "attempts": 3,
        "conditions": ["connection_error", "5xx"],
        "budgetPercent": 20   // at most 20% of active requests may be retries
      }
    }
  }
}
```

---

### `proxy.*.mirror`

Fire-and-forget traffic mirroring to a secondary backend (shadow / dark launch).

```jsonc
{
  "proxy": {
    "/api": {
      "targets": ["http://primary:4000"],
      "mirror": "http://shadow:4000"
    }
  }
}
```

V1: only headers + method + path are mirrored (no body). Body mirroring deferred to V2.

---

### `proxy.*.rateLimit` (per-route)

Rate-limit individual proxy routes independently of the site-level `rateLimit`.

```jsonc
{
  "proxy": {
    "/api/heavy": {
      "targets": ["http://backend:4000"],
      "rateLimit": { "windowSecs": 60, "limit": 5, "keyBy": "ip" }
    }
  }
}
```

Site-level and per-route rate limits are evaluated independently (both must pass).

---

### `proxy.*.upstreamTls`

Control TLS certificate verification for `https://` upstream targets.

```jsonc
{
  "proxy": {
    "/api": {
      "targets": ["https://internal-service:4443"],
      "upstreamTls": {
        "verify": false,            // skip cert verification (self-signed certs)
        "serverName": "my-service"  // custom CN for cert verification
      }
    }
  }
}
```

By default Pingora verifies upstream certificates using the system CA store.
Only set `verify: false` in trusted internal networks.

---

### `global.admin.token`

Protect the Admin API with a bearer token.

```jsonc
{
  "global": {
    "admin": {
      "bind": "127.0.0.1:2019",
      "token": "$ADMIN_TOKEN"
    }
  }
}
```

All Admin API requests must include `Authorization: Bearer <token>`.
When absent, no authentication is enforced (backward-compatible).

---

### `logging.skipPaths`

Suppress specific paths from access logs (e.g. noisy health checks).

```jsonc
{
  "logging": {
    "format": "json",
    "file": "./logs/access.log",
    "skipPaths": ["/__health__", "/__metrics__", "/favicon.ico"]
  }
}
```

---

### `healthCheck.maxConnectionsPerUpstream`

Circuit breaker: limit concurrent in-flight connections per upstream.  When
**all** healthy upstreams reach this limit Conduit returns `503 Service
Unavailable` immediately without contacting any backend.

```jsonc
{
  "proxy": {
    "/api": {
      "targets": ["http://backend-a:4000", "http://backend-b:4000"],
      "healthCheck": {
        "maxConnectionsPerUpstream": 50
      }
    }
  }
}
```

**Behaviour:**
- Conduit tracks in-flight connections with an atomic counter per upstream URL.
- Works for all load-balancing strategies (not just `least-conn`).
- When one upstream is at the limit but others are below it, normal routing
  continues — only requests that would exceed ALL available upstreams get 503.
- The 503 response body is:
  `{"error":"Service Unavailable","status":503,"reason":"upstream_overloaded"}`

---

### Header Transform V2 — JWT templates

`requestTransform.setHeaders` values support `{{ jwt.<claim> }}` substitution
when `jwtAuth` is also configured.  The claims are decoded from the validated
token and substituted before the request is forwarded upstream.

```jsonc
{
  "jwtAuth":  { "secret": "$JWT_SECRET" },
  "requestTransform": {
    "setHeaders": {
      "X-User-ID":    "{{ jwt.sub }}",
      "X-User-Email": "{{ jwt.email }}",
      "X-Role":       "{{ jwt.role }}"
    }
  }
}
```

**Notes:**
- Unknown claims resolve to an empty string.
- Non-string claim values (numbers, arrays) are JSON-serialized.
- Template expansion only runs when `jwtAuth` is configured and the token is valid.
- `responseTransform` does NOT support templates (no JWT context at response time).

---

### `global.otlp`

Export distributed traces to any OTLP-compatible backend.  Requires
`--features otlp` at compile time.  When the feature is disabled the config
field is still accepted but silently ignored.

```jsonc
{
  "global": {
    "otlp": {
      "endpoint":    "http://otel-collector:4317",  // required
      "serviceName": "conduit-gateway",              // default: "conduit"
      "sampleRate":  1.0,                            // 0.0–1.0, default: 1.0
      "timeoutMs":   5000                            // default: 5000
    }
  }
}
```

**Supported backends:**
| Backend | Endpoint |
|---------|----------|
| Grafana Tempo | `http://tempo:4317` |
| Jaeger (OTLP receiver) | `http://jaeger:4317` |
| OpenTelemetry Collector | `http://otel-collector:4317` |
| Honeycomb | `https://api.honeycomb.io:443` |

**Trace attributes per span:**
- `http.method`, `http.path`, `http.status_code`, `http.duration_ms`
- `upstream.url` — selected upstream backend URL
- `request.id` — value of the `X-Request-ID` header

5xx responses are marked as errors in the trace (`span.status = ERROR`).

**Build:** `cargo build --release --features otlp`

---

## Prometheus Metrics

Conduit exposes the following metrics at the `/__metrics__` endpoint (or
configured `metrics.path`):

| Metric | Type | Description |
|---|---|---|
| `conduit_requests_total{method, status}` | Counter | All HTTP requests |
| `conduit_request_duration_seconds{method, status}` | Histogram | Request duration |
| `conduit_active_connections` | Gauge | Current in-flight requests |
| `conduit_upstream_errors_total{route, status}` | Counter | Upstream 5xx responses |
| `conduit_retry_attempts_total{route, condition}` | Counter | Retry attempts |
| `conduit_rate_limit_rejected_total{site}` | Counter | Rate-limited requests (429) |
| `conduit_cache_hits_total{route}` | Counter | Proxy cache hits |
| `conduit_cache_misses_total{route}` | Counter | Proxy cache misses |
