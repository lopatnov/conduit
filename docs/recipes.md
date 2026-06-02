# Configuration Recipes

Ready-to-use configs for common scenarios. Each recipe shows the **YAML** version
with inline comments. Where a JSON equivalent exists in [`examples/`](../examples/),
a link appears after the code block.

For the full field reference see [configuration.md](configuration.md).

---

## Table of Contents

- [Getting started](#getting-started)
  - [Minimal — static files + proxy](#minimal--static-files--proxy)
  - [Local dev server](#local-dev-server)
  - [SPA + API (production)](#spa--api-production)
- [HTTPS](#https)
  - [Manual certificates](#manual-certificates)
  - [Auto-TLS via Let's Encrypt](#auto-tls-via-lets-encrypt)
  - [mTLS — require client certificates](#mtls--require-client-certificates)
- [Authentication](#authentication)
  - [JWT with JWKS (Auth0 / Cognito / Google)](#jwt-with-jwks-auth0--cognito--google)
  - [API key with multiple keys](#api-key-with-multiple-keys)
  - [Named consumer tiers](#named-consumer-tiers)
  - [External auth service (Forward Auth)](#external-auth-service-forward-auth)
- [Load balancing](#load-balancing)
  - [Weighted round-robin](#weighted-round-robin)
  - [Least connections with health checks](#least-connections-with-health-checks)
  - [Active/passive failover](#activepassive-failover)
  - [Geo-regional routing (upstream groups)](#geo-regional-routing-upstream-groups)
- [Reliability](#reliability)
  - [Circuit breaker + retry budget](#circuit-breaker--retry-budget)
  - [Response caching with stale-while-revalidate](#response-caching-with-stale-while-revalidate)
- [File Upload](#file-upload)
- [TCP Proxy](#tcp-proxy)
- [API gateway](#api-gateway)
  - [Microservices gateway](#microservices-gateway)
  - [JWT gateway with per-route rate limits](#jwt-gateway-with-per-route-rate-limits)
- [Security hardening](#security-hardening)
- [Observability](#observability)
- [Multi-site virtual hosting](#multi-site-virtual-hosting)
- [Ready to deploy?](#ready-to-deploy)

---

## Getting started

### Minimal — static files + proxy

The smallest useful configuration.

```yaml
# examples/minimal.yaml
port: 3000
static: ./dist
proxy:
  /api: "http://localhost:4000"
```

```
GET /            → serves ./dist/index.html
GET /styles.css  → serves ./dist/styles.css
GET /api/users   → proxied to http://localhost:4000/api/users
```

→ JSON: [`examples/minimal.json`](../examples/minimal.json)

---

### Local dev server

Browser hot reload, open CORS, colorized logs, SPA fallback.

```yaml
# examples/dev-hot-reload.yaml
port: 3000
logging: dev
cors: true
hotReload:
  extensions: [html, css, js, ts, jsx, tsx, json]
static: ./src
proxy:
  /api: "http://localhost:4000"
fallback:
  file: ./src/index.html
  status: 200
```

→ JSON: [`examples/dev-hot-reload.json`](../examples/dev-hot-reload.json)

---

### SPA + API (production)

Pre-compressed assets, least-conn balancing, API cache, content-aware fallback.

```yaml
# examples/spa-with-api.yaml
port: 443
tls:
  cert: /etc/tls/cert.pem
  key: /etc/tls/key.pem
  httpRedirectPort: 80     # redirect port 80 → 443 automatically

http2: true
securityHeaders: true
compression: true

cors:
  origins: ["https://app.example.com"]
  credentials: true

logging:
  format: json
  file: /var/log/conduit/access.log
  skipPaths: [/__health__, /__metrics__]

static: ./dist
staticOptions:
  preCompressed: true      # serve .br/.gz sidecars when available
  maxAge: "1y"             # long-lived Cache-Control for hashed assets

proxy:
  /api:
    targets:
      - "http://api1:4000"
      - "http://api2:4000"
    strategy: least-conn
    stripPrefix: true
    retry:
      attempts: 3
      conditions: [connection_error, "5xx"]
    healthCheck:
      path: /health
      intervalSecs: 10
    cache:
      store: memory
      ttlSecs: 60
      skipIfCookie: true   # don't cache authenticated responses

rateLimit:
  windowSecs: 60
  limit: 300
  skipPaths: [/__health__]

healthCheck: true
metrics:
  path: /__metrics__
  token: "$METRICS_TOKEN"

fallback:
  byAccept:
    html: { file: ./dist/index.html, status: 200 }  # SPA
    json: { body: { error: "Not Found", status: 404 }, status: 404 }
```

→ JSON: [`examples/spa-with-api.json`](../examples/spa-with-api.json)

---

## HTTPS

### Manual certificates

TLS with your own certificate files — from Let's Encrypt CLI, Certbot, or a CA.

```yaml
# examples/tls-h2.yaml
port: 443
tls:
  cert: /etc/tls/fullchain.pem
  key: /etc/tls/privkey.pem
  httpRedirectPort: 80      # redirect port 80 → 443 automatically
  versions: ["TLSv1.2", "TLSv1.3"]
http2: true
securityHeaders: true
proxy:
  /: "http://localhost:4000"
```

→ JSON: [`examples/tls-h2.json`](../examples/tls-h2.json)

---

### Auto-TLS via Let's Encrypt

> **Requires** `cargo build --features acme`

Conduit obtains and renews certificates automatically.
The domain must point to this server and port 80 must be reachable for the HTTP-01 challenge.

```yaml
# examples/tls-acme.yaml
port: 443
tls:
  acme:
    email: admin@example.com
    storage: /var/cache/conduit/certs
    challenge: http-01
    # Use staging for testing — no rate limits:
    # directory: "https://acme-staging-v02.api.letsencrypt.org/directory"
  httpRedirectPort: 80
http2: true
securityHeaders: true
proxy:
  /: "http://localhost:4000"
```

→ JSON: [`examples/tls-acme.json`](../examples/tls-acme.json)

---

### mTLS — require client certificates

Every client must present a certificate signed by your CA. Useful for
service-to-service auth, B2B APIs, and IoT devices.

```yaml
# examples/mtls.yaml
port: 443
tls:
  cert: /etc/tls/server.crt
  key: /etc/tls/server.key
  clientAuth:
    ca: /etc/tls/client-ca.crt  # CA that signs authorized client certs
    optional: false              # reject connections without a valid cert
proxy:
  /api:
    targets: ["http://backend:4000"]
    stripPrefix: true
```

→ JSON: [`examples/mtls.json`](../examples/mtls.json) — includes certificate generation commands.

---

## Authentication

### JWT with JWKS (Auth0 / Cognito / Google)

> **Requires** `cargo build --features jwt`

Validates `Authorization: Bearer <token>` on every request. Inject validated
claims as upstream headers so backends don't need to re-validate.

```yaml
# examples/jwt-auth.yaml
port: 8080

jwtAuth:
  jwksUrl: "https://YOUR_DOMAIN.auth0.com/.well-known/jwks.json"
  audience: ["https://api.example.com"]
  issuer: "https://YOUR_DOMAIN.auth0.com"
  skipPaths: [/__health__]

# Forward identity to the backend — it trusts these headers, not raw JWT
requestTransform:
  setHeaders:
    X-User-ID: "{{ jwt.sub }}"
    X-User-Email: "{{ jwt.email }}"
  removeHeaders: [Authorization]

proxy:
  /api: "http://backend:4000"
healthCheck: true
```

→ JSON: [`examples/jwt-auth.json`](../examples/jwt-auth.json)

---

### API key with multiple keys

Rotate keys without downtime by keeping both old and new active.

```yaml
port: 8080

apiKey:
  keys:
    - "$API_KEY_V2"   # new key
    - "$API_KEY_V1"   # old key — remove after all clients migrate
  header: X-API-Key
  skipPaths: [/__health__, /public/**]

proxy:
  /api: "http://backend:4000"
```

---

### Named consumer tiers

> **Requires** `cargo build --features consumers`

Each API client gets its own credentials, rate limit, and upstream headers.
Useful for developer portals and partner APIs.

```yaml
# examples/consumers.yaml
port: 8080

consumers:
  idHeader: X-Consumer-ID
  skipPaths: [/__health__]
  consumers:
    - username: free-tier-client
      apiKey: "$FREE_KEY"
      rateLimit: { windowSecs: 60, limit: 60 }
      headers: { X-Tier: free }

    - username: premium-client
      apiKey: "$PREMIUM_KEY"
      rateLimit: { windowSecs: 60, limit: 6000 }
      headers: { X-Tier: premium, X-SLA: "99.9" }

    - username: internal-service
      basicAuth: { password: "$INTERNAL_PASSWORD" }
      headers: { X-Internal: "true" }

proxy:
  /api: "http://backend:4000"
```

→ JSON: [`examples/consumers.json`](../examples/consumers.json)

---

### External auth service (Forward Auth)

> **Requires** `cargo build --features forward-auth`

Delegate every auth decision to an existing service (oauth2-proxy, Ory
Oathkeeper, custom SSO middleware). The auth service's response headers
(`X-User-ID`, `X-Role`, …) are forwarded to the upstream.

```yaml
# examples/forward-auth.yaml
port: 8080

forwardAuth:
  url: "http://auth-service:9000/verify"
  requestHeaders: [Authorization, Cookie]    # pass these to the auth service
  responseHeaders: [X-User-ID, X-Role, X-Tenant]  # inject these into upstream
  timeoutMs: 3000
  skipPaths: [/__health__, /login, /public/**]

proxy:
  /api: "http://backend:4000"
```

→ JSON: [`examples/forward-auth.json`](../examples/forward-auth.json)

---

## Load balancing

### Weighted round-robin

Send more traffic to powerful instances, or gradually shift traffic during a
canary deployment.

```yaml
# examples/load-balanced.yaml (excerpt)
proxy:
  /api:
    targets:
      - { url: "http://main:4000",   weight: 9 }   # 90%
      - { url: "http://canary:4000", weight: 1 }   # 10% canary
    strategy: weighted-round-robin
```

> Adjust weights at runtime without a reload:
> `conduit upstreams weight --route /api --target http://canary:4000 --weight 2`

→ JSON: [`examples/load-balanced.json`](../examples/load-balanced.json)

---

### Least connections with health checks

Routes each request to the backend with the fewest active connections.
Removes unhealthy backends automatically; ramps traffic back slowly after recovery.

```yaml
# examples/load-balanced.yaml (excerpt)
proxy:
  /api:
    targets:
      - "http://api1:4000"
      - "http://api2:4000"
      - "http://api3:4000"
    strategy: least-conn
    healthCheck:
      path: /health
      intervalSecs: 10
      unhealthyThreshold: 2
      slowStartSecs: 30   # ramp recovered upstream over 30 s
```

→ JSON: [`examples/load-balanced.json`](../examples/load-balanced.json)

---

### Active/passive failover

Primary cluster handles all traffic; backup receives traffic only when all
primaries are unhealthy.

```yaml
proxy:
  /api:
    targets:
      - "http://primary1:4000"
      - "http://primary2:4000"
    strategy: round-robin
    healthCheck:
      path: /health
      intervalSecs: 10
    backup: "http://dr-site:4000"   # activated only when all primaries are down
    retry:
      attempts: 2
      conditions: [connection_error, "5xx"]
```

---

### Geo-regional routing (upstream groups)

Outer strategy (`ip-hash`) pins each client to a region; inner strategy
(`least-conn`) balances within the region.

```yaml
# examples/upstream-groups.yaml
proxy:
  /api:
    groups:
      - name: us-east
        targets: ["http://us-east-1:4000", "http://us-east-2:4000"]
        strategy: least-conn
      - name: eu-west
        targets: ["http://eu-west-1:4000", "http://eu-west-2:4000"]
        strategy: least-conn
    groupStrategy: ip-hash   # same client IP always hits the same region
```

→ JSON: [`examples/upstream-groups.json`](../examples/upstream-groups.json)

---

## Reliability

### Circuit breaker + retry budget

`503` when all upstreams are saturated. Retry storms limited to 20% of traffic.
Outlier detection passively ejects backends that return too many 5xx responses.

```yaml
# examples/circuit-breaker.yaml
proxy:
  /api:
    targets: ["http://a:4000", "http://b:4000", "http://c:4000"]
    strategy: least-conn
    healthCheck:
      path: /health
      intervalSecs: 10
      maxConnectionsPerUpstream: 100   # circuit breaker: 503 when all hit 100 conn
    backup: "http://replica:4000"      # cold standby activated by circuit breaker
    retry:
      attempts: 3
      conditions: [connection_error, "5xx", timeout]
      backoffMs: 100
      backoffJitter: true              # ±50% spread to avoid thundering herd
      budgetPercent: 20                # at most 20% of active requests are retries
    timeout:
      connectMs: 500
      readMs: 10000
      perTryMs: 3000

outlierDetection:
  consecutive5xx: 5          # eject after 5 consecutive errors
  baseEjectionTimeSecs: 30   # first ejection: 30 s
  maxEjectionTimeSecs: 300   # cap at 5 min with exponential backoff
  maxEjectionPercent: 33     # never eject more than 1/3 of the cluster

maskErrors: true             # replace 5xx bodies with generic JSON
```

→ JSON: [`examples/circuit-breaker.json`](../examples/circuit-breaker.json)

---

### Response caching with stale-while-revalidate

> **Requires** `cargo build --features cache`

Zero-latency cache expiry: stale content is served immediately while a
background request fetches fresh data.

```yaml
# examples/stale-while-revalidate.yaml
proxy:
  /api:
    targets: ["http://backend:4000"]
    stripPrefix: true
    cache:
      store: memory
      ttlSecs: 60
      staleWhileRevalidateSecs: 300  # serve stale for up to 5 min while refreshing
      staleIfErrorSecs: 600          # serve stale if upstream is down
      varyHeaders: [Accept-Language] # separate cache entries per language
      skipIfCookie: true             # don't cache authenticated sessions
      skipPaths: [/api/me, /api/cart]
```

For shared cache across multiple Conduit instances, use `store: "redis://host:6379"`
(`--features redis` required).

→ JSON: [`examples/stale-while-revalidate.json`](../examples/stale-while-revalidate.json)

---

## File Upload

> **Requires** `cargo build --features upload`

### Accept multipart file uploads

```yaml
# examples/file-upload.yaml
port: 8080

upload:
  path: /files              # POST /files/<anything> → handled by upload server
  dir: ./uploads            # destination directory (created if absent)
  fieldName: file           # multipart field name (default: "file")
  maxFileSizeBytes: 10485760   # 10 MB per file
  maxTotalSizeBytes: 20971520  # 20 MB per request
  maxFiles: 5
  allowedMimeTypes:
    - "image/jpeg"
    - "image/png"
    - "application/pdf"

proxy:
  targets: ["http://api:4000"]   # remaining traffic goes to backend
```

**Upload with curl:**

```bash
curl -X POST http://localhost:8080/files/my-doc \
  -F "file=@document.pdf;type=application/pdf"
# → {"status":"ok","files":[{"name":"<uuid>.pdf","originalName":"document.pdf","size":204800}]}
```

Files are saved with UUID v4 names (preserving extension) to prevent path
traversal and name collisions.

→ JSON: [`examples/file-upload.json`](../examples/file-upload.json)

---

## TCP Proxy

> **Requires** `cargo build --features tcp`

Forward raw TCP connections without HTTP parsing. Useful for databases (MySQL,
PostgreSQL, Redis), SMTP, and other non-HTTP protocols.

```yaml
# conduit.yaml — TCP passthrough to PostgreSQL
port: 5432
tcp:
  targets:
    - "db-primary:5432"
    - "db-replica:5432"
  strategy: round-robin
  connectTimeoutMs: 3000
```

Multiple TCP sites from one process:

```yaml
global:
  admin:
    bind: "127.0.0.1:2019"

sites:
  # MySQL proxy
  - port: 3306
    tcp:
      targets: ["mysql-1:3306", "mysql-2:3306"]
      strategy: round-robin

  # Redis proxy
  - port: 6380
    tcp:
      targets: ["redis-primary:6379"]
      strategy: round-robin
```

> TCP sites are separate from HTTP sites — they cannot share a port. Health
> checks and TLS termination are not available in TCP mode.

---

## API gateway

### Microservices gateway

Route traffic to individual services by path. One place for rate limiting,
IP filtering, auth, and metrics.

```yaml
# examples/api-gateway.yaml
port: 8080
logging: json

ipFilter:
  allow: ["10.0.0.0/8", "172.16.0.0/12"]

rateLimit:
  windowSecs: 60
  limit: 500

proxy:
  /users:   "http://users-svc:4001"
  /orders:  "http://orders-svc:4002"
  /catalog:
    targets: ["http://catalog1:4003", "http://catalog2:4003"]
    strategy: round-robin
    cache:
      store: memory
      ttlSecs: 300
  /payments:
    targets: ["https://payment-svc:8443"]
    upstreamTls: { verify: true }
    rateLimit: { windowSecs: 60, limit: 20, keyBy: "header:X-User-ID" }

healthCheck: true
metrics:
  path: /__metrics__
maskErrors: true
```

→ JSON: [`examples/api-gateway.json`](../examples/api-gateway.json)

---

### JWT gateway with per-route rate limits

> **Requires** `cargo build --features jwt`

Validate JWT once at the gateway. Inject user identity as headers.
Strict per-route limits on expensive endpoints.

```yaml
port: 443
tls:
  cert: /etc/tls/cert.pem
  key: /etc/tls/key.pem
  httpRedirectPort: 80

jwtAuth:
  jwksUrl: "https://auth.example.com/.well-known/jwks.json"
  audience: ["api.example.com"]
  skipPaths: [/__health__]

requestTransform:
  setHeaders:
    X-User-ID: "{{ jwt.sub }}"
    X-User-Role: "{{ jwt.role }}"
  removeHeaders: [Authorization]

responseTransform:
  removeHeaders: [Server, X-Powered-By]

proxy:
  /v1/users:
    targets: ["http://users:4001", "http://users:4002"]
    strategy: least-conn
    stripPrefix: true
    rateLimit: { windowSecs: 60, limit: 200, keyBy: "header:X-User-ID" }

  /v1/payments:
    targets: ["http://payments:4002"]
    stripPrefix: true
    rateLimit: { windowSecs: 60, limit: 10, keyBy: "header:X-User-ID" }

  /v1/search:
    targets: ["http://search:4003"]
    stripPrefix: true
    cache: { store: memory, ttlSecs: 30 }

healthCheck: true
maskErrors: true
logging:
  format: json
  skipPaths: [/__health__]
```

---

## Security hardening

Defence-in-depth: TLS hardening, security headers, CORS, IP allowlist, rate
limit, API key, error masking, admin token, upstream TLS verification.

```yaml
# examples/security-hardened.yaml
global:
  admin:
    bind: "127.0.0.1:2019"
    token: "$ADMIN_TOKEN"   # always protect the admin API

sites:
  - port: 443
    host: secure.example.com

    tls:
      cert: /etc/tls/server.crt
      key: /etc/tls/server.key
      httpRedirectPort: 80
      versions: ["TLSv1.2", "TLSv1.3"]   # disable TLS 1.0/1.1

    securityHeaders:
      hstsMaxAgeSecs: 63072000             # 2 years
      hstsIncludeSubDomains: true
      csp: "default-src 'self'"
      xFrameOptions: DENY
      referrerPolicy: "strict-origin-when-cross-origin"
      permissionsPolicy: "geolocation=(), microphone=()"
      allowedHosts: ["secure.example.com"]  # reject forged Host headers

    cors:
      origins: ["https://app.example.com"]
      credentials: true
      allowedHeaders: [Authorization, Content-Type]

    ipFilter:
      allow: ["10.0.0.0/8", "172.16.0.0/12"]

    rateLimit:
      windowSecs: 60
      limit: 200
      burst: 50                # allow short bursts above the sustained rate

    apiKey:
      keys: ["$API_KEY_PRIMARY", "$API_KEY_SECONDARY"]
      skipPaths: [/__health__]

    maskErrors: true           # never send upstream stack traces to clients

    proxy:
      /api:
        targets: ["https://api-internal:8443"]
        stripPrefix: true
        upstreamTls:
          verify: true
          serverName: api-internal.svc.cluster.local

    healthCheck: true
    metrics:
      path: /__metrics__
      token: "$METRICS_TOKEN"
```

→ JSON: [`examples/security-hardened.json`](../examples/security-hardened.json)

---

## Observability

> OTLP tracing requires `cargo build --features otlp`

Prometheus metrics, OTLP tracing (Grafana Tempo / Jaeger), structured JSON
logs with upstream timing, and passive outlier detection.

```yaml
# examples/observability.yaml
global:
  otlp:
    endpoint: "http://tempo:4317"
    serviceName: "my-service"
    sampleRate: 0.1      # 10% sampling in production

  admin:
    bind: "127.0.0.1:2019"

sites:
  - port: 8080
    logging:
      format: json
      file: ./logs/access.log
      skipPaths: [/__health__, /__metrics__]
      # upstream_ms field in JSON log shows how long the upstream took

    metrics:
      path: /__metrics__
      token: "$METRICS_TOKEN"

    healthCheck:
      includeUpstreams: true   # /__health__ returns upstream latency + ejection status

    outlierDetection:
      consecutive5xx: 5
      baseEjectionTimeSecs: 30
      maxEjectionTimeSecs: 300
      maxEjectionPercent: 10

    securityHeaders: true

    proxy:
      /api:
        targets: ["http://api1:4000", "http://api2:4000"]
        strategy: least-conn
        stripPrefix: true
        healthCheck:
          path: /health
          intervalSecs: 10
```

**Prometheus scrape config:**

```yaml
scrape_configs:
  - job_name: conduit
    static_configs: [{ targets: ["conduit-host:8080"] }]
    metrics_path: /__metrics__
    bearer_token: "$METRICS_TOKEN"
```

**Grafana queries:**

```promql
# Request rate
rate(conduit_requests_total[5m])

# p99 latency
histogram_quantile(0.99, rate(conduit_request_duration_seconds_bucket[5m]))

# Per-upstream error rate
rate(conduit_upstream_errors_total[5m])

# Cache hit ratio
rate(conduit_cache_hits_total[5m])
  / (rate(conduit_cache_hits_total[5m]) + rate(conduit_cache_misses_total[5m]))
```

→ JSON: [`examples/observability.json`](../examples/observability.json)

---

## Multi-site virtual hosting

> Uses `jwtAuth` — requires `cargo build --features jwt`
> Uses `tls.acme` — requires `cargo build --features acme`

Three virtual hosts from one process — each with its own auth, TLS, and backends.

```yaml
# examples/multi-site.yaml
global:
  workers: 4
  admin:
    bind: "127.0.0.1:2019"
    token: "$ADMIN_TOKEN"

sites:
  # Public app — JWT auth, auto-TLS
  - port: 443
    host: app.example.com
    tls:
      acme:
        email: admin@example.com
        storage: /var/cache/conduit/certs
        challenge: http-01
    jwtAuth:
      jwksUrl: "https://auth.example.com/.well-known/jwks.json"
      skipPaths: [/__health__]
    proxy:
      /api: "http://app-backend:4000"
    static: ./dist
    fallback: { file: ./dist/index.html, status: 200 }

  # Admin panel — Basic Auth, internal network only
  - port: 443
    host: admin.example.com
    tls:
      cert: /etc/tls/admin.crt
      key: /etc/tls/admin.key
    ipFilter:
      allow: ["10.0.0.0/8"]
    basicAuth:
      users: { admin: "$ADMIN_PASSWORD" }
    proxy:
      /: "http://admin-ui:3000"

  # Internal metrics — plain HTTP, loopback only
  - port: 9090
    host: 127.0.0.1
    metrics:
      path: /metrics
    healthCheck: true
```

→ JSON: [`examples/multi-site.json`](../examples/multi-site.json)

---

## Ready to deploy?

Config written — now run it in production.

**[→ Deployment Guide](deployment.md)** — Docker, systemd, Kubernetes, production
checklist, secrets management.
