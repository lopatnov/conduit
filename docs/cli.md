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
  - [`otlp`](#otlp--opentelemetry-distributed-tracing)
  - [`wasm`](#wasm--webassembly-plugin-middleware)
  - [`kubernetes`](#kubernetes--kubernetes-crd-config-provider)

---

## Global options

These flags are accepted by every command.

| Flag | Short | Default | Description |
| ---- | ----- | ------- | ----------- |
| `--config FILE` | `-c` | `conduit.json` | Config file path |
| `--version` | `-V` | — | Print version and exit |
| `--help` | `-h` | — | Print help and exit |
| `--kubernetes-namespace NS` | — | — | Read config from Kubernetes `ConduitSite` CRDs instead of a file. `"*"` watches all namespaces. Requires `--features kubernetes`. See [Kubernetes CRD mode](#kubernetes-crd-mode). |

---

## Config file discovery

When no `-c` flag is given, Conduit resolves the config in this order:

1. `conduit.json` — if this file exists, use it
2. `conduit.yaml` — fallback if `conduit.json` is absent
3. `conduit.yml` — fallback if neither of the above exists

Auto-discovery only applies when the path is the default (`conduit.json`).
When you pass `-c some-other-name.json`, that exact path is used and there is
no fallback.

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
3. Starts the Admin API on `127.0.0.1:2019` (if `global.admin` is set)
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

| Setting | Default |
| ------- | ------- |
| format | yaml |
| port | 8080 |
| static | `./dist` (enabled) |
| proxy | disabled |
| TLS | disabled |
| health check | enabled |
| log format | dev |

**All flags:**

| Flag | Short | Description |
| ---- | ----- | ----------- |
| `--output FILE` | `-o` | Output file path (format inferred from extension) |
| `--yes` | `-y` | Non-interactive: accept all defaults, no prompts |
| `--format <yaml\|json>` | — | Output format (overrides extension inference) |
| `--port N` | — | Port number [default: 8080] |
| `--static-dir DIR` | — | Serve static files from `DIR` |
| `--no-static` | — | Disable static file serving |
| `--proxy URL` | — | Proxy requests to upstream `URL` |
| `--no-proxy` | — | Disable proxy |
| `--log <dev\|json\|combined\|none>` | — | Log format [default: dev] |
| `--no-health` | — | Disable `/__health__` endpoint |
| `--tls-cert FILE` | — | TLS certificate file (enables manual TLS) |
| `--tls-key FILE` | — | TLS private key file (required with `--tls-cert`) |
| `--tls-acme EMAIL` | — | ACME email (enables Let's Encrypt auto-TLS) |

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
- `https://` upstreams get a **TCP connect check** (not a full TLS handshake),
  so they appear as "connected" even with an invalid certificate.
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

When `global.admin.token` is set on the server, set it as a Bearer token:

```bash
export CONDUIT_ADMIN_TOKEN="my-secret-token"
# The admin commands automatically use $CONDUIT_ADMIN for the address.
# For the token, pass it via Authorization header directly with curl,
# or set it in your shell profile:
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

| Flag | Description |
| ---- | ----------- |
| `--admin ADDR` | Admin API address |
| `--upstream` | Show upstream health table instead of server JSON |

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

| Flag | Required | Description |
| ---- | -------- | ----------- |
| `--route PATH` | ✅ | Route path (e.g. `/api`) |
| `--target URL` | ✅ | Upstream URL to add |
| `--weight N` | — | Weight for weighted-round-robin (default: 1) |
| `--site LABEL` | — | Limit to a specific site (e.g. `api.example.com:443`). Omit to apply to all sites with this route |

#### Remove an upstream

```bash
conduit upstreams remove --route /api --target http://api-3:4000
conduit upstreams remove --route /api --target http://api-3:4000 --site api.example.com:443
```

| Flag | Required | Description |
| ---- | -------- | ----------- |
| `--route PATH` | ✅ | Route path |
| `--target URL` | ✅ | Upstream URL to remove |
| `--site LABEL` | — | Limit to a specific site |

#### Change upstream weight

Only effective when the route uses `strategy: weighted-round-robin`.

```bash
conduit upstreams weight --route /api --target http://api-1:4000 --weight 3
conduit upstreams weight --route /api --target http://api-2:4000 --weight 1
```

| Flag | Required | Description |
| ---- | -------- | ----------- |
| `--route PATH` | ✅ | Route path |
| `--target URL` | ✅ | Upstream URL |
| `--weight N`   | ✅ | New weight value |
| `--site LABEL` | — | Limit to a specific site |

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
conduit completions powershell >> $PROFILE

# Elvish
conduit completions elvish >> ~/.config/elvish/rc.elv
```

Supported shells: `bash`, `zsh`, `fish`, `powershell`, `elvish`.

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

| Variable                  | Default          | Description |
| ------------------------- | ---------------- | ----------- |
| `RUST_LOG`                | `warn`           | Log level for the server process. Format: `error\|warn\|info\|debug\|trace` or per-crate: `conduit=debug,pingora=warn` |
| `CONDUIT_ADMIN`           | `127.0.0.1:2019` | Admin API address used by `reload`, `status`, `shutdown`, and `upstreams` commands |
| `CONDUIT_ACME_EXTRA_ROOT` | —                | Path to a PEM CA file trusted for ACME HTTP client. For CI environments using test ACME servers (e.g. [Pebble](https://github.com/letsencrypt/pebble)) with self-signed certificates |

Config files also support `$VAR` interpolation — any environment variable can
be referenced in field values:

```yaml
tls:
  cert: $TLS_CERT_PATH
  key:  $TLS_KEY_PATH
apiKey:
  keys: ["$API_KEY_1", "$API_KEY_2"]
```

Unknown variables are left as-is (the literal `$VAR_NAME` string). Variables
are expanded at startup; hot-reload re-expands them from the current environment.

---

## Exit codes

| Code | Meaning |
| ---- | ------- |
| `0`  | Success |
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

Conduit ships three optional compile-time features. The default binary (`cargo
build --release`) includes none of them; enable the ones you need:

```bash
# Single feature
cargo build --release --features otlp
cargo build --release --features wasm
cargo build --release --features kubernetes

# Multiple features
cargo build --release --features otlp,wasm,kubernetes
```

### `otlp` — OpenTelemetry distributed tracing

Enables `global.otlp` configuration block. Conduit exports distributed traces
to any OpenTelemetry-compatible backend (Grafana Tempo, Jaeger, Honeycomb,
OpenTelemetry Collector) via gRPC OTLP.

```yaml
global:
  otlp:
    endpoint: "http://tempo:4317"
    serviceName: "my-api"
    sampleRate: 0.1       # sample 10 % in production
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
panics or returns an error, Conduit logs the error and continues processing.

**17 host functions** are available: read/set/remove headers, get URI/method,
list header names, set response, redirect, get request ID, log.

When the binary is built **without** `--features wasm`, `type: "wasm"` entries
cause a validation error on startup.

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
