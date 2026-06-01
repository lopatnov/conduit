# Configuration Examples

Each file is a ready-to-run Conduit configuration. YAML files include inline
comments explaining every option — start there if you're new to Conduit.
JSON files are equivalent but without comments.

```bash
# Run with a specific config
conduit -c examples/minimal.yaml

# Validate before applying
conduit validate -c examples/api-gateway.yaml

# Or generate a config interactively
conduit init
```

---

## Quick start

| Goal                                  | Example                                |
| ------------------------------------- | -------------------------------------- |
| Serve files + proxy locally           | `minimal.yaml`                         |
| React / Vue / Angular SPA             | `spa-with-api.yaml`                    |
| Local dev with hot reload             | `dev-hot-reload.yaml`                  |
| HTTPS with manual certificates        | `tls-h2.yaml`                          |
| Auto-TLS via Let's Encrypt            | `tls-acme.yaml`                        |
| mTLS — require client certificates    | `mtls.yaml`                            |
| Multiple backends, load balancing     | `load-balanced.yaml`                   |
| Geo-regional / two-level balancing    | `upstream-groups.yaml`                 |
| Multiple apps on one server           | `multi-site.yaml`                      |
| JWT / external auth                   | `jwt-auth.yaml` or `forward-auth.yaml` |
| Named API clients (consumer model)    | `consumers.yaml`                       |
| Full API gateway                      | `api-gateway.yaml`                     |
| Resilience (circuit breaker, retries) | `circuit-breaker.yaml`                 |
| Priority-based load shedding          | `priority-routing.yaml`                |
| Response cache + stale-while-revalidate | `stale-while-revalidate.yaml`        |
| Metrics + tracing                     | `observability.yaml`                   |
| Security hardening                    | `security-hardened.yaml`               |
| URL rewriting                         | `path-rewrite.yaml`                    |
| Custom Rhai middleware                | `rhai-middleware.yaml`                 |
| Redis rate limiting                   | `redis-rate-limit.yaml`                |

For each scenario with both YAML and JSON side by side, and explanations, see
**[docs/recipes.md](../docs/recipes.md)**.

---

## Core examples

### [minimal.yaml](minimal.yaml) / [minimal.json](minimal.json)

The smallest useful config: serve static files from `./dist` and proxy `/api`
to a backend on port 4000.

**Features:** `static`, `proxy` (single upstream)

---

### [spa-with-api.yaml](spa-with-api.yaml) / [spa-with-api.json](spa-with-api.json)

Production Single-Page Application: pre-compressed static assets, API proxy
with least-connection balancing and a 5-minute cache, JSON / HTML fallback.

**Features:** `compression`, `staticOptions.preCompressed`, `proxy.cache`,
`fallback.byAccept`, `logging.skipPaths`

---

### [tls-h2.yaml](tls-h2.yaml) / [tls-h2.json](tls-h2.json)

HTTPS with manual certificates, HTTP/2, automatic HTTP→HTTPS redirect,
security headers, and long-TTL static asset caching.

**Features:** `tls.cert/key`, `tls.httpRedirectPort`, `http2`, `securityHeaders`

---

### [tls-acme.yaml](tls-acme.yaml) / [tls-acme.json](tls-acme.json)

Auto-TLS via Let's Encrypt — no manual certificate management.

**Features:** `tls.acme`, `proxy.healthCheck`

---

### [load-balanced.yaml](load-balanced.yaml) / [load-balanced.json](load-balanced.json)

Multi-strategy load balancing: weighted round-robin, IP-hash, and
least-connections across three route groups.

**Features:** `weighted-round-robin`, `ip-hash`, `least-conn`, `retry`

---

### [multi-site.yaml](multi-site.yaml) / [multi-site.json](multi-site.json)

Three virtual hosts from one process: public app with JWT auth, admin panel
with Basic Auth, and a catch-all 404.

**Features:** virtual hosting, `jwtAuth`, `basicAuth`, `tls`, `fallback`

---

### [dev-hot-reload.yaml](dev-hot-reload.yaml) / [dev-hot-reload.json](dev-hot-reload.json)

Development server with browser hot reload, open CORS, and SPA fallback.

**Features:** `hotReload`, `cors: true`, `logging: dev`, `fallback`

---

### [with-cache.yaml](with-cache.yaml) / [with-cache.json](with-cache.json)

In-memory proxy cache with TTL, Vary headers, and cookie / path exclusions.

**Features:** `cache.store: memory`, `cache.varyHeaders`, `cache.skipIfCookie`

---

### [api-gateway.yaml](api-gateway.yaml) / [api-gateway.json](api-gateway.json)

Full API gateway: JWT auth, per-route rate limiting, JWT claim injection,
response header cleanup, traffic mirroring to a shadow environment, circuit
breaker, failover, outlier detection, and OTLP tracing.

**Features:** `jwtAuth`, `rateLimit` (per-route), `requestTransform`,
`responseTransform`, `mirror`, `healthCheck.maxConnectionsPerUpstream`,
`backup`, `outlierDetection`, `global.otlp` (requires `--features otlp`)

---

## Authentication examples

### [jwt-auth.yaml](jwt-auth.yaml) / [jwt-auth.json](jwt-auth.json)

