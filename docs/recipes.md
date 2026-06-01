# Configuration Recipes

Ready-to-use configs for common scenarios. Each recipe shows **YAML** (with
comments) and the equivalent **JSON** side by side.

Every recipe links to a file in [`examples/`](../examples/) that you can run
directly with `conduit -c examples/<name>.yaml`. The examples include inline
comments explaining every option — start there if you want to explore.

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
- [API gateway](#api-gateway)
  - [Microservices gateway](#microservices-gateway)
  - [JWT gateway with per-route rate limits](#jwt-gateway-with-per-route-rate-limits)
- [Security hardening](#security-hardening)
- [Observability](#observability)
- [Multi-site virtual hosting](#multi-site-virtual-hosting)

---

## Getting started

### Minimal — static files + proxy

The smallest useful configuration.

```yaml
# conduit.yaml
port: 3000
static: ./dist
proxy:
  /api: "http://localhost:4000"
```

```json
// conduit.json
{
  "port": 3000,
  "static": "./dist",
  "proxy": { "/api": "http://localhost:4000" }
}
```

```
GET /            → serves ./dist/index.html
GET /styles.css  → serves ./dist/styles.css
GET /api/users   → proxied to http://localhost:4000/api/users
```

→ [`examples/minimal.yaml`](../examples/minimal.yaml) / [`minimal.json`](../examples/minimal.json)

---

### Local dev server

Browser hot reload, open CORS, colorized logs, SPA fallback.

```yaml
# conduit.yaml
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

```json
// conduit.json
{
  "port": 3000,
  "logging": "dev",
  "cors": true,
  "hotReload": { "extensions": ["html", "css", "js", "ts", "jsx", "tsx"] },
  "static": "./src",
  "proxy": { "/api": "http://localhost:4000" },
  "fallback": { "file": "./src/index.html", "status": 200 }
}
```

→ [`examples/dev-hot-reload.yaml`](../examples/dev-hot-reload.yaml) / [`dev-hot-reload.json`](../examples/dev-hot-reload.json)

---

### SPA + API (production)

Pre-compressed assets, least-conn balancing, API cache, content-aware fallback.

```yaml
# conduit.yaml
port: 443
tls:
  cert: /etc/tls/cert.pem
  key:  /etc/tls/key.pem
  httpRedirectPort: 80

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
  preCompressed: true
  maxAge: "1y"

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
      skipIfCookie: true

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
    html: { file: ./dist/index.html, status: 200 }
    json: { body: { error: "Not Found", status: 404 }, status: 404 }
```

```json
// conduit.json
{
  "port": 443,
  "tls": { "cert": "/etc/tls/cert.pem", "key": "/etc/tls/key.pem", "httpRedirectPort": 80 },
  "http2": true,
  "securityHeaders": true,
  "compression": true,
  "cors": { "origins": ["https://app.example.com"], "credentials": true },
  "logging": { "format": "json", "file": "/var/log/conduit/access.log", "skipPaths": ["/__health__", "/__metrics__"] },
  "static": "./dist",
  "staticOptions": { "preCompressed": true, "maxAge": "1y" },
  "proxy": {
    "/api": {
      "targets": ["http://api1:4000", "http://api2:4000"],
      "strategy": "least-conn",
      "stripPrefix": true,
      "retry": { "attempts": 3, "conditions": ["connection_error", "5xx"] },
      "healthCheck": { "path": "/health", "intervalSecs": 10 },
      "cache": { "store": "memory", "ttlSecs": 60, "skipIfCookie": true }
    }
  },
  "rateLimit": { "windowSecs": 60, "limit": 300, "skipPaths": ["/__health__"] },
  "healthCheck": true,
  "metrics": { "path": "/__metrics__", "token": "$METRICS_TOKEN" },
  "fallback": {
    "byAccept": {
      "html": { "file": "./dist/index.html", "status": 200 },
      "json": { "body": { "error": "Not Found", "status": 404 }, "status": 404 }
    }
  }
}
```

→ [`examples/spa-with-api.yaml`](../examples/spa-with-api.yaml) / [`spa-with-api.json`](../examples/spa-with-api.json)

---

## HTTPS

### Manual certificates

TLS with your own certificate files — from Let's Encrypt CLI, Certbot, or a CA.

```yaml
port: 443
tls:
  cert: /etc/tls/fullchain.pem
  key:  /etc/tls/privkey.pem
  httpRedirectPort: 80        # redirect port 80 → 443 automatically
  versions: ["TLSv1.2", "TLSv1.3"]
http2: true
securityHeaders: true
proxy:
  /: "http://localhost:4000"
```

```json
// conduit.json
{
  "port": 443,
  "tls": {
    "cert": "/etc/tls/fullchain.pem",
    "key": "/etc/tls/privkey.pem",
    "httpRedirectPort": 80,
    "versions": ["TLSv1.2", "TLSv1.3"]
  },
  "http2": true,
  "securityHeaders": true,
  "proxy": { "/": "http://localhost:4000" }
}
```

→ [`examples/tls-h2.yaml`](../examples/tls-h2.yaml) / [`tls-h2.json`](../examples/tls-h2.json)

---

### Auto-TLS via Let's Encrypt

Conduit obtains and renews certificates automatically.
The domain must point to this server and port 80 must be reachable.

```yaml
port: 443
tls:
  acme:
    email: admin@example.com
    storage: /var/cache/conduit/certs
    challenge: http-01
    # Uncomment to test with staging (no rate limits):
    # directory: "https://acme-staging-v02.api.letsencrypt.org/directory"
http2: true
securityHeaders: true
proxy:
  /: "http://localhost:4000"
```

```json
// conduit.json
{
  "port": 443,
  "tls": {
    "acme": {
      "email": "admin@example.com",
      "storage": "/var/cache/conduit/certs",
      "challenge": "http-01"
    }
  },
  "http2": true,
  "securityHeaders": true,
  "proxy": { "/": "http://localhost:4000" }
}
```

→ [`examples/tls-acme.yaml`](../examples/tls-acme.yaml) / [`tls-acme.json`](../examples/tls-acme.json)

---

### mTLS — require client certificates

Every client must present a certificate signed by your CA. Useful for
service-to-service auth, B2B APIs, and IoT devices.

```yaml
port: 443
tls:
  cert: /etc/tls/server.crt
  key:  /etc/tls/server.key
  clientAuth:
    ca: /etc/tls/client-ca.crt   # CA that signs authorized client certs
    optional: false               # reject connections without a valid cert
proxy:
  /api:
    targets: ["http://backend:4000"]
    stripPrefix: true
```

```json
// conduit.json
{
  "port": 443,
  "tls": {
    "cert": "/etc/tls/server.crt",
    "key": "/etc/tls/server.key",
    "clientAuth": { "ca": "/etc/tls/client-ca.crt", "optional": false }
  },
  "proxy": { "/api": { "targets": ["http://backend:4000"], "stripPrefix": true } }
}
```

→ [`examples/mtls.yaml`](../examples/mtls.yaml) / [`mtls.json`](../examples/mtls.json) — certificate generation commands included.

---

## Authentication

### JWT with JWKS (Auth0 / Cognito / Google)

Validates `Authorization: Bearer <token>` on every request. JWT claims are
injected as upstream headers so backends don't need to re-validate.

```yaml
port: 8080

jwtAuth:
  jwksUrl: "https://YOUR_DOMAIN.auth0.com/.well-known/jwks.json"
  audience: ["https://api.example.com"]
  issuer: "https://YOUR_DOMAIN.auth0.com"
  skipPaths: [/__health__]

# Inject validated claims so the backend knows who the user is.
requestTransform:
  setHeaders:
    X-User-ID:    "{{ jwt.sub }}"
    X-User-Email: "{{ jwt.email }}"
  removeHeaders: [Authorization]   # backend trusts X-User-* instead

proxy:
  /api: "http://backend:4000"
healthCheck: true
```

```json
// conduit.json
{
  "port": 8080,
  "jwtAuth": {
    "jwksUrl": "https://YOUR_DOMAIN.auth0.com/.well-known/jwks.json",
    "audience": ["https://api.example.com"],
    "issuer": "https://YOUR_DOMAIN.auth0.com",
    "skipPaths": ["/__health__"]
  },
  "requestTransform": {
    "setHeaders": { "X-User-ID": "{{ jwt.sub }}", "X-User-Email": "{{ jwt.email }}" },
    "removeHeaders": ["Authorization"]
  },
  "proxy": { "/api": "http://backend:4000" },
  "healthCheck": true
}
```

→ [`examples/jwt-auth.yaml`](../examples/jwt-auth.yaml) / [`jwt-auth.json`](../examples/jwt-auth.json)

---

### API key with multiple keys

Rotate keys without downtime by keeping both old and new active.

```yaml
port: 8080

apiKey:
  keys:
    - "$API_KEY_V2"     # new key
    - "$API_KEY_V1"     # old key — remove after all clients migrate
  header: X-API-Key
  skipPaths: [/__health__, /public/**]

proxy:
  /api: "http://backend:4000"
```

```json
// conduit.json
{
  "port": 8080,
  "apiKey": {
    "keys": ["$API_KEY_V2", "$API_KEY_V1"],
    "header": "X-API-Key",
    "skipPaths": ["/__health__", "/public/**"]
  },
  "proxy": { "/api": "http://backend:4000" }
}
```

---

### Named consumer tiers

Each API client gets its own credentials, rate limit, and upstream headers.
Useful for developer portals and partner APIs.

```yaml
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

```json
// conduit.json
{
  "port": 8080,
  "consumers": {
    "skipPaths": ["/__health__"],
    "consumers": [
      { "username": "free-tier-client", "apiKey": "$FREE_KEY",
        "rateLimit": { "windowSecs": 60, "limit": 60 }, "headers": { "X-Tier": "free" } },
      { "username": "premium-client", "apiKey": "$PREMIUM_KEY",
        "rateLimit": { "windowSecs": 60, "limit": 6000 }, "headers": { "X-Tier": "premium" } },
      { "username": "internal-service", "basicAuth": { "password": "$INTERNAL_PASSWORD" },
        "headers": { "X-Internal": "true" } }
    ]
  },
  "proxy": { "/api": "http://backend:4000" }
}
```

→ [`examples/consumers.yaml`](../examples/consumers.yaml) / [`consumers.json`](../examples/consumers.json)

---

### External auth service (Forward Auth)

Delegate every auth decision to an existing service (oauth2-proxy, Ory
Oathkeeper, custom SSO middleware). The auth service's response headers
(`X-User-ID`, `X-Role`, …) are forwarded to the upstream.

```yaml
port: 8080

forwardAuth:
  url: "http://auth-service:9000/verify"
  requestHeaders: [Authorization, Cookie]
  responseHeaders: [X-User-ID, X-Role, X-Tenant]
  timeoutMs: 3000
  skipPaths: [/__health__, /login, /public/**]

proxy:
  /api: "http://backend:4000"
```

```json
// conduit.json
{
  "port": 8080,
  "forwardAuth": {
    "url": "http://auth-service:9000/verify",
    "requestHeaders": ["Authorization", "Cookie"],
    "responseHeaders": ["X-User-ID", "X-Role", "X-Tenant"],
    "timeoutMs": 3000,
    "skipPaths": ["/__health__", "/login", "/public/**"]
  },
  "proxy": { "/api": "http://backend:4000" }
}
```

→ [`examples/forward-auth.yaml`](../examples/forward-auth.yaml) / [`forward-auth.json`](../examples/forward-auth.json)

---

## Load balancing

### Weighted round-robin

Send more traffic to powerful instances, or gradually shift traffic during a
canary deployment.

```yaml
proxy:
  /api:
    targets:
      - { url: "http://main:4000", weight: 9 }    # 90%
      - { url: "http://canary:4000", weight: 1 }  # 10% canary
    strategy: weighted-round-robin
```

```json
// conduit.json
{
  "proxy": {
    "/api": {
      "targets": [
        { "url": "http://main:4000", "weight": 9 },
        { "url": "http://canary:4000", "weight": 1 }
      ],
      "strategy": "weighted-round-robin"
    }
  }
}
```

Use `conduit upstreams weight` to adjust weights at runtime without a reload.

→ [`examples/load-balanced.yaml`](../examples/load-balanced.yaml) / [`load-balanced.json`](../examples/load-balanced.json)

---

### Least connections with health checks

Routes each request to the backend with the fewest active connections.
Removes unhealthy backends automatically and ramps traffic back slowly.

```yaml
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
      slowStartSecs: 30     # ramp recovered upstream over 30 s
```

```json
// conduit.json
{
  "proxy": {
    "/api": {
      "targets": ["http://api1:4000", "http://api2:4000", "http://api3:4000"],
      "strategy": "least-conn",
      "healthCheck": {
        "path": "/health",
        "intervalSecs": 10,
        "unhealthyThreshold": 2,
        "slowStartSecs": 30
      }
    }
  }
}
```

→ [`examples/load-balanced.yaml`](../examples/load-balanced.yaml) / [`load-balanced.json`](../examples/load-balanced.json)

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
    backup: "http://dr-site:4000"   # used only when all primaries are unhealthy
    retry:
      attempts: 2
      conditions: [connection_error, "5xx"]
```

```json
// conduit.json
{
  "proxy": {
    "/api": {
      "targets": ["http://primary1:4000", "http://primary2:4000"],
      "strategy": "round-robin",
      "healthCheck": { "path": "/health", "intervalSecs": 10 },
      "backup": "http://dr-site:4000",
      "retry": { "attempts": 2, "conditions": ["connection_error", "5xx"] }
    }
  }
}
```

---

### Geo-regional routing (upstream groups)

Outer strategy (ip-hash) pins each client to a region; inner strategy
(least-conn) balances within the region.

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
    groupStrategy: ip-hash
```

```json
// conduit.json
{
  "proxy": {
    "/api": {
      "groups": [
        { "name": "us-east", "targets": ["http://us-east-1:4000", "http://us-east-2:4000"], "strategy": "least-conn" },
        { "name": "eu-west", "targets": ["http://eu-west-1:4000", "http://eu-west-2:4000"], "strategy": "least-conn" }
      ],
      "groupStrategy": "ip-hash"
    }
  }
}
```

→ [`examples/upstream-groups.yaml`](../examples/upstream-groups.yaml) / [`upstream-groups.json`](../examples/upstream-groups.json)

---

## Reliability

### Circuit breaker + retry budget

503 when all upstreams are saturated. Retry storms limited to 20% of traffic.

```yaml
proxy:
  /api:
    targets: ["http://a:4000", "http://b:4000", "http://c:4000"]
    strategy: least-conn
    healthCheck:
      path: /health
      intervalSecs: 10
      maxConnectionsPerUpstream: 100   # circuit breaker: 503 when all hit this
    backup: "http://replica:4000"
    retry:
      attempts: 3
      conditions: [connection_error, "5xx", timeout]
      backoffMs: 100
      budgetPercent: 20   # max 20% of active requests may be retries
    timeout:
      connectMs: 500
      readMs: 10000
      perTryMs: 3000

outlierDetection:
  consecutive5xx: 5
  baseEjectionTimeSecs: 30
  maxEjectionTimeSecs: 300
  maxEjectionPercent: 33

maskErrors: true   # hide upstream stack traces from clients
```

```json
// conduit.json
{
  "proxy": {
    "/api": {
      "targets": ["http://a:4000", "http://b:4000", "http://c:4000"],
      "strategy": "least-conn",
      "healthCheck": { "path": "/health", "intervalSecs": 10, "maxConnectionsPerUpstream": 100 },
      "backup": "http://replica:4000",
      "retry": { "attempts": 3, "conditions": ["connection_error", "5xx", "timeout"], "backoffMs": 100, "budgetPercent": 20 },
      "timeout": { "connectMs": 500, "readMs": 10000, "perTryMs": 3000 }
    }
  },
  "outlierDetection": { "consecutive5xx": 5, "baseEjectionTimeSecs": 30, "maxEjectionTimeSecs": 300, "maxEjectionPercent": 33 },
  "maskErrors": true
}
```

→ [`examples/circuit-breaker.yaml`](../examples/circuit-breaker.yaml) / [`circuit-breaker.json`](../examples/circuit-breaker.json)

---

### Response caching with stale-while-revalidate

Zero-latency cache expiry: stale content is served immediately while a
background request fetches fresh data.

```yaml
proxy:
  /api:
    targets: ["http://backend:4000"]
    stripPrefix: true
    cache:
      store: memory
      ttlSecs: 60
      staleWhileRevalidateSecs: 300   # serve stale for up to 5 min while refreshing
      staleIfErrorSecs: 600           # serve stale if upstream is down
      varyHeaders: [Accept-Language]
      skipIfCookie: true
      skipPaths: [/api/me, /api/cart]
```

```json
// conduit.json
{
  "proxy": {
    "/api": {
      "targets": ["http://backend:4000"],
      "stripPrefix": true,
      "cache": {
        "store": "memory",
        "ttlSecs": 60,
        "staleWhileRevalidateSecs": 300,
        "staleIfErrorSecs": 600,
        "varyHeaders": ["Accept-Language"],
        "skipIfCookie": true,
        "skipPaths": ["/api/me", "/api/cart"]
      }
    }
  }
}
```

→ [`examples/stale-while-revalidate.yaml`](../examples/stale-while-revalidate.yaml) / [`stale-while-revalidate.json`](../examples/stale-while-revalidate.json)

---

## API gateway

### Microservices gateway

Route traffic to individual services by path. One place for rate limiting,
IP filtering, auth, and metrics.

```yaml
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

```json
// conduit.json
{
  "port": 8080,
  "logging": "json",
  "ipFilter": { "allow": ["10.0.0.0/8", "172.16.0.0/12"] },
  "rateLimit": { "windowSecs": 60, "limit": 500 },
  "proxy": {
    "/users": "http://users-svc:4001",
    "/orders": "http://orders-svc:4002",
    "/catalog": { "targets": ["http://catalog1:4003", "http://catalog2:4003"], "cache": { "store": "memory", "ttlSecs": 300 } },
    "/payments": { "targets": ["https://payment-svc:8443"], "upstreamTls": { "verify": true }, "rateLimit": { "windowSecs": 60, "limit": 20, "keyBy": "header:X-User-ID" } }
  },
  "healthCheck": true,
  "metrics": { "path": "/__metrics__" },
  "maskErrors": true
}
```

→ [`examples/api-gateway.yaml`](../examples/api-gateway.yaml) / [`api-gateway.json`](../examples/api-gateway.json)

---

### JWT gateway with per-route rate limits

Validate JWT once at the gateway. Inject user identity as headers.
Strict per-route limits on expensive endpoints.

```yaml
port: 443
tls:
  cert: /etc/tls/cert.pem
  key:  /etc/tls/key.pem
  httpRedirectPort: 80

jwtAuth:
  jwksUrl: "https://auth.example.com/.well-known/jwks.json"
  audience: ["api.example.com"]
  skipPaths: [/__health__]

requestTransform:
  setHeaders:
    X-User-ID:   "{{ jwt.sub }}"
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

```json
// conduit.json
{
  "port": 443,
  "tls": { "cert": "/etc/tls/cert.pem", "key": "/etc/tls/key.pem", "httpRedirectPort": 80 },
  "jwtAuth": { "jwksUrl": "https://auth.example.com/.well-known/jwks.json", "audience": ["api.example.com"], "skipPaths": ["/__health__"] },
  "requestTransform": { "setHeaders": { "X-User-ID": "{{ jwt.sub }}", "X-User-Role": "{{ jwt.role }}" }, "removeHeaders": ["Authorization"] },
  "responseTransform": { "removeHeaders": ["Server", "X-Powered-By"] },
  "proxy": {
    "/v1/users": { "targets": ["http://users:4001", "http://users:4002"], "strategy": "least-conn", "stripPrefix": true, "rateLimit": { "windowSecs": 60, "limit": 200, "keyBy": "header:X-User-ID" } },
    "/v1/payments": { "targets": ["http://payments:4002"], "stripPrefix": true, "rateLimit": { "windowSecs": 60, "limit": 10, "keyBy": "header:X-User-ID" } },
    "/v1/search": { "targets": ["http://search:4003"], "stripPrefix": true, "cache": { "store": "memory", "ttlSecs": 30 } }
  },
  "healthCheck": true,
  "maskErrors": true,
  "logging": { "format": "json", "skipPaths": ["/__health__"] }
}
```

→ [`examples/api-gateway.yaml`](../examples/api-gateway.yaml) / [`api-gateway.json`](../examples/api-gateway.json)

---

## Security hardening

Defence-in-depth: TLS, security headers, CORS, IP allowlist, rate limit, API
key, error masking, upstream TLS verification.

```yaml
global:
  admin:
    bind: "127.0.0.1:2019"
    token: "$ADMIN_TOKEN"

sites:
  - port: 443
    host: secure.example.com

    tls:
      cert: /etc/tls/server.crt
      key:  /etc/tls/server.key
      httpRedirectPort: 80
      versions: ["TLSv1.2", "TLSv1.3"]

    securityHeaders:
      hstsMaxAgeSecs: 63072000   # 2 years
      csp: "default-src 'self'"
      xFrameOptions: DENY
      referrerPolicy: "strict-origin-when-cross-origin"

    cors:
      origins: ["https://app.example.com"]
      credentials: true
      allowedHeaders: [Authorization, Content-Type]

    ipFilter:
      allow: ["10.0.0.0/8", "172.16.0.0/12"]

    rateLimit:
      windowSecs: 60
      limit: 200

    apiKey:
      keys: ["$API_KEY_PRIMARY", "$API_KEY_SECONDARY"]
      skipPaths: [/__health__]

    maskErrors: true

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

```json
// conduit.json
{
  "global": { "admin": { "bind": "127.0.0.1:2019", "token": "$ADMIN_TOKEN" } },
  "sites": [{
    "port": 443, "host": "secure.example.com",
    "tls": { "cert": "/etc/tls/server.crt", "key": "/etc/tls/server.key", "httpRedirectPort": 80, "versions": ["TLSv1.2", "TLSv1.3"] },
    "securityHeaders": { "hstsMaxAgeSecs": 63072000, "csp": "default-src 'self'", "xFrameOptions": "DENY" },
    "cors": { "origins": ["https://app.example.com"], "credentials": true },
    "ipFilter": { "allow": ["10.0.0.0/8", "172.16.0.0/12"] },
    "rateLimit": { "windowSecs": 60, "limit": 200 },
    "apiKey": { "keys": ["$API_KEY_PRIMARY", "$API_KEY_SECONDARY"], "skipPaths": ["/__health__"] },
    "maskErrors": true,
    "proxy": { "/api": { "targets": ["https://api-internal:8443"], "stripPrefix": true, "upstreamTls": { "verify": true } } },
    "healthCheck": true
  }]
}
```

→ [`examples/security-hardened.yaml`](../examples/security-hardened.yaml) / [`security-hardened.json`](../examples/security-hardened.json)

---

## Observability

Prometheus metrics, OTLP tracing (Grafana Tempo), structured JSON logs, and
passive outlier detection — all in one config.

> OTLP tracing requires `cargo build --features otlp`.

```yaml
global:
  otlp:
    endpoint: "http://tempo:4317"
    serviceName: "my-service"
    sampleRate: 0.1        # 10% sampling in production

  admin:
    bind: "127.0.0.1:2019"

sites:
  - port: 8080
    logging:
      format: json
      file: ./logs/access.log
      skipPaths: [/__health__, /__metrics__]

    metrics:
      path: /__metrics__
      token: "$METRICS_TOKEN"

    healthCheck:
      includeUpstreams: true

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

```json
// conduit.json
{
  "global": {
    "otlp": { "endpoint": "http://tempo:4317", "serviceName": "my-service", "sampleRate": 0.1 },
    "admin": { "bind": "127.0.0.1:2019" }
  },
  "sites": [{
    "port": 8080,
    "logging": { "format": "json", "file": "./logs/access.log", "skipPaths": ["/__health__", "/__metrics__"] },
    "metrics": { "path": "/__metrics__", "token": "$METRICS_TOKEN" },
    "healthCheck": { "includeUpstreams": true },
    "outlierDetection": { "consecutive5xx": 5, "baseEjectionTimeSecs": 30, "maxEjectionTimeSecs": 300, "maxEjectionPercent": 10 },
    "securityHeaders": true,
    "proxy": {
      "/api": {
        "targets": ["http://api1:4000", "http://api2:4000"],
        "strategy": "least-conn",
        "stripPrefix": true,
        "healthCheck": { "path": "/health", "intervalSecs": 10 }
      }
    }
  }]
}
```

→ [`examples/observability.yaml`](../examples/observability.yaml) / [`observability.json`](../examples/observability.json)

---

## Multi-site virtual hosting

Three sites from one process — each with its own auth, TLS, and backends.

```yaml
global:
  workers: 4
  admin:
    bind: "127.0.0.1:2019"
    token: "$ADMIN_TOKEN"

sites:
  # Public app — JWT auth, HTTPS
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
      key:  /etc/tls/admin.key
    ipFilter:
      allow: ["10.0.0.0/8"]
    basicAuth:
      users: { admin: "$ADMIN_PASSWORD" }
    proxy:
      /: "http://admin-ui:3000"

  # Internal metrics — no TLS, loopback only
  - port: 9090
    host: 127.0.0.1
    metrics:
      path: /metrics
    healthCheck: true
```

```json
// conduit.json
{
  "global": { "workers": 4, "admin": { "bind": "127.0.0.1:2019", "token": "$ADMIN_TOKEN" } },
  "sites": [
    {
      "port": 443, "host": "app.example.com",
      "tls": { "acme": { "email": "admin@example.com", "storage": "/var/cache/conduit/certs", "challenge": "http-01" } },
      "jwtAuth": { "jwksUrl": "https://auth.example.com/.well-known/jwks.json", "skipPaths": ["/__health__"] },
      "proxy": { "/api": "http://app-backend:4000" },
      "static": "./dist",
      "fallback": { "file": "./dist/index.html", "status": 200 }
    },
    {
      "port": 443, "host": "admin.example.com",
      "tls": { "cert": "/etc/tls/admin.crt", "key": "/etc/tls/admin.key" },
      "ipFilter": { "allow": ["10.0.0.0/8"] },
      "basicAuth": { "users": { "admin": "$ADMIN_PASSWORD" } },
      "proxy": { "/": "http://admin-ui:3000" }
    },
    {
      "port": 9090, "host": "127.0.0.1",
      "metrics": { "path": "/metrics" },
      "healthCheck": true
    }
  ]
}
```

→ [`examples/multi-site.yaml`](../examples/multi-site.yaml) / [`multi-site.json`](../examples/multi-site.json)

---

## Ready to deploy?

Config written — now run it in production.

**[→ Deployment Guide](deployment.md)** — Docker, systemd, Kubernetes, production
checklist, secrets management.
