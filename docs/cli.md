# CLI Reference

```
conduit [OPTIONS] [COMMAND]
```

---

## Table of Contents

- [Global options](#global-options)
- [Config file discovery](#config-file-discovery)
- [Server commands](#server-commands)
  - [start (default)](#start-default)
  - [validate](#validate)
  - [fmt](#fmt)
  - [init](#init)
  - [probe](#probe)
- [Admin commands](#admin-commands)
  - [reload](#reload)
  - [status](#status)
  - [shutdown](#shutdown)
  - [upstreams](#upstreams)
- [Tooling](#tooling)
  - [completions](#completions)
  - [man](#man)
- [Environment variables](#environment-variables)
- [Exit codes](#exit-codes)
- [Kubernetes CRD mode](#kubernetes-crd-mode)
- [Build features](#build-features)
  - [`jwt`](#jwt--jwt-bearer-token-authentication)
  - [`consumers`](#consumers--consumer-model)
  - [`forward-auth`](#forward-auth--forwardauth-middleware)
  - [`rhai`](#rhai--rhai-scripting-middleware)
  - [`wasm`](#wasm--webassembly-plugin-middleware)
  - [`tcp`](#tcp--tcp-passthrough-proxy)
  - [`upload`](#upload--file-upload-handler)
  - [`redis`](#redis--redis-backed-rate-limiting-and-caching)
  - [`cache`](#cache--response-caching)
  - [`disk-cache`](#disk-cache--disk-backed-cache)
  - [`acme`](#acme--auto-tls--lets-encrypt)
  - [`fault-injection`](#fault-injection--chaos-testing)
  - [`otlp`](#otlp--opentelemetry-distributed-tracing)
  - [`kubernetes`](#kubernetes--kubernetes-crd-config-provider)

---

## Global options

These flags are accepted by every command.

| Flag                        | Short | Default                        | Description                                                                                                                                                                        |
| --------------------------- | ----- | ------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--config FILE`             | `-c`  | auto-discovered (see below)    | Config file path                                                                                                                                                                   |
| `--version`                 | `-V`  | —              | Print version and exit                                                                                                                                                             |
| `--help`                    | `-h`  | —              | Print help and exit                                                                                                                                                                |
| `--kubernetes-namespace NS` | —     | —              | Read config from Kubernetes `ConduitSite` CRDs instead of a file. `"*"` watches all namespaces. Requires `--features kubernetes`. See [Kubernetes CRD mode](#kubernetes-crd-mode). |

---

## Config file discovery

When no `-c` flag is given, Conduit resolves the config in this order:

1. `conduit.json` — if this file exists, use it
2. `conduit.yaml` — fallback if `conduit.json` is absent
3. `conduit.yml` — fallback if neither of the above exists

Auto-discovery only applies when **no `-c` flag is given**.
When you pass `-c some-other-name.json`, that exact path is used with no fallback.

Both JSON and YAML are supported everywhere `-c` is accepted.

---

## Server commands

### start (default)

Start the reverse proxy server.

```bash
conduit                       # reads conduit.json (or .yaml / .yml)
conduit -c /etc/conduit.yaml  # explicit config path
```

On startup, Conduit:

1. Loads and validates the config — exits 1 with a field-level error message on failure
2. Binds all configured ports
3. Starts the Admin API on `global.admin.bind` (only if `global.admin` is configured)
4. Begins serving traffic

If `hotReload` is enabled, Conduit watches the config file and reloads it
automatically when it changes — no restart needed.

---

### validate

Parse and validate the config file, then exit. Does **not** start the server.

```bash
conduit validate
conduit validate -c staging.yaml
```

**On success** (exit 0):

```
Config is valid — 2 sites, 5 routes.
```

**On failure** (exit 1):

```
error at proxy./api.retry.attempts: must be > 0
error at rateLimit.windowSecs: missing required field

2 errors found.
```

Error messages include the exact JSON path to the invalid field, making it
easy to locate the problem even in large config files.

**Use in CI:**

```bash
conduit validate -c conduit.yaml && echo "Config OK"
```

---

### fmt

Normalize and pretty-print the config, preserving the input format:
`.yaml` / `.yml` files stay YAML, `.json` files stay JSON.

```bash
# Print to stdout (useful for diffing)
conduit fmt
conduit fmt -c conduit.yaml   # → pretty YAML
conduit fmt -c conduit.json   # → pretty JSON

# Overwrite the file in place
conduit fmt --write
conduit fmt --write -c conduit.yaml
```

`--write` rewrites the config file in place. Useful for normalizing key order
and whitespace after manual edits.

---

### init

Interactive wizard that generates a starter config in YAML or JSON. All
questions can be skipped with flags — useful for scripts and CI pipelines.

```bash
# Interactive (default)
conduit init

# Non-interactive: accept all defaults
conduit init --yes
conduit init -y

# Non-interactive with overrides
conduit init -y --port 3000 --proxy http://localhost:4000
conduit init -y --format yaml --port 443 --tls-acme admin@example.com -o prod.yaml

# Format inferred from -o extension
conduit init -o conduit.yaml   # YAML
conduit init -o conduit.json   # JSON
```

**`--yes` defaults:**

| Setting      | Default            |
| ------------ | ------------------ |
| format       | yaml               |
| port         | 8080               |
| static       | `./dist` (enabled) |
| proxy        | disabled           |
| TLS          | disabled           |
| health check | enabled            |
| log format   | dev                |

**All flags:**

| Flag                                | Short | Description                                       |
| ----------------------------------- | ----- | ------------------------------------------------- |
| `--output FILE`                     | `-o`  | Output file path (format inferred from extension) |
| `--yes`                             | `-y`  | Non-interactive: accept all defaults, no prompts  |
| `--format <yaml\|json>`             | —     | Output format (overrides extension inference)     |
| `--port N`                          | —     | Port number [default: 8080]                       |
| `--static-dir DIR`                  | —     | Serve static files from `DIR`                     |
| `--no-static`                       | —     | Disable static file serving                       |
| `--proxy URL`                       | —     | Proxy requests to upstream `URL`                  |
| `--no-proxy`                        | —     | Disable proxy                                     |
| `--log <dev\|json\|combined\|none>` | —     | Log format [default: dev]                         |
| `--no-health`                       | —     | Disable `/__health__` endpoint                    |
| `--tls-cert FILE`                   | —     | TLS certificate file (enables manual TLS)         |
| `--tls-key FILE`                    | —     | TLS private key file (required with `--tls-cert`) |
| `--tls-acme EMAIL`                  | —     | ACME email (enables Let's Encrypt auto-TLS)       |

When both `--yes` and individual flags are given, the flags override the
defaults. Any setting not covered by a flag is silently set to its default
without prompting.

---

### probe

Send a `HEAD` request to every upstream URL defined in the config and print a
latency table. All upstreams are probed in parallel.

```bash
conduit probe
conduit probe -c production.yaml
```

**Example output:**

```
Probing 4 upstream(s) in parallel...

URL                         Status  Latency
────────────────────────────────────────────
✗ http://api-3:4000         timeout  10001 ms
✓ http://api-1:4000         200      12 ms
✓ http://api-2:4000         200      14 ms
✓ https://payment-svc:8443  200      31 ms

3/4 upstreams healthy
```

Results are sorted so failures appear first. Exits **1** if any upstream is
unhealthy (status ≥ 500 or connection failure), **0** if all pass.

**Notes:**

- `https://` upstreams are checked with a **plain TCP connect** — TLS is not
  negotiated and the certificate is not verified. An unreachable host still shows
  as a failure, but a host with an expired or self-signed cert will show as healthy.
- The probe path defaults to `/` — adjust the upstream URL if a different
  path is required.
- Useful as a pre-deploy readiness check in CI:
  ```bash
  conduit probe -c conduit.yaml || exit 1
  ```

---

## Admin commands

These commands talk to the running server's Admin API.
See **[docs/admin.md](admin.md)** for the full HTTP API reference, authentication
details, and all endpoint request/response schemas.

The Admin API HTTP server only starts when `global.admin` is configured.
Without it, admin CLI commands will fail to connect.

The admin address (where the CLI connects to) is resolved in this order:

1. `--admin ADDR` flag
2. `CONDUIT_ADMIN` environment variable
3. Default: `127.0.0.1:2019`

> The default `127.0.0.1:2019` is only the **CLI connection target**.
> The server does not open this port unless `global.admin.bind` is set in config.

When `global.admin.token` is set on the server, authenticate CLI commands via
the `CONDUIT_ADMIN_TOKEN` environment variable:

```bash
export CONDUIT_ADMIN_TOKEN="my-secret-token"
conduit status     # token sent automatically
conduit reload     # token sent automatically
```

For direct `curl` calls, pass the token as a Bearer header:

```bash
curl -H "Authorization: Bearer $CONDUIT_ADMIN_TOKEN" http://localhost:2019/status
```

---

### reload

Hot-reload the config file without restarting the server.

```bash
conduit reload
conduit reload --admin 127.0.0.1:2019
```

The server re-reads the config file from disk and applies all hot-reloadable
changes immediately. Fields that require a cold restart (port, TLS cert/key,
workers) are ignored — the server logs a warning for each one.

**Hot-reloadable:** proxy routes, static paths, auth config, rate limits,
middleware, logging, cache, CORS, security headers, transforms, fault injection.

**Not hot-reloadable (restart required):** `port`, `tls.cert/key`,
`tls.versions/ciphers`, `workers`, `backlog`, `global.admin.bind`.

---

### status

Print server status as JSON. Add `--upstream` to show a table of upstream health.

```bash
conduit status
conduit status --admin 127.0.0.1:2019

# Show upstream health table
conduit status --upstream
```

**Default output (JSON):**

```json
{
  "version": "1.0.0",
  "uptime_secs": 3600,
  "inflight": 42,
  "sites": 2
}
```

**`--upstream` output (table):**

```
URL                      Healthy  Latency     Ejected  5xx
──────────────────────────────────────────────────────────
✓ http://api-1:4000      ✓        12 ms       no       0
✗ http://api-2:4000      ✗        —           yes      5

1/2 upstreams healthy
```

| Flag           | Description                                       |
| -------------- | ------------------------------------------------- |
| `--admin ADDR` | Admin API address                                 |
| `--upstream`   | Show upstream health table instead of server JSON |

---

### shutdown

Gracefully stop the server.

```bash
conduit shutdown
conduit shutdown --admin 10.0.0.1:2019
```

Conduit stops accepting new connections, waits for all in-flight requests to
complete (up to `global.shutdownTimeoutSecs`), then exits.

---

### upstreams

Manage upstream targets at runtime. Changes are **in-memory only** and are
lost when `conduit reload` is run or the server restarts.

#### List upstreams

```bash
conduit upstreams
conduit upstreams --admin 127.0.0.1:2019
```

Prints all upstreams with health status, latency, and weight:

```json
[
  {
    "route": "/api",
    "url": "http://api-1:4000",
    "healthy": true,
    "latency_ms": 12,
    "weight": 1,
    "ejected": false
  }
]
```

#### Add an upstream

```bash
conduit upstreams add --route /api --target http://api-3:4000
conduit upstreams add --route /api --target http://api-3:4000 --weight 2
conduit upstreams add --route /api --target http://api-3:4000 --site api.example.com:443
```

| Flag           | Required | Description                                                                                       |
| -------------- | -------- | ------------------------------------------------------------------------------------------------- |
| `--route PATH` | ✅       | Route path (e.g. `/api`)                                                                          |
| `--target URL` | ✅       | Upstream URL to add                                                                               |
| `--weight N`   | —        | Weight for weighted-round-robin (default: 1)                                                      |
| `--site LABEL` | —        | Limit to a specific site (e.g. `api.example.com:443`). Omit to apply to all sites with this route |

#### Remove an upstream

```bash
conduit upstreams remove --route /api --target http://api-3:4000
conduit upstreams remove --route /api --target http://api-3:4000 --site api.example.com:443
```

| Flag           | Required | Description              |
| -------------- | -------- | ------------------------ |
| `--route PATH` | ✅       | Route path               |
| `--target URL` | ✅       | Upstream URL to remove   |
| `--site LABEL` | —        | Limit to a specific site |

#### Change upstream weight

Only effective when the route uses `strategy: weighted-round-robin`.

```bash
conduit upstreams weight --route /api --target http://api-1:4000 --weight 3
conduit upstreams weight --route /api --target http://api-2:4000 --weight 1
```

| Flag           | Required | Description              |
| -------------- | -------- | ------------------------ |
| `--route PATH` | ✅       | Route path               |
| `--target URL` | ✅       | Upstream URL             |
| `--weight N`   | ✅       | New weight value         |
| `--site LABEL` | —        | Limit to a specific site |

---

## Tooling

### completions

Generate shell completion scripts.

```bash
# Bash
conduit completions bash >> ~/.bashrc
source ~/.bashrc

# Zsh
conduit completions zsh >> ~/.zshrc

# Fish
conduit completions fish > ~/.config/fish/completions/conduit.fish

# PowerShell
conduit completions power-shell >> $PROFILE

# Elvish
conduit completions elvish >> ~/.config/elvish/rc.elv
```

Supported shells: `bash`, `zsh`, `fish`, `power-shell`, `elvish`.

Completions cover all subcommands, flags, and (where statically known) their
accepted values.

---

### man

Generate a `man(1)` page in roff format and write it to stdout.

```bash
conduit man > conduit.1
man ./conduit.1

# Install system-wide (Linux)
conduit man | gzip > /usr/share/man/man1/conduit.1.gz
mandb
```

---

## Environment variables

| Variable                  | Default          | Description                                                                                                                                                                          |
| ------------------------- | ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `RUST_LOG`                | `warn`           | Log level for the server process. Format: `error\|warn\|info\|debug\|trace` or per-crate: `conduit=debug,pingora=warn`                                                               |
| `CONDUIT_ADMIN`           | `127.0.0.1:2019` | Admin API address used by `reload`, `status`, `shutdown`, and `upstreams` commands                                                                                                   |
| `CONDUIT_ADMIN_TOKEN`     | —                | Bearer token sent with every Admin API request from the CLI. Set when the server has `global.admin.token` configured.                                                                |
| `CONDUIT_ACME_EXTRA_ROOT` | —                | Path to a PEM CA file trusted for ACME HTTP client. For CI environments using test ACME servers (e.g. [Pebble](https://github.com/letsencrypt/pebble)) with self-signed certificates |

Config files also support `$VAR` interpolation — any environment variable can
be referenced in field values:

```yaml
tls:
  cert: $TLS_CERT_PATH
  key: $TLS_KEY_PATH
apiKey:
  keys: ["$API_KEY_1", "$API_KEY_2"]
```

Unknown variables are left as-is (the literal `$VAR_NAME` string). Variables
are expanded at startup; hot-reload re-expands them from the current environment.

---

## Exit codes

| Code | Meaning                                                                                                             |
| ---- | ------------------------------------------------------------------------------------------------------------------- |
| `0`  | Success                                                                                                             |
| `1`  | Error — config parse failure, validation error, upstream probe failure, Admin API unreachable, or other fatal error |

The server process itself only exits with `1` on startup errors. Once running,
it exits cleanly (`0`) after a graceful shutdown triggered by `conduit shutdown`
or `SIGTERM`.

---

## Kubernetes CRD mode

> Requires `--features kubernetes` at compile time.

```bash
# Watch ConduitSite CRDs in a specific namespace
conduit --kubernetes-namespace default

# Watch all namespaces (requires cluster-wide RBAC)
conduit --kubernetes-namespace "*"
```

Instead of reading a config file, Conduit connects to the Kubernetes cluster
(via `KUBECONFIG` or in-cluster service account), reads all `ConduitSite`
custom resources, and starts serving. Changes to CRDs are hot-applied
automatically — no restart needed.

When `--kubernetes-namespace` is set, the `-c` flag is ignored.

See [deployment.md — ConduitSite CRD](deployment.md#conduitsite-crd----features-kubernetes)
for the CRD schema and `kubectl apply` instructions.

---

## Build features

Conduit uses compile-time feature flags to keep the default binary lean.
The **standard build** (`cargo build --release`) includes the core reverse proxy,
TLS, static files, rate limiting, basic/API-key auth, compression, hot-reload,
Prometheus metrics, health checks, and the Admin API — everything needed for most
production deployments. Optional capabilities with heavier dependencies are added
with `--features`.

```bash
# Standard build (minimal, fast, secure)
cargo build --release

# Single optional feature
cargo build --release --features jwt
cargo build --release --features rhai
cargo build --release --features acme

# Full build — every feature enabled
cargo build --release --features full

# Select combination
cargo build --release --features "jwt,rhai,redis"
```

### Feature overview

| Feature           | What it enables                                     | Dependencies added      |
| ----------------- | --------------------------------------------------- | ----------------------- |
| `jwt`             | JWT Bearer-token auth + JWKS URL                    | `jsonwebtoken`          |
| `consumers`       | Per-consumer credentials + rate limits              | —                       |
| `forward-auth`    | ForwardAuth subrequest middleware                   | —                       |
| `rhai`            | Rhai scripting middleware (`type: "script"`)        | `rhai`                  |
| `wasm`            | WASM plugin middleware (`type: "wasm"`)             | `wasmtime`              |
| `tcp`             | TCP passthrough proxy (`type: "tcp"` site)          | —                       |
| `upload`          | File upload handler (`upload:` site config)         | `multer`                |
| `redis`           | Redis-backed rate limiting & caching                | `redis`                 |
| `cache`           | Response caching (`proxy.*.cache`)                  | —                       |
| `disk-cache`      | Disk-backed cache store (`cache.store: "disk:/…"`)  | —                       |
| `acme`            | Auto-TLS / Let's Encrypt (`tls.acme`)               | `instant-acme`, `rcgen` |
| `fault-injection` | Fault injection for chaos testing                   | —                       |
| `otlp`            | OpenTelemetry OTLP tracing                          | `opentelemetry` stack   |
| `kubernetes`      | Kubernetes CRD config provider                      | `kube`, `k8s-openapi`   |
| `full`            | All of the above                                    | all of the above        |

When a feature is off but its config field is set, Conduit logs a warning at
startup and continues with that feature disabled (fail-open, no crash).

---

### `jwt` — JWT Bearer-token authentication

Enables `jwtAuth` site config. Supports HS256 (shared secret) and RS256/ES256
(remote JWKS URL). When disabled, `jwtAuth` config is ignored with a warning.

```yaml
jwtAuth:
  jwksUrl: "https://auth.example.com/.well-known/jwks.json"
  issuer: "https://auth.example.com/"
  audience: ["my-api"]
```

Dependencies: `jsonwebtoken = "9"`

---

### `rhai` — Rhai scripting middleware

Enables `type: "script"` middleware entries. Scripts run in a sandboxed Rhai
engine with resource limits (500 000 operations, 1 MiB string, 65 536 array).

```yaml
middleware:
  - type: script
    path: ./scripts/api-gate.rhai
    config: { api_key: "$SECRET" }
  - type: script
    phase: response
    path: ./scripts/response-enricher.rhai
```

Dependencies: `rhai = "1"` (sync feature)

---

### `upload` — File upload handler

Enables `upload:` site config for multipart file uploads. Starts a loopback
Axum server on a random port; the Pingora proxy forwards matching requests to it.

```yaml
upload:
  path: /files
  dir: ./uploads
  maxFileSizeBytes: 10485760   # 10 MB
  allowedMimeTypes: ["image/jpeg", "image/png", "application/pdf"]
```

Files are saved with UUID v4 names. The success response includes `name`,
`originalName`, `size`, and `mimeType`.

Dependencies: `multer = "3"`

---

### `cache` — Response caching

Enables `proxy.*.cache` route config for in-memory response caching via
Pingora's cache layer. Supports TTL, skip-paths, cookie exclusion, Vary headers,
stale-while-revalidate, and stale-if-error.

```yaml
proxy:
  /api:
    targets: ["http://backend:4000"]
    cache:
      store: memory
      ttlSecs: 60
      staleWhileRevalidateSecs: 300
```

For Redis-backed shared cache add `--features redis`.
For disk-backed persistent cache add `--features disk-cache`.

---

### `disk-cache` — Disk-backed cache

Enables `cache.store: "disk:/path"`. Cache entries are written atomically
(`.tmp` → rename) and survive restarts. Useful for large response bodies or
when Redis is not available.

```yaml
proxy:
  /assets:
    targets: ["http://assets:4000"]
    cache:
      store: "disk:/var/cache/conduit"
      ttlSecs: 86400   # 1 day
```

Requires `--features cache` (depends on `cache`).

---

### `acme` — Auto-TLS / Let's Encrypt

Enables `tls.acme` site config for automatic certificate provisioning via the
ACME protocol (Let's Encrypt). Certificates are fetched at startup, cached to
disk, and renewed automatically 30 days before expiry.

The domain is taken from the site's `host` field — no separate `domain:` field exists.

```yaml
host: api.example.com   # ← domain used for the certificate
port: 443
tls:
  acme:
    email: "admin@example.com"
    storage: "./certs"
    challenge: http-01
  httpRedirectPort: 80
```

Dependencies: `instant-acme = "0.8"`, `rcgen = "0.14"`

---

### `redis` — Redis-backed rate limiting and caching

Enables `rateLimit.store: "redis://..."` and `cache.store: "redis://..."`.
Falls back to in-memory on connection failure (fail-open, logged as warning).

```yaml
rateLimit:
  windowSecs: 60
  limit: 1000
  store: "redis://localhost:6379"
```

Dependencies: `redis = "1"` (tokio-comp + rustls features)

---

### `tcp` — TCP passthrough proxy

Enables `tcp:` site config for raw TCP forwarding (no HTTP parsing).
Useful for databases, MQTT, custom binary protocols.

```yaml
port: 5432
tcp:
  targets: ["db-primary:5432", "db-replica:5432"]
  strategy: round-robin
  connectTimeoutMs: 3000
```

---

### `consumers` — Consumer model

Enables `consumers:` site config for named API clients with per-consumer
credentials (API key, Basic Auth, JWT), rate limits, and custom upstream headers.
The identified consumer's username is injected as `X-Consumer-ID`.

```yaml
consumers:
  consumers:
    - username: mobile-app
      apiKey: "$MOBILE_KEY"
      rateLimit: { windowSecs: 60, limit: 500 }
      headers: { X-Tier: mobile }
    - username: partner
      basicAuth: { password: "$PARTNER_PASS" }
```

JWT consumers (V2/V3) additionally require `--features jwt`.

---

### `forward-auth` — ForwardAuth middleware

Enables `forwardAuth:` site config to delegate every authentication decision
to an external HTTP service. `2xx` = allow (inject response headers into
upstream request); `4xx`/`5xx` = deny; unreachable = fail closed.

```yaml
forwardAuth:
  url: "http://auth-service:9000/verify"
  requestHeaders: [Authorization, Cookie]
  responseHeaders: [X-User-ID, X-Role]
  timeoutMs: 3000
  skipPaths: [/__health__]
```

---

### `fault-injection` — Chaos testing

Enables `faultInjection:` site config. **Do not use in production.** Used
to test circuit-breaker, retry, and timeout behaviour by injecting artificial
delays and errors into a fraction of requests.

```yaml
faultInjection:
  abort:
    percent: 5      # 5% of requests return 503
    status: 503
    body: '{"error":"injected"}'
  delay:
    percent: 10     # 10% of requests are delayed
    ms: 500

---

### `otlp` — OpenTelemetry distributed tracing

Enables `global.otlp` configuration block. Conduit exports distributed traces
to any OpenTelemetry-compatible backend (Grafana Tempo, Jaeger, Honeycomb,
OpenTelemetry Collector) via gRPC OTLP.

```yaml
global:
  otlp:
    endpoint: "http://tempo:4317"
    serviceName: "my-api"
    sampleRate: 0.1 # sample 10 % in production
    timeoutMs: 5000
```

Each span includes: `method`, `path`, `status`, `duration_ms`, `upstream_url`,
`request_id`. 5xx responses set span status to `ERROR`.

When the binary is built **without** `--features otlp`, the `global.otlp`
config block is silently ignored — no error, no traces.

Dependencies added: `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`

---

### `wasm` — WebAssembly plugin middleware

Enables `type: "wasm"` entries in the `middleware` array. Plugins are
`.wasm` files that export `on_request() -> i32` and a `memory` memory export.

```yaml
middleware:
  - type: wasm
    path: ./plugins/my-plugin.wasm
```

Conduit uses Wasmtime as the WASM runtime. Plugins run in order alongside Rhai
scripts (`type: "script"`). Plugin failures are **fail-open** — if a plugin
panics or returns an error, Conduit logs a warning and continues processing.

Plugins may export two hooks:

| Export | Signature | Phase | Notes |
|--------|-----------|-------|-------|
| `on_request` | `() → i32` | Before upstream | **Required.** `0` = continue, `1` = abort |
| `on_response` | `(status: i32) → i32` | After upstream response | Optional. `0` = continue, `1` = replace response |

**17 host functions** are available across both phases: read/set/remove headers,
get URI/method/IP/request-id, list header names, set response body/status,
redirect, read plugin config, log.

See [`contrib/wasm/README.md`](../contrib/wasm/README.md) for the full ABI reference
and examples in Rust, C, and WAT.

When the binary is built **without** `--features wasm`, `type: "wasm"` entries
log a startup warning and are skipped (fail-open).

Dependencies added: `wasmtime` (Cranelift JIT)

---

### `kubernetes` — Kubernetes CRD config provider

Enables the `--kubernetes-namespace` startup flag. Conduit reads configuration
from `ConduitSite` custom resources instead of a file, and watches for changes.

```bash
conduit --kubernetes-namespace default
conduit --kubernetes-namespace "*"    # all namespaces
```

Each `ConduitSite` spec mirrors the Conduit JSON schema — any field valid in
`conduit.yaml` is valid in the CRD spec. Multiple resources in a namespace are
combined into a multi-site config.

Install the CRD before use:

```bash
kubectl apply -f contrib/k8s/conduitsite-crd.yaml
```

When the binary is built **without** `--features kubernetes`, the
`--kubernetes-namespace` flag does not exist in the binary.

Dependencies added: `kube` (runtime + derive), `k8s-openapi`, `schemars`, `futures`
