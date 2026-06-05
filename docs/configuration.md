# Configuration Reference

Conduit is configured with a single file in **JSON** or **YAML** format. Both are
fully equivalent — YAML is recommended because it supports comments.

```bash
conduit -c conduit.yaml          # explicit path
conduit -c conduit.json          # JSON is fine too
conduit                          # auto-discover: conduit.json → conduit.yaml → conduit.yml
conduit validate -c conduit.yaml # validate without starting
```

Fields accept **environment variable references** — `"$MY_VAR"` is replaced at
startup. This keeps secrets out of config files.

### Optional build features

The standard binary covers most use cases. Some config sections require a feature flag:

| Feature           | Flag                               | Config section                                              |
| ----------------- | ---------------------------------- | ----------------------------------------------------------- |
| `jwt`             | `--features jwt`                   | [`jwtAuth`](#jwt-auth) + [`{{ jwt.* }}`](#jwt-claim-templates) templates |
| `consumers`       | `--features consumers`             | [`consumers`](#consumers)                                   |
| `forward-auth`    | `--features forward-auth`          | [`forwardAuth`](#forward-auth)                              |
| `rhai`            | `--features rhai`                  | [`middleware[].type: "script"`](#rhai-script-middleware)    |
| `wasm`            | `--features wasm`                  | [`middleware[].type: "wasm"`](#wasm-middleware)             |
| `tcp`             | `--features tcp`                   | [`type: "tcp"` site](#tcp-proxy)                           |
| `upload`          | `--features upload`                | [`upload`](#upload)                                         |
| `redis`           | `--features redis`                 | `rateLimit.store: "redis://..."`, `cache.store: "redis://..."` |
| `cache`           | `--features cache`                 | [`proxy.*.cache`](#proxy-cache)                             |
| `disk-cache`      | `--features disk-cache`            | `cache.store: "disk:/path"`                                 |
| `acme`            | `--features acme`                  | [`tls.acme`](#auto-tls-via-lets-encrypt)                    |
| `fault-injection` | `--features fault-injection`       | [`faultInjection`](#fault-injection)                        |
| `otlp`            | `--features otlp`                  | [`global.otlp`](#opentelemetry-tracing)                     |
| `kubernetes`      | `--features kubernetes`            | `--kubernetes-namespace` CLI flag (not a config field)      |
| `full`            | `--features full`                  | All of the above                                            |

Download a `-full` binary from [GitHub Releases](https://github.com/lopatnov/conduit/releases)
or build from source: `cargo build --release --features full`.
See [docs/cli.md — Build features](cli.md#build-features) for binary sizes and details.

---

## Table of Contents

**Background**

- [Concepts](#concepts) — request pipeline, forwarded headers, skipPaths glob

**Essentials**

- [Port and host](#port-and-host)
- [TLS / HTTPS](#tls--https)
- [HTTP/2](#http2)
- [Compression](#compression)
- [Response time header](#response-time-header)

**Routing**

- [Proxy (reverse proxy)](#proxy)
- [Routes (advanced matching)](#routes)
- [Load balancing](#load-balancing)
- [Static files](#static-files)
- [Redirects](#redirects)
- [Fallback](#fallback)

**Reliability**

- [Health checks](#health-checks)
- [Circuit breaker](#circuit-breaker)
- [Retry](#retry)
- [Outlier detection](#outlier-detection)
- [Limits](#limits)
- [Fault injection](#fault-injection)

**Caching**

- [Proxy cache](#proxy-cache)

**Authentication**

- [Basic Auth](#basic-auth)
- [API Key](#api-key)
- [JWT Auth](#jwt-auth)
- [Forward Auth](#forward-auth)
- [Consumers](#consumers)

**Rate Limiting & Load Shedding**

- [Rate limiting](#rate-limiting)
- [Priority routing](#priority-routing)

**Transforms**

- [Request / response transform](#request--response-transform)
- [Traffic mirroring](#traffic-mirroring)
- [Custom response headers](#custom-response-headers)

**Observability**

- [Logging](#logging)
- [Metrics (Prometheus)](#metrics)
- [OpenTelemetry tracing](#opentelemetry-tracing)
- [Hot reload](#hot-reload)

**Security**

- [Security headers](#security-headers)
- [CORS](#cors)
- [IP filter](#ip-filter)
- [Error masking](#error-masking)
- [Upstream TLS verification](#upstream-tls-verification)
- [mTLS (client certificates)](#mtls--client-certificate-authentication)

**Middleware**

- [Rhai script middleware](#rhai-script-middleware) / [Full guide](rhai.md) ↗
- [WASM middleware](#wasm-middleware) / [Full guide](wasm.md) ↗

**Advanced**

- [Connection pool](#connection-pool)
- [Multi-site (virtual hosting)](#multi-site)
- [Upload](#upload)
- [Admin API](#admin-api) — see also [admin.md](admin.md) for full reference
- [Prometheus metrics reference](#prometheus-metrics-reference)

---

## Concepts

These sections explain **how Conduit behaves** — not config fields you set,
but background knowledge that helps understand the rest of the reference.

### Request pipeline

The order in which configured features are applied for every incoming request:

```txt
Incoming request
  │
  ├─ 1. X-Request-ID injection        — auto-generate UUID v4 if absent
  ├─ 2. IP filter                     — 403 if blocked (before any auth)
  ├─ 3. CORS preflight                — OPTIONS short-circuit
  ├─ 4. Health / ACME bypass          — skip all guards for /__health__ etc.
  ├─ 5. Inflight limit                — 503 if maxInflightRequests exceeded
  ├─ 6. Site-level rate limit         — 429 if over limit
  ├─ 7. Consumers                     — identify named client (V1/V2/V3)
  ├─ 8. Basic Auth                    — 401 if credentials missing/wrong
  ├─ 9. API Key                       — 401 if key missing/wrong
  ├─ 10. JWT Auth                     — 401 if token invalid
  ├─ 11. Forward Auth                 — delegate to external service
  ├─ 12. Redirects                    — 301/308 if path matches
  ├─ 13. Fault injection              — abort/delay (testing only)
  ├─ 14. Rhai / WASM middleware       — custom scripts in order
  │
  ├─ Route matching + per-route rate limit
  ├─ Circuit breaker check
  │
  └─ Upstream request
       ├─ X-Forwarded-For / -Proto / -Host injection
       ├─ requestTransform (setHeaders / removeHeaders)
       ├─ Path rewrite (stripPrefix / rewrite rules)
       └─ Traffic mirror (fire-and-forget)

Upstream response
  ├─ CRLF header protection
  ├─ CORS / security / custom headers injection
  ├─ responseTransform
  ├─ X-Response-Time header
  ├─ Retry-on-error decision
  └─ Error masking (5xx body replacement)
```

Steps 7–11 are mutually exclusive in practice — only one auth method identifies
the client, but multiple may be configured as fallbacks. The `consumers` guard
runs before `basicAuth` and `apiKey`, so a consumer match takes priority.

### Forwarded headers

Conduit **automatically** injects these headers into every proxied upstream
request. No configuration is needed — they are always present.

| Header              | Value                  | Notes                                                        |
| ------------------- | ---------------------- | ------------------------------------------------------------ |
| `X-Forwarded-For`   | Client IP              | Appended to existing value if already present                |
| `X-Forwarded-Proto` | `http` or `https`      | Derived from whether the site has TLS configured             |
| `X-Forwarded-Host`  | Original `Host` header | Lets upstreams reconstruct full URLs                         |
| `X-Request-ID`      | UUID v4                | Auto-generated if absent; forwarded as-is if client sends it |

To remove any of these before forwarding, use `requestTransform.removeHeaders`:

```yaml
# conduit.yaml
requestTransform:
  removeHeaders: [X-Forwarded-For, X-Forwarded-Host]
```

```json
// conduit.json
{
  "requestTransform": {
    "removeHeaders": ["X-Forwarded-For", "X-Forwarded-Host"]
  }
}
```

### `skipPaths` glob syntax

Many config sections accept a `skipPaths` list — requests whose path matches
are bypassed by that feature entirely. It is **not** a top-level field; it
appears inside `basicAuth`, `apiKey`, `jwtAuth`, `forwardAuth`, `consumers`,
`rateLimit`, and `logging`.

Two pattern forms are supported:

| Pattern       | Matches                                         |
| ------------- | ----------------------------------------------- |
| `/exact/path` | Only that exact path                            |
| `/prefix/**`  | The prefix itself, `/prefix/`, and any sub-path |

```yaml
# conduit.yaml — example inside jwtAuth
jwtAuth:
  secret: "$JWT_SECRET"
  skipPaths:
    - /__health__ # exact match — health check bypasses JWT
    - /public/** # /public, /public/, /public/assets/logo.png, …
```

```json
// conduit.json
{
  "jwtAuth": {
    "secret": "$JWT_SECRET",
    "skipPaths": ["/__health__", "/public/**"]
  }
}
```

> **Note:** only `/**` at the end is supported as a wildcard.
> Patterns like `/api/*/details` or `/**.json` are treated as **exact** matches
> (no mid-path or extension wildcards).

---

## Port and Host

```yaml
# YAML
port: 8080
host: api.example.com # optional — virtual hosting
```

```json
// JSON
{ "port": 8080, "host": "api.example.com" }
```

| Field  | Type   | Default          | Description                                                    |
| ------ | ------ | ---------------- | -------------------------------------------------------------- |
| `port` | number | `80` / `443` ¹   | TCP port to listen on                                          |
| `host` | string | —                | Virtual hostname. Omit to match all `Host` headers (catch-all) |

> ¹ Default port is `443` when `tls` is configured, `80` otherwise.
> When no sites are configured at all, Conduit listens on `8080` as a fallback.

Use `host` when running [multiple sites](#multi-site) on the same process.

---

## TLS / HTTPS

### Manual certificates

```yaml
# YAML
port: 443
tls:
  cert: /etc/tls/server.crt
  key: /etc/tls/server.key
  httpRedirectPort: 80
  versions: ["TLSv1.2", "TLSv1.3"]
```

```json
// JSON
{
  "port": 443,
  "tls": {
    "cert": "/etc/tls/server.crt",
    "key": "/etc/tls/server.key",
    "httpRedirectPort": 80,
    "versions": ["TLSv1.2", "TLSv1.3"]
  }
}
```

### Auto-TLS via Let's Encrypt

> **Requires** `cargo build --features acme`

```yaml
# YAML
port: 443
tls:
  acme:
    email: admin@example.com
    storage: ./certs
    challenge: http-01
```

```json
// JSON
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

### TLS field reference

| Field              | Type     | Default | Description                                                                                                                                   |
| ------------------ | -------- | ------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `cert`             | path     | —       | PEM certificate file                                                                                                                          |
| `key`              | path     | —       | PEM private key file                                                                                                                          |
| `ca`               | path     | —       | CA bundle for upstream verification                                                                                                           |
| `httpRedirectPort` | number   | —       | Port that redirects HTTP to HTTPS                                                                                                             |
| `versions`         | string[] | all     | Allowed TLS versions — rustls format (`"TLSv1.2"`, `"TLSv1.3"`)                                                                               |
| `ciphers`          | string[] | all     | Allowed cipher suites — rustls names, **not** OpenSSL                                                                                         |
| `acme.email`       | string   | —       | Contact email for ACME account                                                                                                                |
| `acme.storage`     | path     | —       | Directory for certificate persistence                                                                                                         |
| `acme.challenge`   | string   | —       | `"http-01"` or `"dns-01"`                                                                                                                     |
| `acme.directory`   | string   | —       | Custom ACME directory URL. Use `"https://acme-staging-v02.api.letsencrypt.org/directory"` for Let's Encrypt staging (rate-limit-free testing) |
| `clientAuth`       | object   | —       | [mTLS client cert verification](#mtls--client-certificate-authentication)                                                                     |

> **Note — single cert per port:** rustls does not support per-SNI certificate
> selection. When multiple HTTPS sites share the same port, the first registered
> cert is used for all. Use separate ports for different certificates.

---

## HTTP/2

```yaml
# YAML
port: 443
tls:
  cert: ./certs/cert.pem
  key: ./certs/key.pem
http2: {} # enable HTTP/2 with defaults
```

```json
// JSON
{
  "port": 443,
  "tls": { "cert": "./certs/cert.pem", "key": "./certs/key.pem" },
  "http2": {}
}
```

Full config with all fields:

```yaml
http2:
  maxConcurrentStreams: 100
  initialWindowSize: 65535
  h2c: false   # HTTP/2 cleartext (internal gRPC without TLS)
```

```json
// JSON
{
  "http2": {
    "maxConcurrentStreams": 100,
    "initialWindowSize": 65535,
    "h2c": false
  }
}
```

| Field                  | Type   | Default | Description                                                                                                                                            |
| ---------------------- | ------ | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `maxConcurrentStreams` | number | `100`   | Max parallel streams per connection                                                                                                                    |
| `initialWindowSize`    | number | `65535` | Flow-control window in bytes                                                                                                                           |
| `h2c`                  | bool   | `false` | Allow HTTP/2 upgrade on **plaintext** connections (h2c). For TLS ports HTTP/2 is negotiated via ALPN regardless. Useful for internal gRPC without TLS. |

---

## Compression

Add `Content-Encoding: br` / `zstd` / `gzip` / `deflate` to responses.

```yaml
# YAML — shorthand (enable with defaults)
compression: true

# YAML — fine-grained
compression:
  algorithms: [br, zstd, gzip]  # Brotli first, then Zstd, then gzip
  level: 6                       # 1 = fastest, 9 = smallest
  minBytes: 1024                 # skip responses smaller than 1 KB
```

```json
// JSON — shorthand
{ "compression": true }

// JSON — fine-grained
{
  "compression": {
    "algorithms": ["br", "zstd", "gzip"],
    "level": 6,
    "minBytes": 1024
  }
}
```

The `types` field filters which response Content-Types are compressed:

```yaml
compression:
  algorithms: [br, gzip]
  types:
    - "text/"
    - "application/json"
    - "application/xml"
    - "application/javascript"
    - "image/svg"
```

| Field        | Type     | Default                                                                                                        | Description                                                                                               |
| ------------ | -------- | -------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| `algorithms` | string[] | `["br", "zstd", "gzip"]`                                                                                       | Compression algorithms to offer. Supported: `"br"` (Brotli), `"zstd"` (Zstandard), `"gzip"`, `"deflate"`  |
| `level`      | number   | `6`                                                                                                            | Compression level (1–9)                                                                                   |
| `minBytes`   | number   | `1024`                                                                                                         | Minimum response size to compress (bytes)                                                                 |
| `types`      | string[] | `["text/", "application/json", "application/xml", "application/javascript", "application/xhtml", "image/svg"]` | Content-Type prefixes to compress. Use `["*"]` to compress all types (not recommended for binary content) |

---

## Response Time Header

Inject `X-Response-Time: <ms>` into every response.

```yaml
# YAML
responseTime: true

# With custom precision
responseTime:
  digits: 3   # decimal places in the millisecond value
```

```json
// JSON
{ "responseTime": true }
```

```json
// JSON
{ "responseTime": { "digits": 3 } }
```

---

## Proxy

The `proxy` object maps URL path prefixes to upstream targets.

### Single upstream (shorthand)

```yaml
# YAML
proxy:
  /api: "http://backend:4000"
```

```json
// JSON
{ "proxy": { "/api": "http://backend:4000" } }
```

### Multiple targets

```yaml
# YAML
proxy:
  /api:
    targets:
      - "http://backend-1:4000"
      - "http://backend-2:4000"
    strategy: round-robin
    stripPrefix: true
```

```json
// JSON
{
  "proxy": {
    "/api": {
      "targets": ["http://backend-1:4000", "http://backend-2:4000"],
      "strategy": "round-robin",
      "stripPrefix": true
    }
  }
}
```

### URL rewriting

Rewrite rules are evaluated in order. The first matching rule is applied.

```yaml
# YAML
proxy:
  /api:
    targets: ["http://backend:4000"]
    stripPrefix: true
    rewrite:
      - from: "^/v[0-9]+/(.+)$" # strip version prefix
        to: "/$1"
      - from: "^/users/([0-9]+)$" # migrate legacy paths
        to: "/members/$1"
```

```json
// JSON
{
  "proxy": {
    "/api": {
      "targets": ["http://backend:4000"],
      "stripPrefix": true,
      "rewrite": [
        { "from": "^/v[0-9]+/(.+)$", "to": "/$1" },
        { "from": "^/users/([0-9]+)$", "to": "/members/$1" }
      ]
    }
  }
}
```

### Proxy route field reference

| Field                 | Type                 | Default       | Description                                                                    |
| --------------------- | -------------------- | ------------- | ------------------------------------------------------------------------------ |
| `targets`             | string[] or object[] | —             | Upstream URLs (plain strings or `{url, weight}`)                               |
| `strategy`            | string               | `round-robin` | Load balancing — see [Load balancing](#load-balancing)                         |
| `stripPrefix`         | bool                 | `false`       | Remove matched path prefix before forwarding                                   |
| `hashKey`             | string               | —             | Key for hash-based strategies: `"ip"`, `"url"`, `"header:X-Name"`              |
| `rewrite`             | object[]             | —             | URL rewrite rules: `[{from, to}]` — first match wins                           |
| `http2`               | bool                 | `false`       | Enable HTTP/2 for upstream connections                                         |
| `timeout.connectMs`   | number               | `3000`        | TCP connect timeout                                                            |
| `timeout.sendMs`      | number               | —             | Request send timeout                                                           |
| `timeout.readMs`      | number               | `30000`       | Response read timeout                                                          |
| `timeout.perTryMs`    | number               | —             | Per-retry timeout                                                              |
| `retry.attempts`      | number               | `0`           | Number of retry attempts (0 = disabled)                                        |
| `retry.conditions`    | string[]             | —             | `connection_error`, `5xx`, `timeout`                                           |
| `retry.backoffMs`     | number               | `0`           | Wait between retries (ms)                                                      |
| `retry.backoffJitter` | bool                 | `false`       | ±50% random spread on `backoffMs` to avoid thundering herd                     |
| `retry.budgetPercent` | number               | —             | Soft cap: max % of in-flight requests that may be retries                      |
| `healthCheck`         | object               | —             | Active health probes — see [Health checks](#health-checks)                     |
| `backup`              | string               | —             | Fallback URL when all primaries are unhealthy                                  |
| `cache`               | object               | —             | Response cache — see [Proxy cache](#proxy-cache)                               |
| `pool`                | object               | —             | Connection pool — see [Connection pool](#connection-pool)                      |
| `rateLimit`           | object               | —             | Per-route rate limit — see [Rate limiting](#rate-limiting)                     |
| `upstreamTls`         | object               | —             | TLS for HTTPS upstreams — see [Upstream TLS](#upstream-tls-verification)       |
| `mirror`              | string               | —             | Shadow URL — see [Traffic mirroring](#traffic-mirroring)                       |
| `sticky.cookie`       | string               | —             | Cookie name for sticky sessions                                                |
| `groups`              | object[]             | —             | Two-level LB groups: `[{name, targets, strategy}]`                             |
| `groupStrategy`       | string               | `round-robin` | Outer strategy when `groups` is set                                            |
| `priority`            | number (0–100)       | `50`          | Request priority for load shedding — see [Priority routing](#priority-routing) |

---

## Routes

The `routes` array matches requests before `proxy` / `static`. First match wins.

```yaml
# YAML
routes:
  # Route to dedicated API v2 server — checked first
  - match:
      path: /api/v2/**
      method: [GET, POST]
      headers:
        X-Version: "2"
    proxy:
      targets: ["http://v2-backend:4000"]

  # Write operations go to write cluster
  - match:
      path: /api/**
      method: [POST, PUT, PATCH, DELETE]
    proxy:
      targets: ["http://write-api:4001", "http://write-api:4002"]
      strategy: least-conn

  # Beta users via cookie → canary backend
  - match:
      cookies:
        beta: "1" # exact: cookie beta=1
        experiment: "blue|green" # regex: blue or green
    proxy:
      targets: ["http://canary:4000"]

  # Everything else → SPA static files
  - match:
      path: /**
    static: ./dist
```

```json
// JSON
{
  "routes": [
    {
      "match": {
        "path": "/api/v2/**",
        "method": ["GET", "POST"],
        "headers": { "X-Version": "2" }
      },
      "proxy": { "targets": ["http://v2-backend:4000"] }
    },
    {
      "match": {
        "path": "/api/**",
        "method": ["POST", "PUT", "PATCH", "DELETE"]
      },
      "proxy": {
        "targets": ["http://write-api:4001", "http://write-api:4002"],
        "strategy": "least-conn"
      }
    },
    {
      "match": { "path": "/**" },
      "static": "./dist"
    }
  ]
}
```

Each route entry has exactly three top-level fields: `match`, `proxy`, and `static`.
Auth, rate limiting, and other policies come from the site-level config and apply
to all routes uniformly.

| `match` field | Type     | Description                                                       |
| ------------- | -------- | ----------------------------------------------------------------- |
| `path`        | glob     | Path glob — see [`skipPaths` glob syntax](#skippaths-glob-syntax) |
| `method`      | string[] | HTTP methods to match (case-insensitive)                          |
| `headers`     | object   | Request headers that must match (exact or regex)                  |
| `query`       | object   | Query parameters that must match (exact or regex)                 |
| `cookies`     | object   | Cookies that must match (exact or regex)                          |

All `headers`, `query`, and `cookies` values are matched as **full-string
regex** (anchored `^…$`). Plain strings like `"v2"` or `"1"` match exactly;
regex patterns like `"blue|green"` or `"Bearer .+"` use regex semantics. An
invalid regex falls back to exact-string comparison.

---

## Load Balancing

Set `strategy` inside a proxy route. All strategies skip upstreams that are
currently unhealthy (failed health probes) or ejected (outlier detection).

```yaml
proxy:
  /api:
    targets: ["http://a:4000", "http://b:4000"]
    strategy: least-conn # pick a strategy
```

---

### `round-robin` (default)

Cycles through the target list in order, one request at a time. A per-route
atomic counter is incremented on each request and taken modulo the number of
healthy upstreams.

```txt
Request 1 → a  Request 2 → b  Request 3 → a  …
```

**Use when:** all upstreams are homogeneous (same hardware, same capacity).
Simple, predictable, zero overhead.

```yaml
proxy:
  /api:
    targets: ["http://a:4000", "http://b:4000", "http://c:4000"]
    strategy: round-robin # or omit — this is the default
```

---

### `weighted-round-robin`

Like round-robin, but each upstream gets a number of slots proportional to its
`weight`. If A has weight 3 and B has weight 1, the pattern is A, A, A, B per
four requests (exact interleaving may differ, but ratios are preserved over
time).

**Use when:** upstreams have different capacities — e.g. a beefy primary and a
smaller standby, or a canary deployment receiving a fraction of traffic.

Targets must be `{ url, weight }` objects — plain strings use weight 1.

```yaml
proxy:
  /api:
    targets:
      - { url: "http://primary:4000", weight: 9 } # 90% of traffic
      - { url: "http://canary:4000", weight: 1 } # 10% canary
    strategy: weighted-round-robin
```

```json
// JSON
{
  "proxy": {
    "/api": {
      "targets": [
        { "url": "http://primary:4000", "weight": 9 },
        { "url": "http://canary:4000", "weight": 1 }
      ],
      "strategy": "weighted-round-robin"
    }
  }
}
```

> **Note:** `conduit upstreams weight` can change weights at runtime without
> reloading the config.

---

### `least-conn`

Routes each new request to the upstream with the fewest **active in-flight
connections** at that instant. The connection counter is incremented before the
request is forwarded and decremented when the response completes (including
retries and errors).

**Use when:** requests have highly variable response times — e.g. a mix of fast
reads and slow writes. Under uniform load it behaves like round-robin; its
advantage emerges when some requests stall.

```yaml
proxy:
  /api:
    targets: ["http://a:4000", "http://b:4000", "http://c:4000"]
    strategy: least-conn
```

> **Tip:** combine with `healthCheck.maxConnectionsPerUpstream` for a
> [circuit breaker](#circuit-breaker) that activates when all upstreams are
> saturated.

---

### `least-response-time`

Routes each request to the upstream with the **lowest measured latency** from
the most recent active health probe. Falls back to round-robin during the
warm-up phase before any probe has completed.

Latency is measured by the health-check prober (HEAD request to `healthCheck.path`).
Upstreams without probe data are ranked last (`u64::MAX`).

**Use when:** upstreams have meaningfully different hardware or geographic
proximity and you want to bias traffic toward the fastest one. Requires
`healthCheck` to be configured on the route — without probes, this strategy
falls back to round-robin permanently.

```yaml
proxy:
  /api:
    targets:
      - "http://us-east:4000"
      - "http://eu-west:4000"
    strategy: least-response-time
    healthCheck:
      path: /health
      intervalSecs: 10
```

```json
// JSON
{
  "proxy": {
    "/api": {
      "targets": ["http://us-east:4000", "http://eu-west:4000"],
      "strategy": "least-response-time",
      "healthCheck": { "path": "/health", "intervalSecs": 10 }
    }
  }
}
```

---

### `ip-hash`

Hashes the **client IP address** (FNV-1a) and maps it to an upstream via
`hash % pool_size`. The same IP always hits the same upstream — as long as the
pool size doesn't change.

**Use when:** you need soft session affinity without a session cookie, e.g.
legacy apps that store state per-IP, or to concentrate logs from one user on
one backend.

> **Caveat:** adding or removing an upstream changes `pool_size` and remaps
> roughly half of all clients. This is `hash % N` (modulo), not a consistent
> hash ring. Use `sticky.cookie` for stable, cookie-based affinity.

```yaml
proxy:
  /auth:
    targets: ["http://auth1:5000", "http://auth2:5000"]
    strategy: ip-hash
    hashKey: ip # default for ip-hash; can be omitted
```

```json
// JSON
{
  "proxy": {
    "/auth": {
      "targets": ["http://auth1:5000", "http://auth2:5000"],
      "strategy": "ip-hash"
    }
  }
}
```

---

### `consistent-hash`

Hashes a configurable **`hashKey`** (IP, URL, or any request header) and maps
it to an upstream via `hash % pool_size`. Identical to `ip-hash` in
implementation — the distinction is purely which value is hashed.

**Use when:** you want to route requests by tenant, user, or any other
request attribute to a dedicated upstream.

The `hashKey` field controls what is hashed:

| `hashKey` value | Hashes                   | Example use case                                   |
| --------------- | ------------------------ | -------------------------------------------------- |
| `ip`            | Client IP                | Per-IP affinity                                    |
| `url`           | Full request URL         | Cache-locality — same URL always hits same backend |
| `header:X-Name` | Value of header `X-Name` | Per-tenant or per-user routing                     |

```yaml
proxy:
  /api:
    targets:
      ["http://shard-1:4000", "http://shard-2:4000", "http://shard-3:4000"]
    strategy: consistent-hash
    hashKey: "header:X-Tenant-ID"
```

```json
// JSON
{
  "proxy": {
    "/api": {
      "targets": [
        "http://shard-1:4000",
        "http://shard-2:4000",
        "http://shard-3:4000"
      ],
      "strategy": "consistent-hash",
      "hashKey": "header:X-Tenant-ID"
    }
  }
}
```

> **Same caveat as ip-hash:** pool size changes remap a large fraction of keys.
> This is `hash % N`, not a Karger consistent hash ring.

---

### `random`

Selects an upstream uniformly at random on each request using a fast
thread-local RNG.

**Use when:** you want an even distribution without any coordination overhead —
useful for very large pools where maintaining a round-robin counter per route
is unnecessary. In practice, `round-robin` is usually preferred since it
provides better uniformity over small sample sizes.

```yaml
proxy:
  /api:
    targets: ["http://a:4000", "http://b:4000", "http://c:4000"]
    strategy: random
```

---

### `p2c` (Power of Two Choices)

On each request, samples **two upstreams at random** and forwards to the one
with fewer active connections. Ties are broken in favour of the first sample.

Achieves O(log log N) maximum load imbalance — dramatically better tail latency
than pure random, and competitive with `least-conn` at scale, with O(1)
selection cost (no scan of the full pool).

With a single upstream in the pool, P2C falls back to round-robin.

**Use when:** the upstream pool is large (10+) and `least-conn` would scan the
entire list on every request. Also useful as a drop-in replacement for
`least-conn` when you want reduced coordination overhead.

```yaml
proxy:
  /api:
    targets:
      - "http://a:4000"
      - "http://b:4000"
      - "http://c:4000"
      - "http://d:4000"
    strategy: p2c
```

```json
// JSON
{
  "proxy": {
    "/api": {
      "targets": [
        "http://a:4000",
        "http://b:4000",
        "http://c:4000",
        "http://d:4000"
      ],
      "strategy": "p2c"
    }
  }
}
```

---

### Strategy comparison

| Strategy               | Session affinity | Handles variable load | Pool change impact | Best for                                         |
| ---------------------- | :--------------: | :-------------------: | :----------------: | ------------------------------------------------ |
| `round-robin`          |        ✗         |           ✗           |        none        | Homogeneous, stateless services                  |
| `weighted-round-robin` |        ✗         |           ✗           |        none        | Mixed-capacity pools, canary rollouts            |
| `least-conn`           |        ✗         |           ✓           |        none        | Variable request duration (mixed workloads)      |
| `least-response-time`  |        ✗         |           ✓           |        none        | Multi-region, geographically dispersed upstreams |
| `ip-hash`              |      by IP       |           ✗           |    remaps ~50%     | Soft affinity without cookies                    |
| `consistent-hash`      |      by key      |           ✗           |    remaps ~50%     | Per-tenant / per-key routing                     |
| `random`               |        ✗         |           ✗           |        none        | Very large pools, simple distribution            |
| `p2c`                  |        ✗         |           ✓           |        none        | Large pools, low-overhead load awareness         |

---

### Sticky sessions

Route a client to the same upstream for the duration of a session cookie,
using consistent hashing on the cookie value.

```yaml
proxy:
  /app:
    targets: ["http://a:4000", "http://b:4000"]
    strategy: consistent-hash
    sticky:
      cookie: session_id
```

```json
// JSON
{
  "proxy": {
    "/app": {
      "targets": ["http://a:4000", "http://b:4000"],
      "strategy": "consistent-hash",
      "sticky": { "cookie": "session_id" }
    }
  }
}
```

If the client presents no cookie (first request), the request is routed by the
configured strategy and the upstream is recorded — no cookie is set by Conduit.
The application is responsible for setting the session cookie.

---

### Upstream groups (two-level balancing)

An outer strategy picks the group; an inner strategy balances within it.
`groups` is an array — each entry has `name`, `targets`, and optional `strategy`.

```yaml
proxy:
  /api:
    groups:
      - name: us-east
        targets: ["http://us-east-1:4000", "http://us-east-2:4000"]
        strategy: least-conn
      - name: eu-west
        targets: ["http://eu-west-1:4000", "http://eu-west-2:4000"]
        strategy: least-conn
    groupStrategy: ip-hash # outer: same client IP always hits same region
```

```json
// JSON
{
  "proxy": {
    "/api": {
      "groups": [
        {
          "name": "us-east",
          "targets": ["http://us-east-1:4000", "http://us-east-2:4000"],
          "strategy": "least-conn"
        },
        {
          "name": "eu-west",
          "targets": ["http://eu-west-1:4000", "http://eu-west-2:4000"],
          "strategy": "least-conn"
        }
      ],
      "groupStrategy": "ip-hash"
    }
  }
}
```

See [`examples/upstream-groups.yaml`](../examples/upstream-groups.yaml)

---

## Static Files

### Simple directory

```yaml
# YAML
static: ./dist
```

```json
// JSON
{ "static": "./dist" }
```

### Multiple directories

```yaml
# YAML
static:
  - ./dist
  - ./public
```

```json
// JSON
{ "static": ["./dist", "./public"] }
```

### Path-mapped directories

```yaml
# YAML
static:
  /: ./dist
  /docs: ./docs-dist
```

```json
// JSON
{ "static": { "/": "./dist", "/docs": "./docs-dist" } }
```

### Static options

```yaml
# YAML
static: ./dist
staticOptions:
  index: [index.html] # default files for directory requests
  dotFiles: ignore # "ignore" | "allow" | "deny"
  preCompressed: true # serve .br / .gz if present
  etag: true # ETag / 304 Not Modified support
  lastModified: true # Last-Modified header
  maxAge: "1y" # Cache-Control max-age (humantime: "1d", "30m", "1y")
```

```json
// JSON
{
  "static": "./dist",
  "staticOptions": {
    "index": ["index.html"],
    "dotFiles": "ignore",
    "preCompressed": true,
    "etag": true,
    "lastModified": true,
    "maxAge": "1y"
  }
}
```

| Field           | Type     | Default          | Description                                          |
| --------------- | -------- | ---------------- | ---------------------------------------------------- |
| `index`         | string[] | `["index.html"]` | Default files for directory requests                 |
| `dotFiles`      | string   | `"ignore"`       | `"ignore"`, `"allow"`, or `"deny"`                   |
| `preCompressed` | bool     | `false`          | Serve pre-compressed `.br` / `.gz` files             |
| `etag`          | bool     | `true`           | ETag + conditional GET support                       |
| `lastModified`  | bool     | `true`           | `Last-Modified` header                               |
| `maxAge`        | string   | —                | `Cache-Control: max-age` — humantime duration string |

---

## Redirects

`redirects` is an **array** of redirect rules.

```yaml
# YAML
redirects:
  - from: /old-path
    to: "https://example.com/new-path"
    status: 301

  - from: "/blog/(.+)" # regex capture group
    to: "https://blog.example.com/$1"
    status: 308
```

```json
// JSON
{
  "redirects": [
    {
      "from": "/old-path",
      "to": "https://example.com/new-path",
      "status": 301
    },
    { "from": "/blog/(.+)", "to": "https://blog.example.com/$1", "status": 308 }
  ]
}
```

| Field    | Type   | Default | Description                                             |
| -------- | ------ | ------- | ------------------------------------------------------- |
| `from`   | string | —       | Path or regex pattern to match                          |
| `to`     | string | —       | Destination URL (capture groups `$1`…`$N` are expanded) |
| `status` | number | `301`   | HTTP redirect status code                               |

---

## Fallback

Return a response when no route matches.

```yaml
# YAML — SPA: serve index.html for all unmatched browser routes
fallback:
  file: ./dist/index.html
  status: 200
```

```yaml
# YAML — content-type-aware fallback (Accept header negotiation)
fallback:
  byAccept:
    html:
      file: ./dist/index.html
      status: 200
    json:
      body: { "error": "Not Found", "status": 404 }
      status: 404
  status: 404
  body: "Not Found"
```

```json
// JSON
{
  "fallback": {
    "file": "./dist/index.html",
    "status": 200
  }
}
```

```json
// JSON
{
  "fallback": {
    "byAccept": {
      "html": { "file": "./dist/index.html", "status": 200 },
      "json": { "body": { "error": "Not Found", "status": 404 }, "status": 404 }
    },
    "status": 404,
    "body": "Not Found"
  }
}
```

| Field      | Type   | Default | Description                                                         |
| ---------- | ------ | ------- | ------------------------------------------------------------------- |
| `file`     | path   | —       | File to serve                                                       |
| `body`     | any    | —       | Response body (string or JSON object)                               |
| `status`   | number | `200`   | HTTP status code                                                    |
| `headers`  | object | —       | Response headers to set                                             |
| `byAccept` | object | —       | Content-type-aware rules keyed by Accept type (`html`, `json`, `*`) |

---

## Health Checks

### Active upstream probes

```yaml
# YAML
proxy:
  /api:
    targets: ["http://a:4000", "http://b:4000"]
    healthCheck:
      path: /health
      intervalSecs: 10
      unhealthyThreshold: 3
      healthyThreshold: 1
      slowStartSecs: 30
      unhealthyStatus: [429, 500, 502, 503, 504] # treat these status codes as failures
      unhealthyLatencyMs: 2000 # treat responses slower than 2s as failures
```

```json
// JSON
{
  "proxy": {
    "/api": {
      "targets": ["http://a:4000", "http://b:4000"],
      "healthCheck": {
        "path": "/health",
        "intervalSecs": 10,
        "unhealthyThreshold": 3,
        "healthyThreshold": 1,
        "slowStartSecs": 30,
        "unhealthyStatus": [429, 500, 502, 503, 504],
        "unhealthyLatencyMs": 2000
      }
    }
  }
}
```

### Site health endpoint (for load balancer probes)

```yaml
# YAML
healthCheck: true

healthCheck:
  path: /__health__
  includeUpstreams: true   # include per-upstream health in JSON response
```

```json
// JSON
{ "healthCheck": { "includeUpstreams": true } }
```

### Health check field reference

| Field                       | Type     | Default       | Description                                                                                                                                          |
| --------------------------- | -------- | ------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| `path`                      | string   | `/__health__` | Probe URL path                                                                                                                                       |
| `intervalSecs`              | number   | `10`          | Probe interval                                                                                                                                       |
| `timeoutMs`                 | number   | `2000`        | Probe timeout                                                                                                                                        |
| `unhealthyThreshold`        | number   | `3`           | Consecutive failures before removal                                                                                                                  |
| `healthyThreshold`          | number   | `1`           | Consecutive passes before re-adding                                                                                                                  |
| `unhealthyStatus`           | number[] | any non-2xx   | HTTP status codes from the health-check probe that count as failures. Default: any non-2xx response. Example: `[429, 500, 502, 503, 504]` |
| `unhealthyLatencyMs`        | number   | —             | Health-check probe responses slower than this (ms) count as failures, even if the status code is 2xx                                                 |
| `slowStartSecs`             | number   | `0`           | Traffic ramp-up period after recovery                                                                                                                |
| `maxConnectionsPerUpstream` | number   | —             | [Circuit breaker](#circuit-breaker) threshold                                                                                                        |
| `prewarmConnections`        | number   | `0`           | Pre-establish N keepalive connections at startup (max 8)                                                                                             |
| `includeUpstreams`          | bool     | `false`       | Include upstream health in `/__health__` response                                                                                                    |

---

## Circuit Breaker

When **all** upstreams reach `maxConnectionsPerUpstream` concurrent connections,
Conduit returns `503` immediately instead of queuing.

```yaml
# YAML
proxy:
  /api:
    targets: ["http://a:4000", "http://b:4000", "http://c:4000"]
    healthCheck:
      maxConnectionsPerUpstream: 100
    backup: "http://replica:4000"
```

```json
// JSON
{
  "proxy": {
    "/api": {
      "targets": ["http://a:4000", "http://b:4000", "http://c:4000"],
      "healthCheck": { "maxConnectionsPerUpstream": 100 },
      "backup": "http://replica:4000"
    }
  }
}
```

See [`examples/circuit-breaker.yaml`](../examples/circuit-breaker.yaml)

---

## Retry

```yaml
# YAML
proxy:
  /api:
    targets: ["http://a:4000", "http://b:4000"]
    retry:
      attempts: 3
      conditions:
        - connection_error
        - "5xx"
        - timeout
      backoffMs: 100
      backoffJitter: true # add ±50% random spread to backoffMs
      budgetPercent: 20
    timeout:
      perTryMs: 2000
```

```json
// JSON
{
  "proxy": {
    "/api": {
      "targets": ["http://a:4000", "http://b:4000"],
      "retry": {
        "attempts": 3,
        "conditions": ["connection_error", "5xx", "timeout"],
        "backoffMs": 100,
        "backoffJitter": true,
        "budgetPercent": 20
      },
      "timeout": { "perTryMs": 2000 }
    }
  }
}
```

### Retry field reference

| Field            | Type     | Default | Description                                                                                                |
| ---------------- | -------- | ------- | ---------------------------------------------------------------------------------------------------------- |
| `attempts`       | number   | `0`     | Number of retry attempts (0 = disabled)                                                                    |
| `conditions`     | string[] | —       | Triggers: `"connection_error"`, `"5xx"`, `"timeout"`. Retries only happen when at least one matches.      |
| `backoffMs`      | number   | `0`     | Fixed wait between attempts (ms). `0` = immediate retry.                                                   |
| `backoffJitter`  | bool     | `false` | Apply ±50% random spread to `backoffMs` to avoid thundering herd. Effective delay: `[ms/2, ms*3/2)`       |
| `budgetPercent`  | number   | —       | Soft cap: at most this fraction of in-flight requests may be retries. Prevents retry storms under mass failure. |

> **Body buffering for retry** — POST/PUT/PATCH requests are only retried when
> `limits.maxBodyBufferBytes` is set. Without it, request bodies are not buffered and
> `connection_error` retries on non-GET methods are skipped silently.

---

## Outlier Detection

Passively eject upstreams that return too many 5xx responses from real traffic.

```yaml
# YAML
outlierDetection:
  consecutive5xx: 5
  baseEjectionTimeSecs: 30
  maxEjectionTimeSecs: 300
  maxEjectionPercent: 33
```

```json
// JSON
{
  "outlierDetection": {
    "consecutive5xx": 5,
    "baseEjectionTimeSecs": 30,
    "maxEjectionTimeSecs": 300,
    "maxEjectionPercent": 33
  }
}
```

Ejection uses exponential backoff: 30 s → 60 s → 120 s → … up to
`maxEjectionTimeSecs`.

**Half-open circuit breaker:** when the ejection period expires, the _first_
request is allowed through as a probe. If the probe succeeds (non-5xx), the
upstream is fully restored and `ejection_count` is reset. If it fails, the
upstream is re-ejected with the next backoff level. All other requests during
the probe are blocked until the probe completes.

| Field                  | Type   | Default | Description                                  |
| ---------------------- | ------ | ------- | -------------------------------------------- |
| `consecutive5xx`       | number | `5`     | Consecutive errors before ejection           |
| `baseEjectionTimeSecs` | number | `30`    | Initial ejection duration                    |
| `maxEjectionTimeSecs`  | number | `300`   | Maximum ejection duration (cap on backoff)   |
| `maxEjectionPercent`   | number | `10`    | Max % of cluster that may be ejected at once |

---

## Limits

```yaml
# YAML
limits:
  maxBodyBytes: 10485760 # reject request bodies over 10 MB (413)
  maxHeaderBytes: 65536 # reject headers over 64 KB
  timeoutSecs: 30 # global request timeout (seconds)
  maxInflightRequests: 1000 # return 503 when 1000 requests are in flight
  maxBodyBufferBytes: 1048576 # buffer up to 1 MB per request for retry replay
  maxConnectionsPerIp: 50 # max simultaneous connections from one IP (429)
  keepaliveRequestLimit: 1000 # recycle connections after this many requests
  priorityThreshold: 0.8 # shed low-priority routes above 80% concurrency
```

```json
// JSON
{
  "limits": {
    "maxBodyBytes": 10485760,
    "maxHeaderBytes": 65536,
    "timeoutSecs": 30,
    "maxInflightRequests": 1000,
    "maxBodyBufferBytes": 1048576,
    "maxConnectionsPerIp": 50,
    "keepaliveRequestLimit": 1000,
    "priorityThreshold": 0.8
  }
}
```

| Field                   | Type   | Default | Description                                                                                                                   |
| ----------------------- | ------ | ------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `maxBodyBytes`          | number | —       | Max request body size — returns `413` if exceeded                                                                             |
| `maxHeaderBytes`        | number | —       | Max request header size                                                                                                       |
| `timeoutSecs`           | number | —       | Global request timeout                                                                                                        |
| `maxInflightRequests`   | number | —       | Max concurrent requests — returns `503` if exceeded (must be ≥ 1)                                                             |
| `maxBodyBufferBytes`    | number | —       | Max body buffered per request for retry replay                                                                                |
| `maxConnectionsPerIp`   | number | —       | Max simultaneous open connections from a single client IP — returns `429` if exceeded                                         |
| `keepaliveRequestLimit` | number | —       | Max requests per keepalive connection; closes and recycles after. Equivalent to nginx's `keepalive_requests`.                 |
| `priorityThreshold`     | number | `0.8`   | Fraction of `maxInflightRequests` at which low-priority routes are shed (0.0–1.0) — see [Priority routing](#priority-routing) |

---

## Priority Routing

Priority routing lets high-value routes continue to be served when the site is
under load, while low-priority routes are shed with `503 Load Shedding`.

### How it works

1. Set `limits.maxInflightRequests` to cap total concurrency.
2. Set `limits.priorityThreshold` (default `0.8`) — the fraction of the cap at
   which shedding begins.
3. Mark routes with `priority: 0–100` (`50` = normal, omitted = normal).
4. When `inflight / maxInflightRequests ≥ priorityThreshold`, any request whose
   effective priority is **below 50** receives `503 Load Shedding`.

Requests with `priority ≥ 50` (normal or high) are never shed by this
mechanism. The `X-Priority: <0–100>` request header can **raise** the
effective priority above the configured route value (useful for trusted
internal callers).

```yaml
# YAML
limits:
  maxInflightRequests: 2000
  priorityThreshold: 0.8 # shed low-priority at 1600+ concurrent

routes:
  - match:
      path: /api/critical/**
    proxy:
      targets: [http://api:4000]
      priority: 90 # always served

  - match:
      path: /api/batch/**
    proxy:
      targets: [http://api:4000]
      priority: 10 # shed first when overloaded

  - match:
      path: /api/**
    proxy:
      targets: [http://api:4000]
      # no priority → defaults to 50 (normal, not shed)
```

```json
{
  "limits": {
    "maxInflightRequests": 2000,
    "priorityThreshold": 0.8
  },
  "routes": [
    {
      "match": { "path": "/api/critical/**" },
      "proxy": { "targets": ["http://api:4000"], "priority": 90 }
    },
    {
      "match": { "path": "/api/batch/**" },
      "proxy": { "targets": ["http://api:4000"], "priority": 10 }
    }
  ]
}
```

| Field                      | Type           | Default | Description                                      |
| -------------------------- | -------------- | ------- | ------------------------------------------------ |
| `limits.priorityThreshold` | number         | `0.8`   | Load fraction at which shedding begins (0.0–1.0) |
| `proxy.*.priority`         | number (0–100) | `50`    | Route priority; below 50 = sheddable             |

> **Note:** Priority routing only applies when both `maxInflightRequests` **and**
> `priorityThreshold` are configured on the site.

---

## Fault Injection

> **Requires** `cargo build --features fault-injection`
>
> **For testing only** — do not use in production.

```yaml
# YAML
faultInjection:
  abort:
    percent: 5
    status: 503
    body: "Injected fault"
  delay:
    percent: 10
    ms: 500
```

```json
// JSON
{
  "faultInjection": {
    "abort": { "percent": 5, "status": 503, "body": "Injected fault" },
    "delay": { "percent": 10, "ms": 500 }
  }
}
```

---

## Proxy Cache

> **Requires** `cargo build --features cache`
> For Redis-backed cache also add `--features redis`; for disk cache add `--features disk-cache`.

```yaml
# YAML
proxy:
  /api:
    targets: ["http://backend:4000"]
    cache:
      store: memory
      ttlSecs: 60
      maxSizeMb: 256 # evict LRU entries after 256 MB
      staleWhileRevalidateSecs: 300 # serve stale up to 5 min while refreshing
      staleIfErrorSecs: 600 # serve stale up to 10 min if upstream fails
      varyHeaders: [Accept-Language, Accept-Encoding]
      skipPaths: [/api/me, /api/cart] # never cache these paths
      skipIfCookie: true
      methods: [GET, HEAD]
```

```json
// JSON
{
  "proxy": {
    "/api": {
      "targets": ["http://backend:4000"],
      "cache": {
        "store": "memory",
        "ttlSecs": 60,
        "maxSizeMb": 256,
        "staleWhileRevalidateSecs": 300,
        "staleIfErrorSecs": 600,
        "varyHeaders": ["Accept-Language", "Accept-Encoding"],
        "skipPaths": ["/api/me", "/api/cart"],
        "skipIfCookie": true,
        "methods": ["GET", "HEAD"]
      }
    }
  }
}
```

### Cache field reference

| Field                      | Type     | Default       | Description                                                                        |
| -------------------------- | -------- | ------------- | ---------------------------------------------------------------------------------- |
| `store`                    | string   | —             | `"memory"` (standard), `"redis://..."` / `"rediss://..."` (`--features redis`), `"disk:/path"` (`--features disk-cache`) |
| `ttlSecs`                  | number   | —             | Fresh cache TTL (seconds)                                                          |
| `maxSizeMb`                | number   | —             | Memory budget; LRU eviction above this                                             |
| `staleWhileRevalidateSecs` | number   | `0`           | Serve stale while refreshing in background (RFC 5861)                              |
| `staleIfErrorSecs`         | number   | `0`           | Serve stale when upstream returns 5xx (RFC 5861)                                   |
| `varyHeaders`              | string[] | —             | Vary cache key by these request headers                                            |
| `skipPaths`                | string[] | —             | Paths to never cache                                                               |
| `skipIfCookie`             | bool     | `false`       | Skip caching when request has a cookie                                             |
| `methods`                  | string[] | `[GET, HEAD]` | Cacheable HTTP methods                                                             |

**Cache key** — `scheme + host + path + query string`. Request body is never
part of the key, so POST responses are not cached by default (add `"POST"` to
`methods` only for idempotent endpoints). Use `varyHeaders` to differentiate
responses by `Accept-Language` or `Accept-Encoding`.

**Stale-while-revalidate** (RFC 5861): the first request after TTL expiry
returns the stale response immediately while a background request refreshes the
cache. Zero latency penalty for users. A built-in cache lock prevents thundering
herd — only one background fetch goes to the upstream at a time.

**`s-maxage` handling**: Conduit respects the upstream `Cache-Control: s-maxage=N` directive
as the effective TTL when the upstream returns it. `s-maxage=0` explicitly prevents
caching regardless of `ttlSecs`. When the upstream returns no `Cache-Control` directive,
`ttlSecs` from the route config is used. `ttlSecs` in the config always caps the
maximum TTL — upstream headers cannot extend it beyond the configured limit.

| Upstream `Cache-Control` | Effect                     |
| ------------------------ | -------------------------- |
| not present              | Use `ttlSecs` from config  |
| `s-maxage=N`             | Use `min(N, ttlSecs)`      |
| `s-maxage=0`             | Do not cache this response |
| `no-store` or `private`  | Do not cache this response |

### Cache store options

**`"memory"`** — in-process cache, lost on restart. Best for single-instance
deployments with small response bodies.

**`"redis://host:port"` / `"rediss://host:port"`** — shared Redis cache.
(`--features redis` required)
All Conduit instances share the same cache — consistent hit rate under
horizontal scaling. `rediss://` enables TLS (AWS ElastiCache, Azure Cache).

```yaml
# conduit.yaml — Redis cache shared across multiple instances
proxy:
  /api:
    targets: ["http://api1:4000", "http://api2:4000"]
    cache:
      store: "redis://redis:6379"
      ttlSecs: 300
      staleWhileRevalidateSecs: 60
      varyHeaders: [Accept-Language]
```

```json
// conduit.json
{
  "proxy": {
    "/api": {
      "targets": ["http://api1:4000", "http://api2:4000"],
      "cache": {
        "store": "redis://redis:6379",
        "ttlSecs": 300,
        "staleWhileRevalidateSecs": 60
      }
    }
  }
}
```

If Redis is unreachable at startup or during a request, caching is silently
disabled for that request — the proxy continues to work normally (fail-open).

**`"disk:/path/to/dir"`** — filesystem cache, survives restarts.
(`--features disk-cache` required)
Useful for large response bodies or when Redis is not available.

```yaml
proxy:
  /assets:
    targets: ["http://assets:4000"]
    cache:
      store: "disk:/var/cache/conduit"
      ttlSecs: 86400 # 1 day
```

```json
{
  "proxy": {
    "/assets": {
      "targets": ["http://assets:4000"],
      "cache": { "store": "disk:/var/cache/conduit", "ttlSecs": 86400 }
    }
  }
}
```

See [`examples/stale-while-revalidate.yaml`](../examples/stale-while-revalidate.yaml)

---

## Basic Auth

```yaml
# YAML
basicAuth:
  users:
    alice: "$ALICE_PASSWORD"
    bob: "$BOB_PASSWORD"
  realm: "My App"
  challenge: true # send WWW-Authenticate header (default: true)
  skipPaths: [/__health__]
```

```json
// JSON
{
  "basicAuth": {
    "users": { "alice": "$ALICE_PASSWORD", "bob": "$BOB_PASSWORD" },
    "realm": "My App",
    "challenge": true,
    "skipPaths": ["/__health__"]
  }
}
```

| Field       | Type     | Default     | Description                                                              |
| ----------- | -------- | ----------- | ------------------------------------------------------------------------ |
| `users`     | object   | —           | `{ username: password }` map                                             |
| `realm`     | string   | `"Conduit"` | Shown in browser login dialog                                            |
| `challenge` | bool     | `true`      | Whether to send `WWW-Authenticate` header                                |
| `skipPaths` | string[] | —           | Paths that bypass Basic Auth — see [glob syntax](#skippaths-glob-syntax) |

---

## API Key

```yaml
# YAML
apiKey:
  keys:
    - "$PRIMARY_API_KEY"
    - "$SECONDARY_API_KEY"
  header: X-API-Key
  skipPaths: [/__health__, /public/**]
```

```json
// JSON
{
  "apiKey": {
    "keys": ["$PRIMARY_API_KEY", "$SECONDARY_API_KEY"],
    "header": "X-API-Key",
    "skipPaths": ["/__health__", "/public/**"]
  }
}
```

The key may be sent in the configured `header` or as a `?api_key=` query
parameter.

---

## JWT Auth

> **Requires** `cargo build --features jwt`

Validates `Authorization: Bearer <token>` on every request.

### JWKS endpoint (recommended for production)

```yaml
# YAML
jwtAuth:
  jwksUrl: "https://YOUR_DOMAIN.auth0.com/.well-known/jwks.json"
  audience: ["https://api.example.com"]
  issuer: "https://YOUR_DOMAIN.auth0.com"
  jwksRefreshSecs: 3600
  skipPaths: [/__health__, /public/**]
```

```json
// JSON
{
  "jwtAuth": {
    "jwksUrl": "https://YOUR_DOMAIN.auth0.com/.well-known/jwks.json",
    "audience": ["https://api.example.com"],
    "issuer": "https://YOUR_DOMAIN.auth0.com",
    "jwksRefreshSecs": 3600,
    "skipPaths": ["/__health__", "/public/**"]
  }
}
```

### Shared secret (HS256)

```yaml
jwtAuth:
  secret: "$JWT_SECRET"
  skipPaths: [/__health__]
```

```json
// JSON
{ "jwtAuth": { "secret": "$JWT_SECRET", "skipPaths": ["/__health__"] } }
```

### JWT field reference

| Field             | Type     | Default | Description                                             |
| ----------------- | -------- | ------- | ------------------------------------------------------- |
| `secret`          | string   | —       | HS256 shared secret (mutually exclusive with `jwksUrl`) |
| `jwksUrl`         | string   | —       | JWKS endpoint URL (RS256 / ES256)                       |
| `jwksRefreshSecs` | number   | `3600`  | JWKS key refresh interval                               |
| `audience`        | string[] | —       | Required `aud` claims                                   |
| `issuer`          | string   | —       | Required `iss` claim                                    |
| `skipPaths`       | string[] | —       | Paths that bypass JWT validation                        |

### Injecting JWT claims as upstream headers

```yaml
requestTransform:
  setHeaders:
    X-User-ID: "{{ jwt.sub }}"
    X-User-Email: "{{ jwt.email }}"
    X-Tenant: "{{ jwt.tid }}"
  removeHeaders:
    - Authorization
```

```json
// JSON
{
  "requestTransform": {
    "setHeaders": {
      "X-User-ID": "{{ jwt.sub }}",
      "X-User-Email": "{{ jwt.email }}"
    },
    "removeHeaders": ["Authorization"]
  }
}
```

Unknown claims expand to empty string. See [Request / Response Transform](#request--response-transform).

See [`examples/jwt-auth.yaml`](../examples/jwt-auth.yaml)

---

## Forward Auth

> **Requires** `cargo build --features forward-auth`

Delegate authentication to an external HTTP service.

```txt

Client -> Conduit -> Auth service
2xx -> copy responseHeaders, forward to upstream
4xx -> return to client, stop
fail -> 401 (fail closed)

```

```yaml
# YAML
forwardAuth:
  url: "http://auth-service:9000/verify"
  requestHeaders: [Authorization, Cookie]
  responseHeaders: [X-User-ID, X-Role]
  timeoutMs: 3000
  skipPaths: [/__health__, /public/**]
```

```json
// JSON
{
  "forwardAuth": {
    "url": "http://auth-service:9000/verify",
    "requestHeaders": ["Authorization", "Cookie"],
    "responseHeaders": ["X-User-ID", "X-Role"],
    "timeoutMs": 3000,
    "skipPaths": ["/__health__", "/public/**"]
  }
}
```

| Field             | Type     | Default | Description                                           |
| ----------------- | -------- | ------- | ----------------------------------------------------- |
| `url`             | string   | —       | Auth service URL (required)                           |
| `requestHeaders`  | string[] | —       | Client headers to forward to auth service             |
| `responseHeaders` | string[] | —       | Auth response headers to inject into upstream request |
| `timeoutMs`       | number   | `5000`  | Auth service timeout                                  |
| `skipPaths`       | string[] | —       | Paths that bypass forward auth                        |

The auth service receives `X-Forwarded-Method`, `X-Forwarded-Uri`,
`X-Forwarded-For`, plus any `requestHeaders`.

See [`examples/forward-auth.yaml`](../examples/forward-auth.yaml)

---

## Consumers

> **Requires** `cargo build --features consumers`
> JWT consumers (V2 / V3) additionally require `--features jwt`.

Named API clients with per-consumer credentials, rate limits, and headers.
After identification, the consumer's username is injected as `X-Consumer-ID`.
Unidentified requests receive `401`.

```yaml
# YAML — V1 (API key / Basic Auth) + V2 (per-consumer JWT)
consumers:
  idHeader: "X-Consumer-ID"
  apiKeyHeader: "X-API-Key"
  skipPaths: [/__health__]

  consumers:
    # V1: API key
    - username: alice
      apiKey: "$ALICE_KEY"
      rateLimit: { windowSecs: 60, limit: 100 }
      headers: { X-Tier: free }

    # V1: Basic Auth (username from consumer.username, password from basicAuth.password)
    - username: billing-service
      basicAuth: { password: "$BILLING_PASSWORD" }
      headers: { X-Internal: "true" }

    # V2: JWT with HS256 secret — token must be signed with this secret
    - username: mobile-app
      jwt:
        secret: "$MOBILE_JWT_SECRET"
        issuer: "https://auth.internal"
      rateLimit: { windowSecs: 60, limit: 500 }

    # V2: JWT with JWKS — token validated against public keys from this endpoint
    - username: partner-app
      jwt:
        jwksUrl: "https://partner.example.com/.well-known/jwks.json"
        audience: ["my-api"]
```

```json
// JSON
{
  "consumers": {
    "idHeader": "X-Consumer-ID",
    "skipPaths": ["/__health__"],
    "consumers": [
      {
        "username": "alice",
        "apiKey": "$ALICE_KEY",
        "rateLimit": { "windowSecs": 60, "limit": 100 },
        "headers": { "X-Tier": "free" }
      },
      {
        "username": "billing-service",
        "basicAuth": { "password": "$BILLING_PASSWORD" },
        "headers": { "X-Internal": "true" }
      },
      {
        "username": "mobile-app",
        "jwt": { "secret": "$MOBILE_JWT_SECRET" },
        "rateLimit": { "windowSecs": 60, "limit": 500 }
      },
      {
        "username": "partner-app",
        "jwt": {
          "jwksUrl": "https://partner.example.com/.well-known/jwks.json",
          "audience": ["my-api"]
        }
      }
    ]
  }
}
```

### V3: Shared JWT (Auth0 / Cognito / Keycloak pattern)

One JWKS endpoint for all consumers; consumers are identified by `sub` claim.

```yaml
# YAML
consumers:
  sharedJwt:
    jwksUrl: "https://YOUR_DOMAIN.auth0.com/.well-known/jwks.json"
    audience: ["https://api.example.com"]
    issuer: "https://YOUR_DOMAIN.auth0.com"
    usernameClaim: "sub" # default

  consumers:
    - username: "auth0|alice123"
      rateLimit: { windowSecs: 60, limit: 100 }
      headers: { X-Tier: free }

    - username: "auth0|bob456"
      rateLimit: { windowSecs: 60, limit: 10000 }
      headers: { X-Tier: premium }
```

```json
// JSON
{
  "consumers": {
    "sharedJwt": {
      "jwksUrl": "https://YOUR_DOMAIN.auth0.com/.well-known/jwks.json",
      "audience": ["https://api.example.com"],
      "issuer": "https://YOUR_DOMAIN.auth0.com",
      "usernameClaim": "sub"
    },
    "consumers": [
      {
        "username": "auth0|alice123",
        "rateLimit": { "windowSecs": 60, "limit": 100 }
      },
      {
        "username": "auth0|bob456",
        "rateLimit": { "windowSecs": 60, "limit": 10000 }
      }
    ]
  }
}
```

### `ConsumersConfig` field reference

| Field          | Type     | Default         | Description                                          |
| -------------- | -------- | --------------- | ---------------------------------------------------- |
| `consumers`    | object[] | —               | List of named consumers                              |
| `idHeader`     | string   | `x-consumer-id` | Header injected into upstream with consumer username |
| `apiKeyHeader` | string   | `x-api-key`     | Header to read API keys from                         |
| `skipPaths`    | string[] | —               | Paths that bypass consumer auth                      |
| `sharedJwt`    | object   | —               | Single JWKS for all consumers, identified by `sub` claim — Auth0/Cognito/Keycloak pattern (`--features jwt` required) |

### Per-consumer fields

| Field       | Type   | Description                                                 |
| ----------- | ------ | ----------------------------------------------------------- |
| `username`  | string | Required — unique name, injected as `X-Consumer-ID`         |
| `apiKey`    | string | API key credential                                          |
| `basicAuth` | object | `{ password }` — username is taken from `consumer.username` |
| `jwt`       | object | `{ secret? jwksUrl? audience? issuer? }` — per-consumer JWT identification (`--features jwt` required) |
| `rateLimit` | object | Per-consumer rate limit (global, not per-IP)                |
| `headers`   | object | Additional headers to inject into upstream request          |

See [`examples/consumers.yaml`](../examples/consumers.yaml)

---

## Rate Limiting

In-memory rate limiting is part of the standard build.
`store: "redis://..."` requires `--features redis`.

### Site-level

Applied to all requests before authentication.

```yaml
# YAML
rateLimit:
  windowSecs: 60
  limit: 1000
  keyBy: ip
  store: memory # or "redis://host:port" for multi-instance
  skipPaths: [/__health__]
```

```json
// JSON
{
  "rateLimit": {
    "windowSecs": 60,
    "limit": 1000,
    "keyBy": "ip",
    "store": "memory",
    "skipPaths": ["/__health__"]
  }
}
```

### Per-route

Applied after routing, independently of the site-level limit.

```yaml
proxy:
  /api/payments:
    targets: ["http://payments:4000"]
    rateLimit:
      windowSecs: 60
      limit: 10
      keyBy: "header:X-User-ID"
```

```json
// JSON
{
  "proxy": {
    "/api/payments": {
      "targets": ["http://payments:4000"],
      "rateLimit": {
        "windowSecs": 60,
        "limit": 10,
        "keyBy": "header:X-User-ID"
      }
    }
  }
}
```

### Rate limit field reference

| Field        | Type     | Default    | Description                                                                                                 |
| ------------ | -------- | ---------- | ----------------------------------------------------------------------------------------------------------- |
| `windowSecs` | number   | —          | Sliding window duration (seconds) — **required**                                                            |
| `limit`      | number   | —          | Max requests per key per window — **required**                                                              |
| `burst`      | number   | `0`        | Extra burst capacity above `limit` (see below)                                                              |
| `keyBy`      | string   | `"ip"`     | `"ip"` or `"header:<name>"`                                                                                 |
| `store`      | string   | `"memory"` | `"memory"` or `"redis://host:port"` (`--features redis` required for Redis)                                 |
| `skipPaths`  | string[] | —          | Paths that bypass rate limiting — see [skipPaths glob syntax](#skippaths-glob-syntax)                       |
| `dryRun`     | bool     | `false`    | Log rate-limit violations without actually rejecting requests — useful for tuning limits before enforcement |

The rate limiter uses a **token-bucket** algorithm. Tokens refill at
`limit / windowSecs` per second. Without `burst`, the bucket holds `limit` tokens.

**Burst capacity** (`burst: N`): the bucket starts with `limit + N` tokens,
allowing short spikes above the sustained rate. The refill rate stays at
`limit / windowSecs` — burst is absorbed and not refilled.

```yaml
# Allow up to 80 requests in a burst, sustained at 1 req/s
rateLimit:
  windowSecs: 60
  limit: 60
  burst: 20
```

---

## Request / Response Transform

### Static header transforms

```yaml
# YAML
requestTransform:
  setHeaders:
    X-User-ID: "{{ jwt.sub }}"
    X-Gateway: conduit
  removeHeaders:
    - Authorization

responseTransform:
  setHeaders:
    X-Served-By: conduit
  removeHeaders:
    - Server
    - X-Powered-By
    - X-AspNet-Version
```

```json
// JSON
{
  "requestTransform": {
    "setHeaders": { "X-User-ID": "{{ jwt.sub }}", "X-Gateway": "conduit" },
    "removeHeaders": ["Authorization"]
  },
  "responseTransform": {
    "setHeaders": { "X-Served-By": "conduit" },
    "removeHeaders": ["Server", "X-Powered-By", "X-AspNet-Version"]
  }
}
```

### JWT claim templates

> **Requires** `cargo build --features jwt` — templates expand to `""` when JWT auth is not active.

Available in `requestTransform.setHeaders` after JWT validation:

| Template          | Claim   | Notes                                               |
| ----------------- | ------- | --------------------------------------------------- |
| `{{ jwt.sub }}`   | `sub`   | User identifier — always present                    |
| `{{ jwt.email }}` | `email` | Email claim (if IdP includes it)                    |
| `{{ jwt.iss }}`   | `iss`   | Token issuer                                        |
| any claim         | any     | `{{ jwt.<claim> }}` — unknown claims expand to `""` |

---

## Traffic Mirroring

Send a copy of requests to a shadow backend. The shadow response is discarded
— clients only see the primary response.

```yaml
proxy:
  /api:
    targets: ["http://api-v1:4000"]
    mirror: "http://api-v2:4000"
```

```json
// JSON
{
  "proxy": {
    "/api": {
      "targets": ["http://api-v1:4000"],
      "mirror": "http://api-v2:4000"
    }
  }
}
```

The mirrored request includes all original headers plus `X-Mirrored-From: <host>`.
Mirror failures do not affect clients.

> **Note:** request body is not mirrored — only headers are forwarded to the shadow
> backend. This is sufficient for observability and shadow testing of read workloads.

---

## Custom Response Headers

Inject headers into every response site-wide. These are in addition to any
headers set by `responseTransform`.

```yaml
# YAML
headers:
  X-Environment: production
  X-API-Version: "3"
```

```json
// JSON
{
  "headers": {
    "X-Environment": "production",
    "X-API-Version": "3"
  }
}
```

---

## Logging

```yaml
# YAML — shorthand
logging: dev         # colorized, short — for development
logging: json        # structured JSON — for production
logging: combined    # Apache Combined Log Format
```

```json
// JSON
{ "logging": "json" }
```

Full config:

```yaml
logging:
  format: json
  file: ./logs/access.log
  stripQuery: true # omit query string from logged path (e.g. /search?q=... → /search)
  skipPaths:
    - /__health__
    - /__metrics__
    - /favicon.ico
```

```json
// JSON
{
  "logging": {
    "format": "json",
    "file": "./logs/access.log",
    "stripQuery": true,
    "skipPaths": ["/__health__", "/__metrics__", "/favicon.ico"]
  }
}
```

| Field        | Type     | Default | Description                                                                         |
| ------------ | -------- | ------- | ----------------------------------------------------------------------------------- |
| `format`     | string   | `dev`   | Log format — see table below                                                        |
| `file`       | string   | —       | Append access logs to this file path (in addition to stdout)                        |
| `stripQuery` | bool     | `false` | Remove query string from the logged path. Useful when queries contain PII or tokens |
| `skipPaths`  | string[] | —       | Glob patterns — requests matching these paths are not logged                        |

| Format     | Description                                  |
| ---------- | -------------------------------------------- |
| `dev`      | Colorized, short — development               |
| `combined` | Apache Combined Log Format                   |
| `common`   | Apache Common Log Format                     |
| `short`    | Short, no timestamps                         |
| `json`     | Structured JSON — Loki, Datadog, Splunk, ELK |

### JSON log fields

```json
// JSON
{
  "time": "2026-01-15T14:23:01Z",
  "method": "GET",
  "path": "/api/users",
  "status": 200,
  "bytes": 1234,
  "duration_ms": 42,
  "upstream_ms": 38,
  "ip": "10.0.0.1",
  "request_id": "a1b2c3d4-e5f6-...",
  "upstream": "http://api-1:4000"
}
```

| Field         | Description                                                                                   |
| ------------- | --------------------------------------------------------------------------------------------- |
| `duration_ms` | Total request time from accept to response sent (ms)                                          |
| `upstream_ms` | Time spent waiting for the upstream response (ms). Absent for local handlers (health, static) |
| `request_id`  | Value of `X-Request-ID` — auto-generated UUID v4 if absent                                    |
| `upstream`    | Selected upstream URL                                                                         |

---

## Metrics

```yaml
# YAML
metrics:
  path: /__metrics__
  token: "$METRICS_TOKEN"
```

```json
// JSON
{ "metrics": { "path": "/__metrics__", "token": "$METRICS_TOKEN" } }
```

Prometheus scrape config:

```yaml
scrape_configs:
  - job_name: conduit
    static_configs: [{ targets: ["conduit-host:8080"] }]
    metrics_path: /__metrics__
    bearer_token: "my-token"
```

### Per-upstream metrics

In addition to the site-level metrics, Conduit exposes per-upstream URL metrics:

| Metric                                | Type      | Labels               | Description                                                   |
| ------------------------------------- | --------- | -------------------- | ------------------------------------------------------------- |
| `conduit_upstream_requests_total`     | counter   | `upstream`, `status` | Requests forwarded to each upstream (including retries)       |
| `conduit_upstream_latency_seconds`    | histogram | `upstream`           | Upstream response latency (request sent → response received)  |
| `conduit_upstream_active_connections` | gauge     | `upstream`           | In-flight requests currently being processed by each upstream |

These complement `conduit_upstream_errors_total{route}` for diagnosing which
specific backend is slow or returning errors.

---

## OpenTelemetry Tracing

> **Requires** `cargo build --features otlp`

```yaml
# YAML
global:
  otlp:
    endpoint: "http://tempo:4317"
    serviceName: "my-api"
    sampleRate: 0.1
    timeoutMs: 5000
```

```json
// JSON
{
  "global": {
    "otlp": {
      "endpoint": "http://tempo:4317",
      "serviceName": "my-api",
      "sampleRate": 0.1,
      "timeoutMs": 5000
    }
  }
}
```

Each span: `method`, `path`, `status`, `duration_ms`, `upstream_url`, `request_id`.
5xx responses set span status to `ERROR`.

| Field         | Type   | Default | Description                             |
| ------------- | ------ | ------- | --------------------------------------- |
| `endpoint`    | string | —       | gRPC OTLP endpoint URL                  |
| `serviceName` | string | —       | `service.name` in all spans             |
| `sampleRate`  | number | `1.0`   | Fraction of requests to trace (0.0–1.0) |
| `timeoutMs`   | number | `5000`  | Export timeout                          |

See [`examples/observability.yaml`](../examples/observability.yaml)

---

## Hot Reload

Watch the config file for changes and reload without restarting.

```yaml
# YAML
hotReload: true

hotReload:
  extensions: [html, css, js, ts, jsx, tsx]   # file types that trigger browser reload
```

```json
// JSON
{ "hotReload": true }
```

```json
// JSON
{ "hotReload": { "extensions": ["html", "css", "js"] } }
```

**Hot-reloadable** (no restart): `proxy`, `static`, `routes`, `rateLimit`,
`basicAuth`, `apiKey`, `jwtAuth`, `forwardAuth`, `consumers`, `middleware`,
`logging`, `cors`, `securityHeaders`, `cache`, `outlierDetection`, `limits`,
`requestTransform`, `responseTransform`, `maskErrors`.

**Requires cold restart:** `port`, `tls.cert/key`, `tls.versions/ciphers`,
`workers`, `backlog`, `global.admin.bind`.

---

## Security Headers

### `securityHeaders: true` — safe defaults

```yaml
securityHeaders: true
```

Sets these four headers on every response:

| HTTP header              | Value                             |
| ------------------------ | --------------------------------- |
| `X-Content-Type-Options` | `nosniff`                         |
| `X-Frame-Options`        | `SAMEORIGIN`                      |
| `Referrer-Policy`        | `strict-origin-when-cross-origin` |
| `X-XSS-Protection`       | `1; mode=block`                   |

### Custom security headers

```yaml
securityHeaders:
  hstsMaxAgeSecs: 63072000 # → Strict-Transport-Security: max-age=63072000; includeSubDomains
  csp: "default-src 'self'; script-src 'self'"
  xFrameOptions: DENY
  referrerPolicy: "strict-origin-when-cross-origin"
```

```json
// JSON
{
  "securityHeaders": {
    "hstsMaxAgeSecs": 63072000,
    "csp": "default-src 'self'",
    "xFrameOptions": "DENY",
    "referrerPolicy": "strict-origin-when-cross-origin"
  }
}
```

Full example with all fields:

```yaml
securityHeaders:
  hstsMaxAgeSecs: 63072000
  hstsIncludeSubDomains: true # add includeSubDomains to HSTS header
  hstsPreload: true # add preload to HSTS (see hstspreload.org)
  csp: "default-src 'self'"
  xFrameOptions: DENY
  referrerPolicy: "no-referrer"
  permissionsPolicy: "geolocation=(), microphone=()"
  allowedHosts: # reject Host headers not in this list (→ 421)
    - "example.com"
    - "www.example.com"
```

| Field                   | Type     | Default (object form)             | Sets HTTP header / action                                                            |
| ----------------------- | -------- | --------------------------------- | ------------------------------------------------------------------------------------ |
| `hstsMaxAgeSecs`        | number   | — (not set)                       | `Strict-Transport-Security: max-age=<N>`                                             |
| `hstsIncludeSubDomains` | bool     | `true` when hstsMaxAgeSecs is set | Append `; includeSubDomains` to HSTS header                                          |
| `hstsPreload`           | bool     | `false`                           | Append `; preload` to HSTS header (see [hstspreload.org](https://hstspreload.org))   |
| `csp`                   | string   | — (not set)                       | `Content-Security-Policy`                                                            |
| `xFrameOptions`         | string   | `SAMEORIGIN`                      | `X-Frame-Options`                                                                    |
| `referrerPolicy`        | string   | `strict-origin-when-cross-origin` | `Referrer-Policy`                                                                    |
| `permissionsPolicy`     | string   | — (not set)                       | `Permissions-Policy` — restrict browser feature access                               |
| `allowedHosts`          | string[] | — (not set)                       | Reject requests with a `Host` header not in this list with `421 Misdirected Request` |

> **Always set:** `X-Content-Type-Options: nosniff` and `X-XSS-Protection: 1; mode=block`
> are added in both `true` and object forms — they cannot be disabled.
>
> **HSTS and CSP** are only set when explicitly configured.
> HSTS should only be used on HTTPS sites — omit it for HTTP-only configs.
>
> **Permissions-Policy** restricts access to browser APIs (geolocation, camera, microphone, etc.).
> See the [Permissions Policy spec](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Permissions-Policy).
>
> **`allowedHosts`** prevents host-header injection attacks where an attacker sends a request with
> a forged `Host` header to bypass routing or cache-keying logic.

---

## CORS

```yaml
# YAML — open CORS (development only)
cors: true

# Locked to specific origins (production)
cors:
  origins: ["https://app.example.com", "https://admin.example.com"]
  credentials: true
  methods: [GET, POST, PUT, DELETE, OPTIONS]
  allowedHeaders: [Authorization, Content-Type, X-Request-ID]
  maxAgeSecs: 86400
```

```json
// JSON
{
  "cors": {
    "origins": ["https://app.example.com"],
    "credentials": true,
    "methods": ["GET", "POST", "PUT", "DELETE", "OPTIONS"],
    "allowedHeaders": ["Authorization", "Content-Type"],
    "maxAgeSecs": 86400
  }
}
```

| Field            | Type     | Default | Description                                  |
| ---------------- | -------- | ------- | -------------------------------------------- |
| `origins`        | string[] | `["*"]` | Allowed origins                              |
| `methods`        | string[] | all     | Allowed methods                              |
| `allowedHeaders` | string[] | all     | Allowed request headers                      |
| `credentials`    | bool     | `false` | Allow `Authorization` / cookies cross-origin |
| `maxAgeSecs`     | number   | —       | `Access-Control-Max-Age` (preflight cache)   |

`cors: true` allows any origin (`*`). Always use the object form in production.

---

## IP Filter

Allow or deny requests by client IP or CIDR range. Evaluated **before**
authentication — blocked IPs get `403` immediately.

```yaml
# YAML — allowlist (deny all others)
ipFilter:
  allow:
    - "10.0.0.0/8"
    - "172.16.0.0/12"
    - "203.0.113.0/24"
  trustProxy: true   # trust X-Forwarded-For for client IP detection

# Denylist
ipFilter:
  deny:
    - "192.0.2.0/24"

# Dry-run mode — log violations without blocking
ipFilter:
  deny:
    - "192.0.2.0/24"
  dryRun: true
```

```json
// JSON
{
  "ipFilter": {
    "allow": ["10.0.0.0/8", "172.16.0.0/12"],
    "trustProxy": true
  }
}
```

| Field        | Type     | Default | Description                                                                               |
| ------------ | -------- | ------- | ----------------------------------------------------------------------------------------- |
| `allow`      | string[] | —       | Allowed CIDRs — deny all others                                                           |
| `deny`       | string[] | —       | Denied CIDRs — allow all others                                                           |
| `trustProxy` | bool     | `false` | Trust `X-Forwarded-For` for client IP                                                     |
| `dryRun`     | bool     | `false` | Log blocks without enforcing them — useful for auditing a new deny list before going live |

When both `allow` and `deny` are set, `allow` takes precedence.

> **Dynamic CIDR management:** use the Admin API to add or remove deny entries at runtime
> without a configuration reload — see [Admin API — IP deny list](#admin-api).

---

## Error Masking

Replace upstream `5xx` bodies with a generic JSON error.

```yaml
maskErrors: true
```

```json
// JSON
{ "maskErrors": true }
```

Clients receive: `{ "error": "Internal Server Error", "status": 500 }`

---

## Upstream TLS Verification

```yaml
proxy:
  /api:
    targets: ["https://api-internal:8443"]
    upstreamTls:
      verify: true
      serverName: api-internal.svc.cluster.local
```

```json
// JSON
{
  "proxy": {
    "/api": {
      "targets": ["https://api-internal:8443"],
      "upstreamTls": {
        "verify": true,
        "serverName": "api-internal.svc.cluster.local"
      }
    }
  }
}
```

| Field        | Type   | Default  | Description                                  |
| ------------ | ------ | -------- | -------------------------------------------- |
| `verify`     | bool   | `false`  | Verify upstream cert against system CA store |
| `serverName` | string | from URL | Override SNI hostname                        |

---

## mTLS — Client Certificate Authentication

Require clients to present a TLS certificate signed by a trusted CA.

```yaml
tls:
  cert: /etc/tls/server.crt
  key: /etc/tls/server.key
  clientAuth:
    ca: /etc/tls/client-ca.crt
    optional: false # true = allow connections without cert
```

```json
// JSON
{
  "tls": {
    "cert": "/etc/tls/server.crt",
    "key": "/etc/tls/server.key",
    "clientAuth": { "ca": "/etc/tls/client-ca.crt", "optional": false }
  }
}
```

| Field      | Type | Default | Description                                                   |
| ---------- | ---- | ------- | ------------------------------------------------------------- |
| `ca`       | path | —       | CA PEM file that signs authorized client certs — **required** |
| `optional` | bool | `false` | `false` = reject without cert; `true` = allow without cert    |

See [`examples/mtls.yaml`](../examples/mtls.yaml)

---

## Rhai Script Middleware

> **Requires** `cargo build --features rhai`

Execute custom Rhai scripts per request. Scripts run in order; any script can
reject the request or read headers to make decisions.

**→ Full guide with examples: [rhai.md](rhai.md)**

```yaml
# YAML
middleware:
  - type: script
    path: ./scripts/auth-check.rhai

  - type: script
    path: ./scripts/add-headers.rhai
    config:
      tier: premium
```

```json
// JSON
{
  "middleware": [
    { "type": "script", "path": "./scripts/auth-check.rhai" },
    {
      "type": "script",
      "path": "./scripts/add-headers.rhai",
      "config": { "tier": "premium" }
    }
  ]
}
```

> **Note:** inline scripts are not supported — use `path` to a `.rhai` file.
> Optional `config` is passed to the script as a JSON value.

**Available Rhai functions:**

| Function                             | Description                          |
| ------------------------------------ | ------------------------------------ |
| `request.header(name)`               | Read a request header                |
| `request.set_header(name, value)`    | Set a request header                 |
| `request.remove_header(name)`        | Remove a request header              |
| `request.uri()`                      | Get the request URI                  |
| `request.method()`                   | Get the HTTP method                  |
| `request.set_response(status, body)` | Short-circuit with a custom response |

---

## WASM Middleware

> **Requires** `cargo build --features wasm`

**→ Full guide with ABI reference, Rust examples, and build instructions: [wasm.md](wasm.md)**

```yaml
# YAML
middleware:
  - type: wasm
    path: ./plugins/my-plugin.wasm
```

```json
// JSON
{ "middleware": [{ "type": "wasm", "path": "./plugins/my-plugin.wasm" }] }
```

Plugins export `on_request() -> i32` and a `memory` export.
Return `0` to continue, non-zero to reject. Conduit **fails open** on errors.

**Host functions:**

| Function                      | Description                          |
| ----------------------------- | ------------------------------------ |
| `conduit_get_header`          | Read a request header                |
| `conduit_set_header`          | Set a request header                 |
| `conduit_remove_header`       | Remove a request header              |
| `conduit_get_uri`             | Get request URI                      |
| `conduit_get_method`          | Get HTTP method                      |
| `conduit_get_header_names`    | List all header names                |
| `conduit_set_response`        | Short-circuit with a custom response |
| `conduit_abort_with_redirect` | Redirect the client                  |
| `conduit_get_request_id`      | Get X-Request-ID                     |
| `conduit_log`                 | Write to Conduit log                 |

---

## Connection Pool

Configure the upstream HTTP connection pool per route.

```yaml
proxy:
  /api:
    targets: ["http://backend:4000"]
    pool:
      maxIdle: 100 # max idle connections to keep open
      idleTimeoutSecs: 90 # close idle connections after 90 s
```

```json
// JSON
{
  "proxy": {
    "/api": {
      "targets": ["http://backend:4000"],
      "pool": { "maxIdle": 100, "idleTimeoutSecs": 90 }
    }
  }
}
```

| Field             | Type   | Default | Description                                    |
| ----------------- | ------ | ------- | ---------------------------------------------- |
| `maxIdle`         | number | —       | Max idle connections kept alive                |
| `idleTimeoutSecs` | number | —       | Close idle connections after this many seconds |

---

## Multi-Site

Run multiple virtual hosts from one process.

```yaml
# YAML
global:
  workers: 4
  backlog: 1024
  shutdownTimeoutSecs: 30
  admin:
    bind: "127.0.0.1:2019"
    token: "$ADMIN_TOKEN"

sites:
  - port: 443
    host: app.example.com
    tls: { cert: ./certs/app.crt, key: ./certs/app.key }
    jwtAuth: { jwksUrl: "https://auth.example.com/.well-known/jwks.json" }
    proxy:
      /api: "http://app-backend:4000"

  - port: 443
    host: admin.example.com
    tls: { cert: ./certs/admin.crt, key: ./certs/admin.key }
    basicAuth: { users: { admin: "$ADMIN_PASS" } }
    proxy:
      /: "http://admin-ui:3000"

  - port: 8080
    static: ./public
    fallback: { file: ./public/404.html, status: 404 }
```

```json
// JSON
{
  "global": {
    "workers": 4,
    "shutdownTimeoutSecs": 30,
    "admin": { "bind": "127.0.0.1:2019", "token": "$ADMIN_TOKEN" }
  },
  "sites": [
    {
      "port": 443,
      "host": "app.example.com",
      "tls": { "cert": "./certs/app.crt", "key": "./certs/app.key" },
      "proxy": { "/api": "http://app-backend:4000" }
    }
  ]
}
```

### Global field reference

| Field                 | Type   | Default         | Description                                                                          |
| --------------------- | ------ | --------------- | ------------------------------------------------------------------------------------ |
| `workers`             | number | CPU count       | Worker threads — cold restart to change                                              |
| `backlog`             | number | `1024`          | TCP accept backlog                                                                   |
| `shutdownTimeoutSecs` | number | —               | Grace period for in-flight requests on shutdown                                      |
| `admin.bind`          | string | — (not started) | Admin API address. **Required to enable the Admin API.** Omit to disable it entirely |
| `admin.token`         | string | —               | Bearer token required for every Admin API request (strongly recommended)             |
| `otlp`                | object | —               | OpenTelemetry tracing config (`--features otlp` required — see [OpenTelemetry Tracing](#opentelemetry-tracing)) |

---

## TCP Proxy

> **Requires** `cargo build --features tcp`

Forward raw TCP connections without HTTP parsing. Useful for MySQL, PostgreSQL,
Redis, SMTP, and any other non-HTTP protocol.

```yaml
# conduit.yaml
sites:
  - port: 3306
    tcp:
      targets:
        - "mysql-primary:3306"
        - "mysql-replica:3306"
      strategy: round-robin # or "random" (default: round-robin)
      connectTimeoutMs: 5000 # upstream connect timeout (default: 5000)
```

```json
// conduit.json
{
  "sites": [
    {
      "port": 3306,
      "tcp": {
        "targets": ["mysql-primary:3306", "mysql-replica:3306"],
        "strategy": "round-robin",
        "connectTimeoutMs": 5000
      }
    }
  ]
}
```

| Field              | Type     | Default       | Description                                   |
| ------------------ | -------- | ------------- | --------------------------------------------- |
| `targets`          | string[] | —             | Upstream `host:port` addresses — **required** |
| `strategy`         | string   | `round-robin` | `"round-robin"` or `"random"`                 |
| `connectTimeoutMs` | number   | `5000`        | Upstream connect timeout (ms)                 |

> **Note:** `tcp` cannot be combined with `proxy`, `static`, or other HTTP
> features on the same site. Use a separate port/site for HTTP traffic.

---

## Upload

> **Requires** `cargo build --features upload`

Enable multipart file upload. The upload handler is only started when this
section is present in the config.

```yaml
# YAML
upload:
  path: /upload # URL path for the upload endpoint (required)
  dir: ./uploads # directory where files are saved (required)
  maxFileSizeBytes: 52428800 # 50 MB per file
  maxTotalSizeBytes: 209715200 # 200 MB total per request
  maxFiles: 10
  allowedMimeTypes: ["image/jpeg", "image/png", "application/pdf"]
  fieldName: file # multipart field name (default: "file")
```

```json
// JSON
{
  "upload": {
    "path": "/upload",
    "dir": "./uploads",
    "maxFileSizeBytes": 52428800,
    "maxTotalSizeBytes": 209715200,
    "maxFiles": 10,
    "allowedMimeTypes": ["image/jpeg", "image/png", "application/pdf"],
    "fieldName": "file"
  }
}
```

| Field               | Type     | Default  | Description                                     |
| ------------------- | -------- | -------- | ----------------------------------------------- |
| `path`              | string   | —        | URL path for upload endpoint — **required**     |
| `dir`               | string   | —        | Directory to save uploaded files — **required** |
| `maxFileSizeBytes`  | number   | —        | Max size per individual file                    |
| `maxTotalSizeBytes` | number   | —        | Max total size of all files in one request      |
| `maxFiles`          | number   | —        | Max number of files per request                 |
| `allowedMimeTypes`  | string[] | all      | Allowed MIME types (e.g. `"image/jpeg"`)        |
| `fieldName`         | string   | `"file"` | Multipart field name to read                    |

---

## Admin API

Configure with [`global.admin`](#multi-site):

```yaml
global:
  admin:
    bind: "127.0.0.1:2019" # loopback only
    token: "$ADMIN_TOKEN" # optional Bearer token
```

The Admin API provides 10 endpoints: hot-reload, status, upstream management,
cache purge, and runtime IP deny-list.

**→ Full reference with request/response examples: [docs/admin.md](admin.md)**

---

## Prometheus Metrics Reference

All metrics are at the [`metrics.path`](#metrics) endpoint.

| Metric                                | Type      | Labels               | Description                                                      |
| ------------------------------------- | --------- | -------------------- | ---------------------------------------------------------------- |
| `conduit_requests_total`              | counter   | `method`, `status`   | Total HTTP requests handled                                      |
| `conduit_request_duration_seconds`    | histogram | `method`, `status`   | Full request latency (accept → response sent)                    |
| `conduit_active_connections`          | gauge     | —                    | Requests currently in-flight (site-wide)                         |
| `conduit_upstream_errors_total`       | counter   | `route`, `status`    | Upstream 5xx responses per route                                 |
| `conduit_upstream_requests_total`     | counter   | `upstream`, `status` | Requests forwarded to each upstream URL                          |
| `conduit_upstream_latency_seconds`    | histogram | `upstream`           | Upstream response latency per URL                                |
| `conduit_upstream_active_connections` | gauge     | `upstream`           | In-flight requests per upstream URL                              |
| `conduit_retry_attempts_total`        | counter   | `route`, `condition` | Retry attempts by trigger (`5xx`, `connection_error`, `timeout`) |
| `conduit_rate_limit_rejected_total`   | counter   | `site`               | Rate-limited (429) requests per site                             |
| `conduit_cache_hits_total`            | counter   | `route`              | Proxy cache hits                                                 |
| `conduit_cache_misses_total`          | counter   | `route`              | Proxy cache misses                                               |

**Example Grafana queries:**

```promql
# Request rate
rate(conduit_requests_total[5m])

# p99 latency
histogram_quantile(0.99, rate(conduit_request_duration_seconds_bucket[5m]))

# Error rate
rate(conduit_upstream_errors_total[5m])

# Cache hit ratio
rate(conduit_cache_hits_total[5m])
  / (rate(conduit_cache_hits_total[5m]) + rate(conduit_cache_misses_total[5m]))
```