JWT bearer-token validation. Supports JWKS endpoints (Auth0, AWS Cognito,
Keycloak, Google) and HS256 shared secrets. Injects JWT claims as upstream
headers using `{{ jwt.sub }}` template syntax.

**Features:** `jwtAuth`, `requestTransform`, `{{ jwt.<claim> }}` templates

---

### [forward-auth.yaml](forward-auth.yaml) / [forward-auth.json](forward-auth.json)

Delegate auth to an external HTTP service (oauth2-proxy, Ory Oathkeeper,
custom auth middleware). The auth service's response headers are injected
into the upstream request.

**Features:** `forwardAuth`, `responseHeaders` injection, `skipPaths`

---

### [consumers.yaml](consumers.yaml) / [consumers.json](consumers.json)

Named API clients with per-consumer credentials (API key, Basic Auth, JWT),
rate limits, and custom headers. Includes the sharedJwt pattern for Auth0 /
Cognito / Keycloak.

**Features:** `consumers`, `consumer.apiKey`, `consumer.basicAuth`,
`consumer.jwt`, `consumers.sharedJwt` (V3)

---

## Resilience examples

### [circuit-breaker.yaml](circuit-breaker.yaml) / [circuit-breaker.json](circuit-breaker.json)

Circuit breaker, retry budget, service failover, and outlier detection — all
working together to keep the service stable under load and failures.

**Features:** `healthCheck.maxConnectionsPerUpstream`, `retry.budgetPercent`,
`backup` (failover), `outlierDetection`, `maskErrors`

---

### [priority-routing.yaml](priority-routing.yaml) / [priority-routing.json](priority-routing.json)

Progressive load shedding: when the site is under heavy load, low-priority
routes (batch jobs, analytics) are rejected with `503 Load Shedding` while
critical routes (checkout, main API) continue to be served normally.

**Features:** `limits.maxInflightRequests`, `limits.priorityThreshold`,
`proxy.*.priority`, `X-Priority` header override

---

### [stale-while-revalidate.yaml](stale-while-revalidate.yaml) / [stale-while-revalidate.json](stale-while-revalidate.json)

Serve stale cached content immediately while refreshing in the background.
Zero latency penalty on cache expiry.

**Features:** `cache.staleWhileRevalidateSecs`, `cache.staleIfErrorSecs`

---

### [mtls.yaml](mtls.yaml) / [mtls.json](mtls.json)

Mutual TLS — require clients to present a certificate signed by a trusted CA.
Authentication at the TLS handshake, before any HTTP processing.

**Features:** `tls.clientAuth`, certificate generation guide

---

## Observability examples

### [observability.yaml](observability.yaml) / [observability.json](observability.json)

Full observability stack: Prometheus metrics, OpenTelemetry OTLP tracing
(Grafana Tempo / Jaeger), structured JSON access logs, and health endpoints.

**Features:** `global.otlp` (requires `--features otlp`), `metrics`,
`logging.format: json`, `logging.skipPaths`, `outlierDetection`

---

## Security examples

### [security-hardened.yaml](security-hardened.yaml) / [security-hardened.json](security-hardened.json)

Defence-in-depth configuration: TLS hardening, security headers, CORS policy,
IP allowlist, rate limiting, API key auth, error masking, admin auth, and
upstream TLS verification.

**Features:** `securityHeaders`, `cors.origins`, `ipFilter`, `rateLimit`,
`apiKey`, `maskErrors`, `global.admin.token`, `upstreamTls`

---

## Advanced routing examples

### [routes.yaml](routes.yaml) / [routes.json](routes.json)

Advanced routing with the `routes` array: match on path glob, HTTP method,
query parameters, and request headers.

**Features:** `routes[].match.method`, `routes[].match.path`, `ipFilter`

---

### [path-rewrite.yaml](path-rewrite.yaml) / [path-rewrite.json](path-rewrite.json)

Regex-based URL rewriting: strip version prefixes and remap legacy paths.

**Features:** `proxy.rewrite`, regex capture groups (`$1`)

---

### [upstream-groups.yaml](upstream-groups.yaml) / [upstream-groups.json](upstream-groups.json)

Two-level load balancing: outer strategy selects a region group, inner
strategy spreads load within the group.

**Features:** `proxy.groups`, `groupStrategy`

---

## Middleware examples

### [rhai-middleware.yaml](rhai-middleware.yaml) / [rhai-middleware.json](rhai-middleware.json)

Custom request middleware in Rhai script: read headers, inject values, or
reject requests.

**Features:** `middleware[].type: script`, Rhai scripting

---

### [redis-rate-limit.yaml](redis-rate-limit.yaml) / [redis-rate-limit.json](redis-rate-limit.json)

Rate limiting backed by Redis for consistent limits across multiple Conduit
instances.

**Features:** `rateLimit.store: redis://`, multi-instance rate limiting

---

## YAML vs JSON

Both formats are fully supported. YAML is recommended for new configs:

- **Comments** — document every decision directly in the file
- **Multi-line strings** — cleaner CSP / header values
- **Less noise** — no quotes, commas, or brackets required

```bash
# YAML (recommended for new configs)
conduit -c conduit.yaml

# JSON (compatible with all JSON tooling)
conduit -c conduit.json
```

Environment variable substitution works the same in both formats:
`"$MY_VAR"` or `"${MY_VAR}"` is replaced at startup.
