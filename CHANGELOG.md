# Changelog

All notable changes to Conduit are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

---

## [Unreleased]

---

## [0.3.0] — 2026-05-26

### Added

- **Two-level load balancing (`groups`)** — Route to named upstream groups via an outer
  `groupStrategy`, then distribute within each group via its own `strategy`. Enables
  geographic (region-based), tiered (canary/stable), or any topology that benefits from
  a two-level hierarchy. Supports all seven load-balancing strategies at both levels.

- **Path rewrite (`rewrite`)** — Regex-based path transformation rules applied after
  `stripPrefix`. First matching rule wins. Capture groups (`$1`, `$2`, …) are supported.

- **Advanced route table (`routes`)** — Explicit ordered route array with full match
  criteria: glob path, HTTP method list, header regex, query regex. Backward-compatible
  with existing top-level `proxy` and `static` fields (auto-normalized at parse time).

- **Shell completions** — `conduit completions <bash|zsh|fish|powershell|elvish>` prints
  completion script for the given shell.

- **Man page** — `conduit man` prints a troff-formatted man page to stdout.
  Pipe to `man -l -` or install with `conduit man > /usr/share/man/man1/conduit.1`.

- **`GET /upstreams` enriched** — Admin API response now includes a `routes` array
  showing per-route strategy, target weights, health, latency, and runtime-override flag.

- **Contrib assets** — `contrib/Dockerfile` (multi-stage musl + `FROM scratch`),
  `contrib/docker-compose.yml` (conduit + Node.js backend example),
  `contrib/conduit.service` (hardened systemd unit with `ProtectSystem`, `NoNewPrivileges`).

- **New config examples** — `examples/path-rewrite.json`, `examples/upstream-groups.json`.

- **JSON Schema** — Added `RouteConfig`, `MatchConfig`, `UpstreamGroup`, `RewriteRule`
  definitions; `routes` field on `SiteConfig`; `rewrite`/`groups`/`groupStrategy` on
  `ProxyRouteConfig`; `targets` no longer required when `groups` is set.

### Changed

- `ProxyRouteConfig.targets` — No longer required in the JSON config when `groups` is
  configured. Previously the absence of `targets` caused a parse error.

- `conduit validate` — Empty `targets` is now allowed when `groups` is set. Each group
  is validated to contain at least one target.

---

## [0.2.0] — 2026-05-24

### Added

- **Prometheus metrics** (`/__metrics__`) — `conduit_requests_total` (CounterVec) and
  `conduit_request_duration_seconds` (HistogramVec). Optional Bearer token auth.

- **Upstream health checks** — Background HTTP probes with `unhealthyThreshold` /
  `healthyThreshold`. Unhealthy upstreams are excluded from load balancing.

- **`least-conn` strategy** — Tracks active connections per upstream with atomic counters.

- **`random` strategy** — Uniform random upstream selection.

- **Dynamic upstream management** — `POST /upstreams/add|remove|weight` (Admin API) and
  matching `conduit upstreams add/remove/weight` CLI subcommands. Changes survive until
  `conduit reload`.

- **Proxy cache** — In-memory response cache via `pingora-cache`. Configurable TTL,
  `skipIfCookie`, `skipPaths`, allowed HTTP methods. CVE-2026-2836 mitigated via custom
  cache key (host + scheme + path + query).

- **Hot config reload** — `POST /reload` (Admin API) / `conduit reload`. Hot fields
  reload without dropping connections; cold fields (port, TLS cert, workers) return an
  error listing what changed.

- **Auto-TLS via ACME** — `tls.acme` field; `instant-acme` + `rcgen` + HTTP-01
  challenge handler. Background renewal 30 days before expiry.

- **File upload** — Axum loopback server on `127.0.0.1:0`. UUID filenames, MIME
  allowlist, per-file and total size limits.

- **Browser hot-reload** — SSE endpoint + `notify` file watcher + debounce.
  Injects `/__hot-reload__/client.js` automatically.

- **Pre-compressed static files** — When `staticOptions.preCompressed: true`, Conduit
  serves `.br` / `.gz` sidecar files without on-the-fly compression.

- **SNI / multi-cert TLS** — Multiple TLS certificates per server via SNI.

- **WebSocket proxying** — Transparent `Connection: Upgrade` / `101 Switching Protocols`
  tunnel through the proxy layer.

- **CORS** — Preflight `OPTIONS` handling, per-origin response headers, credentials mode,
  `Vary: Origin`, configurable max age.

- **Security headers** — X-Content-Type-Options, X-Frame-Options, Referrer-Policy,
  X-XSS-Protection; HSTS and CSP via object form.

- **`conduit init`** — Interactive wizard (dialoguer) that generates a starter config.

- **`conduit probe`** — HEAD each configured upstream and display a latency table with
  `indicatif` progress bar.

### Changed

- `GET /upstreams` now returns live health status, latency, and inflight connection count
  for every upstream tracked by the health registry.

---

## [0.1.0] — 2026-05-23

### Added

- **Core proxy pipeline** — Pingora-based HTTP/1.1 + HTTP/2 reverse proxy with full
  request/response filter pipeline.

- **Static file server** — ETag, Last-Modified, Cache-Control, Range requests, dotfile
  control, `index` file list.

- **TLS** — rustls via Pingora; `tls.httpRedirectPort` for HTTP → HTTPS redirect.

- **IP filtering** — CIDR allowlist / denylist, `trustProxy` for `X-Forwarded-For`.

- **Request limits** — `maxBodyBytes` (413), `maxHeaderBytes` (431), `timeoutSecs`.

- **Rate limiting** — Token-bucket algorithm, per-IP or per-header keying, path exclusions.

- **Basic Auth** — RFC 7617 `Authorization: Basic`, WWW-Authenticate challenge.

- **API key auth** — Custom header (`X-API-Key` default), path exclusions.

- **Redirect rules** — `:param` capture, 301/302/307/308, query string preserved.

- **Virtual hosting** — `host` field, catch-all `*`, duplicate detection.

- **Admin API** (`127.0.0.1:2019`) — `GET /status`, `POST /reload`, `POST /shutdown`,
  `GET /upstreams`.

- **Access logging** — five formats: `combined`, `common`, `dev`, `short`, `json`.
  File output with atomic switch on reload.

- **Compression** — on-the-fly gzip and Brotli (`Accept-Encoding`); streaming
  one-chunk-ahead buffering; Range requests bypass compression.

- **X-Response-Time** — millisecond precision, configurable decimal digits.

- **Proxy retries** — configurable attempts, `connection_error` / `5xx` / `timeout`
  conditions, backoff in milliseconds.

- **`conduit validate`** — exits 0 when config is valid, 1 with error list otherwise.

- **`conduit fmt`** — pretty-prints the config to stdout; `--write` overwrites in place.

- **JSON Schema** (`schema/conduit.schema.json`) — covers the full config surface.

- **Config examples** — `minimal.json`, `spa-with-api.json`, `multi-site.json`,
  `tls-h2.json`, `tls-acme.json`, `load-balanced.json`, `with-cache.json`,
  `dev-hot-reload.json`.

- **CI** — GitHub Actions matrix: ubuntu / macos / windows, fmt + clippy + test.

- **Release pipeline** — `cross`-compiled binaries for six targets; Docker image
  (musl + `FROM scratch`); npm wrapper (`npx conduit`); crates.io publish.

[Unreleased]: https://github.com/lopatnov/conduit/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/lopatnov/conduit/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/lopatnov/conduit/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/lopatnov/conduit/releases/tag/v0.1.0
