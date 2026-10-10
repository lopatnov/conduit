# Conduit

[![CI](https://github.com/lopatnov/conduit/actions/workflows/ci.yml/badge.svg)](https://github.com/lopatnov/conduit/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/lopatnov-conduit.svg)](https://crates.io/crates/lopatnov-conduit)
[![crates.io downloads](https://img.shields.io/crates/d/lopatnov-conduit.svg)](https://crates.io/crates/lopatnov-conduit)
[![npm version](https://img.shields.io/npm/v/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![npm downloads](https://img.shields.io/npm/dt/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![License](https://img.shields.io/badge/license-Apache_2.0-blue.svg)](LICENSE)

**Conduit is a reverse proxy, API gateway and static file server written in Rust, built on
[Cloudflare Pingora](https://github.com/cloudflare/pingora).** You describe your sites, routes,
authentication, limits and caching in one YAML or JSON file and run one executable. Conduit
terminates TLS, balances and protects your upstreams, serves a single-page app next to its API,
proxies raw TCP, lets you add your own logic with scripts and plugins, and picks up configuration
changes without dropping connections. It runs from a config file, from a Docker image, or on
Kubernetes, where every `ConduitSite` resource becomes a site and changes apply on their own.

<p align="center">
  <img src="docs/img/hero.svg" alt="Clients reach Conduit over HTTP/1.1, HTTP/2, WebSocket or TCP. Inside Conduit, listeners feed an ordered chain of guards and a router that picks a handler: reverse proxy, TCP proxy, static files or upload. Handlers use your services and files on disk. Configuration comes from a YAML or JSON file or from Kubernetes ConduitSite resources. Redis and an identity provider are optional." width="900">
</p>

```bash
npx @lopatnov/conduit init   # write a starter conduit.yaml
npx @lopatnov/conduit        # run it
```

Other ways to run it: `npm install -g @lopatnov/conduit` · [pre-built binaries](#pre-built-binaries) for
Linux, macOS and Windows · the Docker image `ghcr.io/lopatnov/conduit` · `cargo install lopatnov-conduit` ·
[Kubernetes](#kubernetes-one-conduitsite-per-site-needs-kubernetes) with `ConduitSite` resources.
Details: [Installation](#installation).

## Table of Contents

- [Quick start](#quick-start)
- [What Conduit does](#what-conduit-does)
- [How a request flows](#how-a-request-flows)
- [Features by example](#features-by-example), including
  [Kubernetes](#kubernetes-one-conduitsite-per-site-needs-kubernetes)
- [Installation](#installation)
- [Choose your build](#choose-your-build)
- [CLI commands](#cli-commands)
- [Configuration](#configuration)
- [Hot reload and the Admin API](#hot-reload-and-the-admin-api)
- [Benchmarks](#benchmarks)
- [Limits you should know about](#limits-you-should-know-about)
- [Editor integration (JSON Schema)](#editor-integration-json-schema)
- [Documentation](#documentation)
- [Contributing](#contributing)
- [License](#license)

## Quick start

```bash
conduit init          # interactive wizard
conduit init --yes    # non-interactive: accept the defaults and write conduit.yaml
conduit validate      # check the config
conduit               # start
```

A minimal hand-written config. YAML and JSON are equivalent:

```yaml
# conduit.yaml
port: 8080
static: ./dist
proxy:
  /api: http://localhost:4000
```

<p align="center">
  <img src="docs/img/one-port.svg" alt="The three-line config serves ./dist for GET / and GET /logo.png and proxies GET /api/users to http://localhost:4000/api/users." width="900">
</p>

```bash
curl http://localhost:8080/__health__
# {"status":"ok"}
```

**→ Ready-to-run configs:** [`examples/`](examples/) · **→ Every field:** [docs/configuration.md](docs/configuration.md)
· **→ Other ways to install:** [Installation](#installation)

## What Conduit does

<p align="center">
  <img src="docs/img/feature-map.svg" alt="Sixteen feature groups: routing; proxy and balancing; resilience; caching; static files; TCP and uploads; response shaping; TLS; authentication; traffic control; scripting; observability; operations; developer tools; packaging; Kubernetes. Items marked with an asterisk need an optional build feature." width="900">
</p>

Where each group is documented in detail:

| Group | Read |
| --- | --- |
| Routing, redirects, path rewrite | [Routes](docs/configuration.md#routes) · [Redirects](docs/configuration.md#redirects) · [URL rewriting](docs/configuration.md#url-rewriting) |
| Proxy and balancing | [Proxy](docs/configuration.md#proxy) · [Load balancing](docs/configuration.md#load-balancing) · [Sticky sessions](docs/configuration.md#sticky-sessions) · [Mirroring](docs/configuration.md#traffic-mirroring) · [Upstream TLS](docs/configuration.md#upstream-tls-verification) · [Connection pool](docs/configuration.md#connection-pool) |
| Resilience | [Health checks](docs/configuration.md#health-checks) · [Circuit breaker](docs/configuration.md#circuit-breaker) · [Retry](docs/configuration.md#retry) · [Outlier detection](docs/configuration.md#outlier-detection) |
| Caching | [Proxy cache](docs/configuration.md#proxy-cache) |
| Static files | [Static files](docs/configuration.md#static-files) · [Fallback](docs/configuration.md#fallback) |
| TCP and uploads | [TCP proxy](docs/configuration.md#tcp-proxy) · [Upload](docs/configuration.md#upload) |
| Response shaping | [Compression](docs/configuration.md#compression) · [Security headers](docs/configuration.md#security-headers) · [CORS](docs/configuration.md#cors) · [Error masking](docs/configuration.md#error-masking) · [Response transform](docs/configuration.md#request--response-transform) |
| TLS | [TLS / HTTPS](docs/configuration.md#tls--https) · [mTLS](docs/configuration.md#mtls--client-certificate-authentication) · [HTTP/2](docs/configuration.md#http2) |
| Authentication | [Basic](docs/configuration.md#basic-auth) · [API key](docs/configuration.md#api-key) · [JWT](docs/configuration.md#jwt-auth) · [Forward auth](docs/configuration.md#forward-auth) · [Consumers](docs/configuration.md#consumers) |
| Traffic control | [Rate limiting](docs/configuration.md#rate-limiting) · [Limits](docs/configuration.md#limits) · [Priority routing](docs/configuration.md#priority-routing) · [IP filter](docs/configuration.md#ip-filter) |
| Scripting | [Rhai guide](docs/rhai.md) · [WebAssembly guide](docs/wasm.md) |
| Observability | [Logging](docs/configuration.md#logging) · [Metrics](docs/configuration.md#metrics) · [OpenTelemetry](docs/configuration.md#opentelemetry-tracing) · [Response time](docs/configuration.md#response-time-header) · [Server-Timing](docs/configuration.md#server-timing-header) |
| Operations | [Hot reload](docs/configuration.md#hot-reload) · [Admin API](docs/admin.md) |
| Developer tools | [CLI](docs/cli.md) · [JSON Schema for editors](#editor-integration-json-schema) |
| Packaging and Kubernetes | [Deployment guide](docs/deployment.md) · [ConduitSite resources](docs/deployment.md#conduitsite-crd---features-kubernetes) · [Building](docs/building.md) |

An asterisk on the map means the feature is optional at build time. [Choose your
build](#choose-your-build) shows which build has what.

## How a request flows

<p align="center">
  <img src="docs/img/request-flow.svg" alt="A request passes through seven stages in order: accept, gate, protect, auth, shape, serve and respond. A guard that rejects the request answers it immediately with 403, 400, 429, 503 or 401. After routing, the proxy checks per-route limits and the circuit breaker, picks an upstream, rewrites the request and handles the response." width="900">
</p>

Guards run in a fixed order, so you can reason about what happens first: a client blocked by the IP
filter never reaches rate limiting or auth, and a request that fails auth never reaches your
scripts or your upstream. Routing then picks the first matching route, and a handler serves it.
`/__health__` is answered at the end of the gate stage, before the host check, limits and auth.

## Features by example

Each block is a complete config you can save as `conduit.yaml` and check with `conduit validate`.
Features marked *(needs `x`)* are optional at build time; see [Choose your build](#choose-your-build).

### Reverse proxy and load balancing

<p align="center">
  <img src="docs/img/balancing.svg" alt="A balancer picks an upstream with one of eight strategies. Unhealthy and ejected upstreams are skipped, upstreams at their connection cap are skipped, and a failed attempt is retried on another target within a retry budget." width="900">
</p>

```yaml
port: 8080
proxy:
  /api:
    targets:
      - http://api-1:4000
      - http://api-2:4000
      - http://api-3:4000
    strategy: least-conn               # also round-robin, p2c, ip-hash, consistent-hash …
    healthCheck: { path: /health, intervalSecs: 10 }
    maxConnectionsPerUpstream: 200     # 503 instead of queueing when every target is full
    retry: { attempts: 2, conditions: [connection_error, 5xx] }
outlierDetection: { consecutive5xx: 5 }   # eject a target that keeps failing real requests
```

**→ Details:** [Load balancing](docs/configuration.md#load-balancing) ·
[Health checks](docs/configuration.md#health-checks) ·
[Circuit breaker](docs/configuration.md#circuit-breaker) · [Retry](docs/configuration.md#retry)

### More proxy options

```yaml
port: 8080
proxy:
  /api:
    targets: [https://api-1.internal:8443, https://api-2.internal:8443]
    backup: https://api-standby.internal:8443    # used while every target is unhealthy
    upstreamTls: { verify: true, serverName: api.internal }   # verification is on by default; serverName adds an accepted name
    http2: true                                  # HTTP/2 to the upstream
    mirror: http://api-v2:4000                   # shadow copy of each request (headers only); the reply is dropped
    websocket: true                              # allow WebSocket upgrades on this route
```

The mirror receives a copy of the request headers, `Authorization` and cookies included, so mirror
only to a service you trust.
**→ Details:** [Backup targets](docs/configuration.md#proxy-route-field-reference) ·
[Upstream TLS](docs/configuration.md#upstream-tls-verification) ·
[Mirroring](docs/configuration.md#traffic-mirroring) ·
[Connection pool](docs/configuration.md#connection-pool)

### Static files and single-page apps

```yaml
port: 8080
static: ./dist
fallback: { file: ./dist/index.html, status: 200 }   # unknown paths get the app shell
compression: true
proxy:
  /api: http://localhost:4000
```

**→ Details:** [Static files](docs/configuration.md#static-files) ·
[Fallback](docs/configuration.md#fallback) · [Compression](docs/configuration.md#compression)

### Headers, compression and error masking

```yaml
port: 8080
static: ./dist
compression: true                  # br, zstd or gzip: static files, fallback pages and metrics
securityHeaders: true              # nosniff, frame, referrer and XSS headers; HSTS is opt-in
cors: { origins: ["https://app.example.com"] }
maskErrors: true                   # 5xx bodies from upstreams become a generic JSON error
responseTransform:
  setHeaders: { X-Served-By: conduit }
  removeHeaders: [Server]
proxy:
  /api: http://localhost:4000
```

**→ Details:** [Compression](docs/configuration.md#compression) ·
[Security headers](docs/configuration.md#security-headers) · [CORS](docs/configuration.md#cors) ·
[Error masking](docs/configuration.md#error-masking) ·
[Response transform](docs/configuration.md#request--response-transform)

### HTTPS with automatic certificates *(needs `acme`)*

```yaml
host: example.com
port: 443
http2: {}
tls:
  acme: { email: admin@example.com }   # Let's Encrypt; it validates over port 80
  httpRedirectPort: 80                 # plain HTTP is redirected to HTTPS
proxy: http://localhost:4000
```

Bring your own certificate instead with `tls: { cert: ./fullchain.pem, key: ./privkey.pem }`.
Conduit renews ACME certificates in the background and writes them to disk, but a running process
keeps serving the certificate it loaded at start, so restart it within 30 days of expiry (any
deploy does it).
**→ Details:** [TLS / HTTPS](docs/configuration.md#tls--https) ·
[mTLS](docs/configuration.md#mtls--client-certificate-authentication)

### Authentication

<p align="center">
  <img src="docs/img/security-layers.svg" alt="Four layers: network (IP filter, TLS and mTLS, allowed hosts), abuse control (request limits, rate limits, load shedding), identity (consumers, Basic auth, API keys, JWT, forward auth) and hardening (security headers, CORS, header hygiene, error masking)." width="900">
</p>

```yaml
port: 8080
jwtAuth:                               # needs jwt
  jwksUrl: https://auth.example.com/.well-known/jwks.json
  audience: [my-api]
requestTransform:
  setHeaders:
    X-User-ID: "{{ jwt.sub }}"         # pass a claim on to your service
proxy:
  /api: http://localhost:4000
```

Prefer something simpler? `basicAuth: { users: { alice: "$ALICE_PASSWORD" } }` and
`apiKey: { keys: ["$API_KEY"], header: X-API-Key }` work the same way, and `forwardAuth` hands the
decision to your own service. **→ Details:** [Basic](docs/configuration.md#basic-auth) ·
[API key](docs/configuration.md#api-key) · [JWT](docs/configuration.md#jwt-auth) ·
[Forward auth](docs/configuration.md#forward-auth) · [Consumers](docs/configuration.md#consumers)

### Rate limiting

```yaml
port: 8080
rateLimit: { windowSecs: 60, limit: 300 }          # per client IP, for the whole site
proxy:
  /api/payments:
    targets: [http://payments:4000]
    rateLimit: { windowSecs: 60, limit: 10 }       # a stricter limit for this route, also per client IP
```

Limits are counted per client IP unless you set `keyBy: "header:X-Name"`. A header is chosen by the
client and can be forged, so only key on one that a trusted proxy in front of Conduit sets. For
quotas per authenticated client use [consumers](docs/configuration.md#consumers). Add
`store: "redis://host:6379"` to share counters between instances *(needs `redis`)*.
**→ Details:** [Rate limiting](docs/configuration.md#rate-limiting)

### Caching *(needs `cache`)*

<p align="center">
  <img src="docs/img/caching.svg" alt="A request is looked up by host, scheme, path and query. A fresh hit is served from the store, a stale entry is served while one background fetch refreshes it, and a miss goes to the upstream and is stored." width="900">
</p>

```yaml
port: 8080
proxy:
  /api:
    targets: [http://backend:4000]
    cache:
      store: memory                    # or redis://… or disk:/path
      ttlSecs: 60
      staleWhileRevalidateSecs: 300    # serve a stale copy while refreshing in the background
      staleIfErrorSecs: 600            # and when the upstream is down
      skipIfCookie: true               # never cache requests that carry a cookie
```

**→ Details:** [Proxy cache](docs/configuration.md#proxy-cache)

### Your own logic: Rhai scripts and WebAssembly plugins

<p align="center">
  <img src="docs/img/middleware.svg" alt="Scripts and plugins run after the built-in guards and again on the response. Rhai needs no build step; WebAssembly plugins can be written in Rust, C, Go or AssemblyScript and can also read the client IP and change the headers sent upstream. Both fail open." width="900">
</p>

```yaml
port: 8080
middleware:
  - type: script                       # Rhai, needs rhai
    path: ./scripts/auth-check.rhai
  - type: wasm                         # WebAssembly, needs wasm
    path: ./plugins/my-plugin.wasm
proxy:
  /api: http://localhost:4000
```

```rhai
// scripts/auth-check.rhai: deny requests without an Authorization header
let token = request.header("Authorization");
if token == "" {
    response.status = 401;
    response.body   = "Unauthorized";
    return false;
}
true
```

Scripts and plugins fail open, so keep your real access checks in the auth guards.
**→ Guides:** [Rhai](docs/rhai.md) · [WebAssembly](docs/wasm.md)

### Observability

```yaml
sites:
  - port: 8080
    logging: json                      # structured access log; every request gets an X-Request-ID
    metrics: { path: /__metrics__, token: "$METRICS_TOKEN" }   # Prometheus
    responseTime: true                 # adds an X-Response-Time header (milliseconds)
    serverTiming: true                 # Server-Timing: total and upstream, for browser DevTools
    proxy: http://localhost:4000
global:
  otlp: { endpoint: "http://tempo:4317", serviceName: my-api, sampleRate: 0.1 }   # needs otlp
```

**→ Details:** [Logging](docs/configuration.md#logging) · [Metrics](docs/configuration.md#metrics) ·
[OpenTelemetry](docs/configuration.md#opentelemetry-tracing) ·
[Response time](docs/configuration.md#response-time-header) ·
[Server-Timing](docs/configuration.md#server-timing-header)

### Several sites in one process

```yaml
sites:
  - host: app.example.com
    port: 443
    tls: { acme: { email: admin@example.com } }
    static: ./dist
    proxy: { /api: http://api:4000 }

  - host: admin.example.com
    port: 443
    tls: { acme: { email: admin@example.com } }
    basicAuth: { users: { admin: "$ADMIN_PASSWORD" } }
    proxy: http://admin-backend:5000
```

### Kubernetes: one `ConduitSite` per site *(needs `kubernetes`)*

<p align="center">
  <img src="docs/img/kubernetes.svg" alt="You apply a ConduitSite resource with kubectl. The Kubernetes API stores it next to the other ConduitSite resources. Conduit watches the namespace, builds one configuration from every resource, checks it and swaps it in without a restart; an invalid update is rejected. Requests go to the services each site names." width="900">
</p>

Instead of a config file, Conduit can read its sites from Kubernetes custom resources. Each
`ConduitSite` is one virtual site, and its `spec` takes the same fields as a site in `conduit.yaml`.

```yaml
# site.yaml
apiVersion: conduit.io/v1
kind: ConduitSite
metadata:
  name: my-app
  namespace: default
spec:
  port: 8080
  host: app.example.com
  proxy:
    /api:
      targets: [http://my-svc:4000]
      healthCheck: { path: /health }
  rateLimit: { windowSecs: 60, limit: 500 }
```

```bash
kubectl apply -f contrib/k8s/conduitsite-crd.yaml    # once per cluster: the ConduitSite definition
kubectl apply -f site.yaml                           # add or change a site
conduit --kubernetes-namespace default               # in the Conduit pod; '*' watches all namespaces
```

Conduit watches the namespace, combines the resources into one config, checks it and swaps it in
without a restart; an update that fails validation is rejected and the running config stays. Use
the `:latest-full` image (or a build with `--features kubernetes`) and give the pod a service
account that may `get`, `list` and `watch` `conduitsites`. Write access to `ConduitSite` objects is
deploy access: `spec` takes any site field, file paths such as `static` and `upload.dir` included, so
grant it only to people who may deploy to the Conduit pod.
**→ CRD, RBAC and Deployment manifests:**
[docs/deployment.md](docs/deployment.md#conduitsite-crd---features-kubernetes)

### Raw TCP *(needs `tcp`)* and file upload *(needs `upload`)*

```yaml
sites:
  - port: 3306                          # raw passthrough with no auth layer: restrict who can reach this port
    tcp:
      targets: ["mysql-primary:3306", "mysql-replica:3306"]
      strategy: round-robin             # or random

  - port: 8080
    upload:
      path: /upload                     # multipart POST; files get generated names
      dir: ./uploads
      maxFileSizeBytes: 5242880
      allowedMimeTypes: [image/png, application/pdf]
```

Uploads are open to anyone who can reach the port unless the site also has an auth guard
(`basicAuth`, `apiKey` or `jwtAuth`).
**→ Details:** [TCP proxy](docs/configuration.md#tcp-proxy) · [Upload](docs/configuration.md#upload)

**→ More scenarios:** [docs/recipes.md](docs/recipes.md) covers HTTPS, load balancing, failover,
circuit breaker, caching, security hardening, observability and Kubernetes, and
[`examples/`](examples/) has a validated config for each.

## Installation

### npx — nothing to install

```bash
npx @lopatnov/conduit init      # generate config
npx @lopatnov/conduit           # start the server
npx @lopatnov/conduit validate  # validate config
```

### npm global

```bash
npm install -g @lopatnov/conduit
conduit validate
conduit
```

The npm package downloads the native binary for your platform from
[GitHub Releases](https://github.com/lopatnov/conduit/releases) when it is installed (set
`CONDUIT_SKIP_DOWNLOAD=1` to skip that). Node.js is only needed for the download and for the
`conduit` launcher script; the server itself is the Rust binary. Since 2.0.0 the package installs
the **full** build; platforms: Linux x64 and arm64 (glibc), macOS Intel and Apple Silicon,
Windows x64. See the [npm package page](npm/Readme.md) for details.

### Pre-built binaries

Download from [GitHub Releases](https://github.com/lopatnov/conduit/releases):

| Platform             | Standard                                    | Full                                             |
| -------------------- | ------------------------------------------- | ------------------------------------------------ |
| Linux x86-64         | `conduit-x86_64-unknown-linux-gnu.tar.gz`   | `conduit-x86_64-unknown-linux-gnu-full.tar.gz`   |
| Linux x86-64 musl    | `conduit-x86_64-unknown-linux-musl.tar.gz`  | `conduit-x86_64-unknown-linux-musl-full.tar.gz`  |
| Linux ARM64          | `conduit-aarch64-unknown-linux-gnu.tar.gz`  | `conduit-aarch64-unknown-linux-gnu-full.tar.gz`  |
| Linux RISC-V 64      | `conduit-riscv64gc-unknown-linux-gnu.tar.gz`| —                                                |
| macOS Intel          | `conduit-x86_64-apple-darwin.tar.gz`        | `conduit-x86_64-apple-darwin-full.tar.gz`        |
| macOS Apple Silicon  | `conduit-aarch64-apple-darwin.tar.gz`       | `conduit-aarch64-apple-darwin-full.tar.gz`       |
| Windows x86-64       | `conduit-x86_64-pc-windows-msvc.exe.zip`    | `conduit-x86_64-pc-windows-msvc-full.exe.zip`    |

```bash
# Linux x86-64, standard
curl -L https://github.com/lopatnov/conduit/releases/latest/download/conduit-x86_64-unknown-linux-gnu.tar.gz \
  | tar xz && ./conduit-x86_64-unknown-linux-gnu --version
```

Every release also carries `SHA256SUMS.txt` (it lists every file attached to the release,
archives included) and GitHub build-provenance attestations for the bare binaries, which are
attached next to the archives (`gh attestation verify <binary> --repo lopatnov/conduit`; the
archives themselves are covered by the checksums only). The musl builds are statically linked;
the glibc builds link the system C library dynamically.

### cargo install

```bash
cargo install lopatnov-conduit                       # default build
cargo install lopatnov-conduit --features standard   # + JWT, consumers, forward auth, cache, ACME
cargo install lopatnov-conduit --features full       # everything

# or pick the features you need
cargo install lopatnov-conduit --features "jwt,cache,rhai"        # default build + JWT auth, caching, Rhai scripts
cargo install lopatnov-conduit --features "tcp,redis,otlp"        # default build + TCP proxy, Redis, OpenTelemetry
cargo install lopatnov-conduit --no-default-features --features proxy          # reverse proxy only
cargo install lopatnov-conduit --no-default-features --features static-server  # static files only, no proxy
cargo install lopatnov-conduit --no-default-features --features gateway        # proxy, auth, cache, ACME; no static files
```

For build instructions, feature bundles, cross-compilation and troubleshooting see
**[docs/building.md](docs/building.md)**.

### Docker

```bash
docker pull ghcr.io/lopatnov/conduit:latest        # standard build
docker pull ghcr.io/lopatnov/conduit:latest-full   # full build

docker run -p 8080:8080 \
  -v $(pwd)/conduit.yaml:/etc/conduit/conduit.yaml:ro \
  ghcr.io/lopatnov/conduit:latest -c /etc/conduit/conduit.yaml
```

The images are `FROM scratch` (a static musl binary, no shell, no OS userland) and run as UID
65534. On Kubernetes, use the `:latest-full` image and `ConduitSite` resources:
[see the example above](#kubernetes-one-conduitsite-per-site-needs-kubernetes).
**→ docker-compose, systemd, Kubernetes and a production checklist:**
[docs/deployment.md](docs/deployment.md)

## Choose your build

<p align="center">
  <img src="docs/img/build-profiles.svg" alt="Four build layers, each including the ones below it. Always on: routing, TLS, filters, basic auth, metrics and the Admin API. Default adds reverse proxy, static files, compression and browser live reload. Standard adds JWT, consumers, forward auth, caching and ACME. Full adds scripting, WebAssembly, TCP proxy, upload, Redis, disk cache, fault injection, OpenTelemetry and Kubernetes." width="900">
</p>

| You install with                    | You get                                            |
| ----------------------------------- | -------------------------------------------------- |
| `npx` / `npm install -g`            | full                                               |
| GitHub Release archives             | standard, or full (files ending in `-full`)        |
| Docker                              | `:latest` is standard, `:latest-full` is full      |
| `cargo install lopatnov-conduit`    | the default build; add `--features …` for more     |

If your config uses a feature your binary was built without, `conduit validate` and the start-up
log say so instead of silently ignoring it. To go the other way and find out what a config
needs:

```console
$ conduit features -c conduit.yaml
required: proxy
build:    cargo install lopatnov-conduit --no-default-features --features proxy
bundle:   `--features gateway` also covers this configuration
```

### Optional features

| Feature           | What it enables                                                            |
| ----------------- | -------------------------------------------------------------------------- |
| `jwt`             | JWT Bearer-token auth and JWKS URLs (`jwtAuth`)                            |
| `consumers`       | Named API clients with per-consumer credentials and rate limits            |
| `forward-auth`    | Delegate auth to an external HTTP service (`forwardAuth`)                  |
| `rhai`            | Rhai scripting middleware (`type: "script"`)                               |
| `wasm`            | WebAssembly plugin middleware (`type: "wasm"`) through Wasmtime            |
| `tcp`             | Raw TCP proxy mode (`type: "tcp"` site)                                    |
| `upload`          | Multipart file upload handler (`upload:` site config)                      |
| `redis`           | Redis-backed rate limiting and caching                                     |
| `cache`           | Response caching (`proxy.*.cache`); implies `proxy`                        |
| `disk-cache`      | Disk-backed cache store (`cache.store: "disk:/path"`)                      |
| `acme`            | Automatic certificates from Let's Encrypt (`tls.acme`)                     |
| `fault-injection` | Fault injection for chaos testing                                          |
| `otlp`            | OpenTelemetry OTLP tracing (`global.otlp`)                                 |
| `tokio-metrics`   | `conduit_eventloop_lag_ms` Prometheus gauge (no config key)                |
| `kubernetes`      | Kubernetes CRD config provider (`--kubernetes-namespace`)                  |

Bundles: `standard` is `jwt` + `consumers` + `forward-auth` + `cache` + `acme` on top of the
default set; `full` is every feature above plus `standard`. `static-server` and `gateway` are
shorthands for `--no-default-features` builds: static serving without the proxy, or the proxy, auth,
cache and ACME stack without static files. `proxy`, `compression`, `static` and `hotreload` are on
by default.

## CLI commands

```text
conduit [-c FILE]                       start the server (default config: conduit.json, conduit.yaml or conduit.yml)
conduit --kubernetes-namespace NS       start from ConduitSite resources instead of a file ('*' = all namespaces; needs kubernetes)
conduit validate [-c FILE]              validate the config — exit 0 = ok, exit 1 = errors
conduit features [-c FILE] [--json]     which Cargo features this config needs
conduit fmt [-c FILE] [--write]         pretty-print / normalise the config
conduit init [--yes] [-o FILE]          setup wizard (--yes = non-interactive)
conduit probe [-c FILE]                 ping every configured upstream and show latency

conduit reload   [--admin ADDR]         hot-reload the config
conduit status   [--admin ADDR]         version, uptime, in-flight requests
conduit status   [--admin ADDR] --upstream   upstream health table (latency, ejected, 5xx)
conduit shutdown [--admin ADDR]         graceful shutdown
conduit upstreams [--admin ADDR]        list upstream health and latency
conduit upstreams add    --route PATH --target URL [--weight N]
conduit upstreams remove --route PATH --target URL
conduit upstreams weight --route PATH --target URL --weight N

conduit completions bash|zsh|fish|power-shell|elvish   shell completion script
conduit man                             generate a man page (roff)
```

`reload`, `status`, `shutdown` and `upstreams` talk to the [Admin API](#admin-api). Changes made
with `upstreams add|remove|weight` live in memory only and are dropped by the next reload.
Full flag reference and exit codes: **[docs/cli.md](docs/cli.md)**

## Configuration

Config is a single YAML or JSON file. Without `-c`, Conduit looks for `conduit.json`, then
`conduit.yaml`, then `conduit.yml` in the current directory.

String values can reference environment variables (`"$VAR"`), which are expanded at start-up and
on reload. Keep secrets there rather than in the file.

```yaml
# conduit.yaml — annotated overview of common fields
port: 443
host: example.com          # virtual host (omit for catch-all)

tls:
  acme:                    # automatic certificates (--features acme)
    email: admin@example.com

http2: {}                  # HTTP/2 settings (on TLS ports it is negotiated via ALPN anyway)
compression: true
securityHeaders: true

proxy:
  /api:
    targets:
      - http://api-1:4000
      - http://api-2:4000
    strategy: least-conn
    healthCheck: { path: /health }
    retry: { attempts: 2, conditions: [5xx, connection_error] }
    cache: { store: memory, ttlSecs: 60 }

rateLimit:
  windowSecs: 60
  limit: 300

jwtAuth:                   # --features jwt
  jwksUrl: https://auth.example.com/.well-known/jwks.json
  audience: [my-api]

logging: json
metrics: { path: /__metrics__, token: "$METRICS_TOKEN" }
healthCheck: true
```

**→ All fields with examples:** [docs/configuration.md](docs/configuration.md)  
**→ Ready-to-run configs:** [examples/](examples/)

## Hot reload and the Admin API

<p align="center">
  <img src="docs/img/operations.svg" alt="Edit the config, check it with conduit validate, apply it with conduit reload. Most settings apply live without dropping connections; the port, certificate files, worker count and admin bind need a restart and reload refuses them. While running, the CLI and the Admin API show status and upstream health, change targets in memory and shut the server down gracefully." width="900">
</p>

Changing the config does not mean restarting the server: edit the file, run `conduit validate`, then
`conduit reload`. Most settings apply live, without dropping connections. Settings that need a
restart (the port, certificate files, `global.workers`, `global.admin.bind`,
`global.shutdownTimeoutSecs`) are refused by reload with a 400 that names the field.

### Admin API

An optional management server, off unless you configure `global.admin.bind`. Bind it to loopback
(`127.0.0.1:2019`, which is also where the CLI commands look by default). Binding it to another
interface is possible, but only do that behind a VPN or an SSH tunnel and always with a token.

```yaml
# conduit.yaml — `global` sits next to `sites:`; in a single-site config it is ignored
global:
  admin:
    bind: "127.0.0.1:2019"
    token: "$ADMIN_TOKEN"    # Bearer token, strongly recommended

sites:
  - port: 8080
    proxy: http://localhost:4000
```

```bash
conduit reload                                     # hot-reload the config
conduit status                                     # uptime, version, in-flight count
conduit status --upstream                          # upstream health table
conduit upstreams add --route /api --target http://new-backend:4000

# or call the API directly (with a token configured, add -H "Authorization: Bearer $ADMIN_TOKEN")
curl http://localhost:2019/status
curl -X POST http://localhost:2019/reload
curl -X DELETE "http://localhost:2019/cache/purge?url=https://example.com/api/data"
curl -X POST http://localhost:2019/ip-deny -d '{"cidr":"1.2.3.0/24"}'
```

**→ All endpoints with request and response examples:** [docs/admin.md](docs/admin.md)

## Benchmarks

Every pull request runs a **Performance report** in CI that measures reverse-proxy passthrough
for the PR head and for its base commit, back to back on the same runner, and posts the
difference. It is a trend signal that catches regressions, not a lab measurement, so this README
quotes no throughput figure: GitHub-hosted runners land on different CPUs from one run to the
next, and throughput depends on the hardware and on `global.workers`. If you quote a number,
quote its setup too. This is the setup CI uses:

| | |
| --- | --- |
| Runner | GitHub-hosted `ubuntu-latest`: 4 vCPU, 16 GB RAM. The host CPU model differs between runs and is printed in every report. |
| Conduit | `cargo build --release` with default features (`lto = true`, `codegen-units = 1`, stripped), run with the default of **one worker thread** (`global.workers` unset) |
| Upstream | a small Go `net/http` server on port 4000 returning a fixed 22-byte JSON body ([source](docs/benchmarks.md#ci-performance-report-exact-setup)) |
| Load generator | [oha](https://github.com/hatoo/oha) 1.16.0: `oha -z 10s -c 50 --no-tui --output-format json http://127.0.0.1:8080/` |
| What is compared | PR head against PR base, built and measured in the same job |

The server configuration, as JSON (what CI writes) and as the equivalent YAML:

```json
{ "port": 8080, "proxy": "http://127.0.0.1:4000" }
```

```yaml
port: 8080
proxy: http://127.0.0.1:4000
```

Throughput grows with `global.workers`, up to what the machine can give. For scripts that
measure requests per second and CPU cost per request on your own hardware, see
[`scripts/bench/`](scripts/bench/README.md); [docs/benchmarks.md](docs/benchmarks.md) has the
step-by-step reproduction, the reason CI uses one worker, and how to compare Conduit with another
proxy fairly.

## Limits you should know about

- **HTTP/3 (QUIC) is not supported.** Clients connect over HTTP/1.1 and HTTP/2; HTTP/3 depends on
  support in Pingora.
- **TLS versions and cipher suites are not configurable.** `tls.versions` and `tls.ciphers` are
  rejected by `conduit validate` because the TLS layer offers no way to apply them.
- **Some settings need a restart:** `port`, `tls.cert` / `tls.key`, `workers`,
  `global.shutdownTimeoutSecs` and `global.admin.bind`. Everything else reloads in place.
- **Certificates are loaded at start.** ACME renewals and files written through the Admin API
  reach the disk but are only served after a restart, so restart before the loaded certificate
  expires.
- **One worker thread by default.** Set `global.workers` to use more cores.
- **Scripts and plugins fail open.** If a Rhai script or a WASM plugin cannot be loaded or errors
  at run time, the problem is logged and the request continues. Do not rely on a script or plugin
  as your only access check.
- **No PROXY protocol** support yet, on either the listening or the upstream side.

## Editor integration (JSON Schema)

Conduit ships a [JSON Schema](schema/conduit.schema.json) for completion, hover docs and inline
validation in JSON and YAML configs.

**VS Code — JSON: add one line to your config:**

```json
{
  "$schema": "https://raw.githubusercontent.com/lopatnov/conduit/main/schema/conduit.schema.json",
  "port": 3000
}
```

**VS Code — JSON: workspace-wide (all `conduit*.json` files):**

```json
// .vscode/settings.json
{
  "json.schemas": [{
    "fileMatch": ["conduit.json", "conduit.*.json"],
    "url": "https://raw.githubusercontent.com/lopatnov/conduit/main/schema/conduit.schema.json"
  }]
}
```

**VS Code — YAML: add to `.vscode/settings.json`** (requires the
[YAML extension](https://marketplace.visualstudio.com/items?itemName=redhat.vscode-yaml)):

```json
// .vscode/settings.json
{
  "yaml.schemas": {
    "https://raw.githubusercontent.com/lopatnov/conduit/main/schema/conduit.schema.json":
      ["conduit.yaml", "conduit.yml", "conduit.*.yaml"]
  }
}
```

**IntelliJ / WebStorm:** Settings → Languages & Frameworks → Schemas and DTDs → JSON Schema
Mappings → add the URL `https://raw.githubusercontent.com/lopatnov/conduit/main/schema/conduit.schema.json`
with the file pattern `conduit*.json, conduit*.yaml`.

## Documentation

Everything beyond this page lives in [`docs/`](docs/); this table says which file answers which
question.

| Read this                                | When you want to                                                  |
| ---------------------------------------- | ----------------------------------------------------------------- |
| [docs/configuration.md](docs/configuration.md) | look up any config field, in YAML and JSON                  |
| [docs/recipes.md](docs/recipes.md)       | start from a working config for a common scenario                 |
| [docs/cli.md](docs/cli.md)               | see every command, flag, exit code and build feature              |
| [docs/admin.md](docs/admin.md)           | manage a running server over the Admin API                        |
| [docs/deployment.md](docs/deployment.md) | run it in production: Docker, systemd, Kubernetes                 |
| [docs/building.md](docs/building.md)     | build from source, pick features, cross-compile                   |
| [docs/rhai.md](docs/rhai.md)             | write request and response logic as scripts                       |
| [docs/wasm.md](docs/wasm.md)             | write plugins in Rust, C, Go, AssemblyScript or Zig               |
| [docs/benchmarks.md](docs/benchmarks.md) | reproduce the CI measurement or run your own                      |
| [examples/](examples/)                   | copy a complete, validated config                                 |

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a PR.

- **Bug reports** → [GitHub Issues](https://github.com/lopatnov/conduit/issues)
- **Security vulnerabilities** → [GitHub Security Advisories](https://github.com/lopatnov/conduit/security/advisories) (not public issues)
- **Questions and ideas** → [GitHub Discussions](https://github.com/lopatnov/conduit/discussions)
- **Found it useful?** A star on GitHub helps others discover the project

## License

[Apache 2.0](LICENSE) © 2024–2026 [Oleksandr Lopatnov](https://github.com/lopatnov)
