# @lopatnov/conduit

[![npm version](https://img.shields.io/npm/v/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![npm downloads](https://img.shields.io/npm/dt/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![License](https://img.shields.io/github/license/lopatnov/conduit)](https://github.com/lopatnov/conduit/blob/main/LICENSE)
[![GitHub stars](https://img.shields.io/github/stars/lopatnov/conduit)](https://github.com/lopatnov/conduit/stargazers)

**Conduit is a reverse proxy, API gateway and static file server written in Rust, built on
[Cloudflare Pingora](https://github.com/cloudflare/pingora).** You describe your sites, routes,
authentication, limits and caching in one YAML or JSON file and run one executable.

Put it in front of your apps to terminate TLS, route requests, authenticate and rate-limit
clients, cache responses and run your own scripted logic — or serve a single-page app and its API
from one port — without gluing together a proxy, an auth sidecar and a plugin system.

<p align="center">
  <img src="https://raw.githubusercontent.com/lopatnov/conduit/main/docs/img/architecture.svg" alt="Clients talk to Conduit over HTTP/1.1, HTTP/2, WebSocket or TCP. Inside Conduit, listeners feed an ordered set of guards and a router, which picks a handler: reverse proxy, TCP proxy, static files or upload. Handlers talk to your services and to files on disk. Redis and an auth service are optional." width="900">
</p>

This npm package is a convenience wrapper: it downloads the native Conduit binary for your
platform. The full project, source and documentation live on
[GitHub](https://github.com/lopatnov/conduit).

---

## Getting started

**No installation needed:**

```bash
npx @lopatnov/conduit init    # interactive setup wizard
npx @lopatnov/conduit         # start
```

**Install globally**, then just type `conduit`:

```bash
npm install -g @lopatnov/conduit
conduit init
conduit
```

> **How it works:** when the package is installed, a `postinstall` script downloads the native
> binary for your platform from
> [GitHub Releases](https://github.com/lopatnov/conduit/releases). Nothing is compiled. Node.js is
> needed for that download and for the small `conduit` launcher script that starts the binary;
> the server itself is the Rust executable and does not run on Node.js. Set
> `CONDUIT_SKIP_DOWNLOAD=1` to skip the download (for example when you provide the binary
> yourself). If the download fails, `npm install` still succeeds and `conduit` tells you the
> binary is missing.

---

## Which build you get

**Since 2.0.0 the npm package installs the `full` build**, so every optional capability is
included: JWT auth, consumers, forward auth, response caching (memory, Redis, disk), automatic
certificates from Let's Encrypt, Rhai scripting, WebAssembly plugins, TCP proxy, file upload,
Redis-backed rate limiting, OpenTelemetry tracing, fault injection and the Kubernetes provider.
There is nothing to download separately.

<p align="center">
  <img src="https://raw.githubusercontent.com/lopatnov/conduit/main/docs/img/build-profiles.svg" alt="Four build layers, each including the ones below it. Always on: routing, TLS, filters, basic auth, metrics and the Admin API. Default adds reverse proxy, static files, compression and hot reload. Standard adds JWT, consumers, forward auth, caching and ACME. Full adds scripting, WebAssembly, TCP proxy, upload, Redis, disk cache, fault injection, OpenTelemetry and Kubernetes." width="900">
</p>

(Through 1.x the package installed the smaller `standard` build. If you are pinned to a 1.x
version, see that version's page.)

If you want a smaller binary, for example for a container image, build from source with only the
features you need. `conduit features -c conduit.yaml` prints the features a config requires and a
ready-to-paste `cargo install` line; see
[docs/building.md](https://github.com/lopatnov/conduit/blob/main/docs/building.md) and the
[feature list](https://github.com/lopatnov/conduit/blob/main/docs/cli.md#build-features).

---

## Minimal config

Create `conduit.yaml` (or `conduit.json`):

```yaml
port: 3000
proxy:
  /api: "http://localhost:4000"
```

Run:

```bash
conduit
```

`GET /api/users` → `http://localhost:4000/api/users`. Check it with
`curl http://localhost:3000/__health__`, which answers `{"status":"ok"}`. To check a config
without starting the server, run `conduit validate`.

---

## Common recipes

### Serve static files

```yaml
port: 3000
static: ./dist
```

### Reverse proxy to a backend

```yaml
port: 3000
proxy: "http://localhost:4000"
```

### SPA + API (most common)

```yaml
port: 3000
static: ./dist
proxy:
  /api: "http://localhost:4000"
fallback:
  status: 200
  file: ./dist/index.html
```

### Dev server with hot reload

```yaml
port: 3000
logging: dev
hotReload: true
cors: true
static: ./src
proxy:
  /api: "http://localhost:4000"
fallback:
  status: 200
  file: ./src/index.html
```

### Load-balanced backend with health checks

```yaml
port: 8080
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
    retry:
      attempts: 3
      conditions: [connection_error, "5xx"]
```

### Production HTTPS with your own certificates

```yaml
port: 443
tls:
  cert: /etc/tls/fullchain.pem
  key: /etc/tls/privkey.pem
  httpRedirectPort: 80
http2: {}
securityHeaders: true
compression: true
static: ./dist
proxy:
  /api:
    targets: ["http://api1:4000", "http://api2:4000"]
    strategy: least-conn
    stripPrefix: true
rateLimit:
  windowSecs: 60
  limit: 200
healthCheck: true
metrics:
  path: /__metrics__
  token: "$METRICS_TOKEN"
```

### Multiple sites from one process

```yaml
global:
  admin:
    bind: "127.0.0.1:2019"

sites:
  - host: app.example.com
    port: 443
    tls:
      cert: "$CERT_PATH"
      key: "$KEY_PATH"
    static: ./dist
    proxy:
      /api: "http://api:4000"

  - host: admin.example.com
    port: 443
    tls:
      cert: "$CERT_PATH"
      key: "$KEY_PATH"
    basicAuth:
      users: { admin: "$ADMIN_PASS" }
      challenge: true
    static: ./admin-ui
```

More scenarios (JWT gateway, failover, circuit breaker, caching, security hardening,
Kubernetes) are in the
[recipes](https://github.com/lopatnov/conduit/blob/main/docs/recipes.md) and in the
[examples](https://github.com/lopatnov/conduit/tree/main/examples) directory.

---

## How a request flows

<p align="center">
  <img src="https://raw.githubusercontent.com/lopatnov/conduit/main/docs/img/request-pipeline.svg" alt="A request passes through seven stages in order: accept, gate, protect, auth, shape, serve and respond. A guard that rejects the request answers it immediately." width="900">
</p>

Guards run in a fixed order: a client blocked by the IP filter never reaches rate limiting or
auth, and a request that fails auth never reaches your scripts or your upstream.

---

## CLI reference

```text
conduit                       start the server (reads conduit.json, conduit.yaml or conduit.yml)
conduit -c <file>             use a specific config file (.yaml or .json)
conduit --version             print the version
conduit --help                show all options

conduit init [--yes]          setup wizard (--yes = non-interactive)
conduit validate              validate the config (exit 0 = OK, exit 1 = errors)
conduit features [--json]     which Cargo features this config needs
conduit probe                 HEAD each upstream and show a latency table
conduit fmt [--write]         pretty-print / normalise the config

conduit reload   [--admin ADDR]    hot-reload the config without a restart
conduit status   [--admin ADDR]    show uptime and in-flight requests
conduit status   [--admin ADDR] --upstream   show the upstream health table
conduit upstreams [--admin ADDR]   list upstream health and latency
conduit upstreams add    --route PATH --target URL [--weight N] [--site LABEL]
conduit upstreams remove --route PATH --target URL [--site LABEL]
conduit upstreams weight --route PATH --target URL --weight N [--site LABEL]
conduit shutdown [--admin ADDR]    graceful shutdown

conduit completions bash|zsh|fish|power-shell|elvish
conduit man                   generate a man page (roff)
```

Admin commands connect to `127.0.0.1:2019` by default. Override that with `--admin ADDR` or the
`CONDUIT_ADMIN` environment variable. They only work when the config enables the Admin API
(`global.admin.bind`). Changes made with `upstreams add|remove|weight` live in memory and are
dropped by the next reload. The complete reference is
[docs/cli.md](https://github.com/lopatnov/conduit/blob/main/docs/cli.md).

---

## What it does

| Area                  | Details                                                                                   |
| --------------------- | ----------------------------------------------------------------------------------------- |
| **Reverse proxy**     | Balancing (round-robin, least-connections, IP hash, consistent hash, power of two choices and more), health checks, outlier detection, circuit breaker, retries, sticky sessions, traffic mirroring |
| **Static files**      | ETag, Last-Modified, Range, optional pre-compressed `.br` / `.gz` files, SPA fallback     |
| **TLS**               | Your own certificates, HTTP→HTTPS redirect, mTLS client certificates                      |
| **Automatic TLS**     | Let's Encrypt through ACME: issue and renewal                                             |
| **HTTP/2**            | Negotiated through ALPN on TLS ports, optional h2c (cleartext), HTTP/2 to upstreams       |
| **Compression**       | gzip, Brotli, Zstd and deflate, with a content-type filter                                |
| **WebSocket**         | Upgrade requests are proxied                                                              |
| **Proxy cache**       | Memory, Redis or disk store; stale-while-revalidate, stale-if-error, request coalescing   |
| **IP filtering**      | CIDR allow and deny lists, `X-Forwarded-For` trust, runtime deny-list through the Admin API |
| **Rate limiting**     | Token bucket per client IP or header, bursts, per-route and per-consumer limits, optional Redis backend |
| **Auth**              | Basic, API key, JWT (HS256 with a shared secret; RS256/ES256 through a JWKS URL), forward auth, consumers |
| **CORS and security headers** | Origin allow-list, preflight, credentials mode; HSTS, CSP, X-Frame-Options, Referrer-Policy, allowed hosts |
| **Transforms**        | Set or remove request and response headers; use JWT claims in headers (`{{ jwt.sub }}`)   |
| **Scripting**         | Rhai scripts and WebAssembly plugins, in the request and response phases                  |
| **Hot reload**        | `conduit reload` or a config file watch; connections stay open                            |
| **Observability**     | Health endpoint with upstream status, Prometheus metrics, OpenTelemetry tracing, JSON access log |
| **File upload**       | `multipart/form-data` with generated filenames, a MIME allow-list and size limits         |
| **TCP proxy**         | Raw TCP passthrough for databases, SMTP and similar                                       |
| **Redirects**         | Named parameters (`:slug`) with 301, 302, 307 or 308                                      |
| **Routing**           | Virtual hosts; path glob, method, header regex, query and cookie predicates               |
| **Kubernetes**        | `ConduitSite` CRD config provider                                                         |

### Limits you should know about

- HTTP/3 (QUIC) is not supported. Clients connect over HTTP/1.1 and HTTP/2.
- TLS versions and cipher suites are not configurable (`tls.versions` and `tls.ciphers` are
  rejected by `conduit validate`).
- `port`, `tls.cert` / `tls.key`, `workers`, `global.shutdownTimeoutSecs` and
  `global.admin.bind` need a restart; everything else reloads in place.
- Conduit uses one worker thread unless you set `global.workers`.
- Rhai scripts and WASM plugins fail open: if one cannot be loaded or errors at run time, the
  problem is logged and the request continues. Do not make a script your only access check.

---

## Supported platforms

**This npm package** installs the full build for:

| Platform | Architecture          |
| -------- | --------------------- |
| Linux    | x86-64 (glibc), ARM64 (glibc) |
| macOS    | Intel, Apple Silicon  |
| Windows  | x86-64                |

Other platforms (Linux musl, Linux RISC-V 64) and the smaller `standard` build are available as
[release downloads](https://github.com/lopatnov/conduit/releases) and as Docker images
(`ghcr.io/lopatnov/conduit:latest` for standard, `:latest-full` for full), or you can build from
source:

```bash
cargo install lopatnov-conduit                      # default build
cargo install lopatnov-conduit --features standard  # + JWT, consumers, forward auth, cache, ACME
cargo install lopatnov-conduit --features full      # everything (what npm installs since 2.0.0)
```

---

## Links

- 📦 [npm package](https://www.npmjs.com/package/@lopatnov/conduit)
- 🦀 [crates.io package](https://crates.io/crates/lopatnov-conduit)
- 🐳 [Docker image](https://github.com/lopatnov/conduit/pkgs/container/conduit) (`ghcr.io/lopatnov/conduit`)
- 📖 [Full documentation](https://github.com/lopatnov/conduit/tree/main/docs)
- ⚙️ [Configuration reference](https://github.com/lopatnov/conduit/blob/main/docs/configuration.md)
- 🚀 [Deployment guide](https://github.com/lopatnov/conduit/blob/main/docs/deployment.md)
- 📊 [Benchmarks and the CI measurement setup](https://github.com/lopatnov/conduit/blob/main/docs/benchmarks.md)
- 🐛 [Report a bug](https://github.com/lopatnov/conduit/issues)
- 💬 [Discussions](https://github.com/lopatnov/conduit/discussions)

---

## Contributing

Contributions are welcome. Read
[CONTRIBUTING.md](https://github.com/lopatnov/conduit/blob/main/CONTRIBUTING.md) before opening a
pull request.

Bug reports → [GitHub Issues](https://github.com/lopatnov/conduit/issues).  
Security vulnerabilities → [GitHub Security Advisories](https://github.com/lopatnov/conduit/security/advisories).  
Found it useful? A ⭐ on GitHub helps others discover the project.

---

## License

[Apache 2.0](https://github.com/lopatnov/conduit/blob/main/LICENSE) ©
2024–2026 [Oleksandr Lopatnov](https://github.com/lopatnov)
