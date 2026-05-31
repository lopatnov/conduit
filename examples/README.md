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
| HTTPS with certificates               | `tls-h2.yaml` or `tls-acme.yaml`       |
| Auto-TLS via Let's Encrypt            | `tls-acme.yaml`                        |
| Multiple backends, load balancing     | `load-balanced.yaml`                   |
| Multiple apps on one server           | `multi-site.yaml`                      |
| Local dev with hot reload             | `dev-hot-reload.yaml`                  |
| JWT / external auth                   | `jwt-auth.yaml` or `forward-auth.yaml` |
| Full API gateway                      | `api-gateway.yaml`                     |
| Resilience (circuit breaker, retries) | `circuit-breaker.yaml`                 |
| Metrics + tracing                     | `observability.yaml`                   |
| Security hardening                    | `security-hardened.yaml`               |
| Response cache                        | `with-cache.yaml`                      |
| URL rewriting                         | `path-rewrite.yaml`                    |
| Custom Rhai middleware                | `rhai-middleware.yaml`                 |
| Redis rate limiting                   | `redis-rate-limit.yaml`                |

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

## Authentication examples

### [jwt-auth.yaml](jwt-auth.yaml) ✨ NEW

JWT bearer-token validation. Supports JWKS endpoints (Auth0, AWS Cognito,
Keycloak, Google) and HS256 shared secrets. Injects JWT claims as upstream
headers using `{{ jwt.sub }}` template syntax.

**Features:** `jwtAuth`, `requestTransform`, `{{ jwt.<claim> }}` templates

---

### [forward-auth.yaml](forward-auth.yaml) ✨ NEW

Delegate auth to an external HTTP service (oauth2-proxy, Ory Oathkeeper,
custom auth middleware). The auth service's response headers are injected
into the upstream request.

**Features:** `forwardAuth`, `responseHeaders` injection, `skipPaths`

---

## Resilience examples

### [circuit-breaker.yaml](circuit-breaker.yaml) ✨ NEW

Circuit breaker, retry budget, service failover, and outlier detection — all
working together to keep the service stable under load and failures.

**Features:** `healthCheck.maxConnectionsPerUpstream`, `retry.budgetPercent`,
`backup` (failover), `outlierDetection`, `maskErrors`

---

## Observability examples

### [observability.yaml](observability.yaml) ✨ NEW

Full observability stack: Prometheus metrics, OpenTelemetry OTLP tracing
(Grafana Tempo / Jaeger), structured JSON access logs, and health endpoints.

**Features:** `global.otlp` (requires `--features otlp`), `metrics`,
`logging.format: json`, `logging.skipPaths`, `outlierDetection`

---

## Security examples

### [security-hardened.yaml](security-hardened.yaml) ✨ NEW

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
