# Changelog

All notable changes to Conduit are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

---

## [Unreleased]

---

## [1.1.0] — TBD

### Security

- Upgrade `pingora` and all `pingora-*` crates `0.8.0 → 0.8.1` — mitigates
  HTTP/2 Bomb (CVE-2026-47774 / RUSTSEC) by bounding the default H2 server
  header-list size to 64 KiB and limiting concurrent streams to 100.
- Upgrade `jsonwebtoken` `9.3.1 → 10.4.0` — fixes CVE-2026-25537
  (Type Confusion leading to potential authorization bypass).

### Changes

- Bump GitHub Actions in CI/CD workflows (Trivy, taiki-e/install-action,
  codeql-action, attest-build-provenance) via Dependabot.
- Fix release workflow artifact naming: standard and full builds for the same
  target now use distinct artifact names, ensuring all binaries appear in the
  GitHub Release.

---

## [1.0.0] — 2026-06-02

This release promotes Conduit to a stable, production-ready API gateway and
reverse proxy.  It adds authentication, scripting, observability, reliability,
and security features across every layer of the stack, and introduces a
compile-time feature-flag system so the binary stays lean for simple deployments.

### Security fixes

- **SSRF — proxy loop prevention** — Requests that would route back to Conduit
  itself are rejected with `421 Misdirected Request`.
- **SSRF — ForwardAuth URL validation** — `forwardAuth.url` is validated at
  startup; loopback / metadata CIDR targets are rejected.
- **Timing-safe credential comparison** — Basic Auth and API-key comparisons
  use `subtle::ConstantTimeEq` to eliminate timing side-channels.
- **X-Consumer-ID injection** — Header is stripped from incoming requests before
  the consumer pipeline runs to prevent spoofing.
- **X-Priority bypass** — Untrusted `X-Priority` header cannot raise effective
  priority above the configured route ceiling.
- **Cache poisoning via unkeyed headers** — Host normalisation and scheme
  derivation prevent cache-key collisions across virtual hosts.
- **Static-file symlink safety** — `O_NOFOLLOW` used on directory open;
  pre-compressed `.br`/`.gz` sidecar resolution rejects symlink traversal.
- **TOCTOU in static file handler** — Path resolved once and reused; no
  re-stat window between permission check and open.
- **Request smuggling — CRLF stripping** — `\r`/`\n` characters in upstream
  response header values are stripped before forwarding.
- **Blocking I/O in async context** — File I/O moved to `tokio::task::spawn_blocking`
  to prevent Tokio thread-pool starvation.
- **Query string redaction in logs** — `logging.stripQuery: true` removes
  query parameters from the logged path to prevent PII / token leakage.
- **reqwest connect timeout** — `connect_timeout` set on ForwardAuth and JWKS
  clients to bound latency under DNS failure.
- **WASM CPU limiting** — Wasmtime fuel consumption enforced per invocation;
  runaway plugins cannot monopolise worker threads.
- **Server header suppression** — `Server:` header from upstream is removed by
  default to avoid information disclosure.
- **Admin API loopback-only binding** — HTTP server for the Admin API binds
  exclusively to `127.0.0.1`; configuring a non-loopback address is rejected.
- **20+ additional hardening fixes** identified through systematic audit of
  200+ potential vulnerability patterns (cache, auth, retry, proxy, TLS layers).

### Added — Authentication & Authorisation

- **JWT Bearer token validation** (`--features jwt`) — HS256 (shared secret)
  and RS256/ES256 (JWKS URL).  `jwtAuth: { jwksUrl, issuer, audience, skipPaths }`.
  JWKS cached per-URL with background refresh.  60-second leeway for clock skew.
  `jsonwebtoken = "9"`.

- **Consumer model** (`--features consumers`) — Named API clients with
  individual credentials (API key, Basic Auth, JWT), per-consumer rate limits,
  and injected `X-Consumer-ID` header.  `consumers.sharedJwt` supports Auth0 /
  Cognito / Keycloak patterns with JWKS and `sub` claim identification.

- **Forward Auth** (`--features forward-auth`) — Delegate authentication to an
  external HTTP service.  2xx → allow + inject response headers; 4xx/5xx → deny;
  unreachable → fail closed.  `forwardAuth: { url, requestHeaders, responseHeaders,
  timeoutMs, skipPaths }`.

- **mTLS — client certificate authentication** — `tls.clientAuth: { ca, optional }`.
  Rustls `WebPkiClientVerifier`; CA bundle loaded from a PEM file.  Optional mode
  allows unauthenticated clients while still forwarding cert info.

- **Admin API Bearer token auth** — `global.admin.token` protects every Admin
  API endpoint; Conduit returns `401` for missing or invalid tokens.

- **Conditional error responses** — `401`/`403` responses honour `Accept`:
  `application/json` clients get `{"error":"…","status":N}`; others get an empty body.

### Added — Load Balancing & Routing

- **P2C — Power of Two Choices** (`loadBalance: p2c`) — O(1) pick via
  splitmix64 RNG; routes to the less-loaded of two randomly selected upstreams.
  Combines with Peak EWMA latency for latency-aware balancing.

- **Peak EWMA latency tracking** — Per-upstream `ewma_latency_us` (α = 0.1)
  updated passively on every request in `logging()`.  Used by P2C.

- **Sticky sessions** — `proxy.*.sticky.cookie`: consistent-hash keying on a
  named cookie value; first request sets the cookie, subsequent requests are
  pinned to the same upstream.

- **Outlier Detection** — `outlierDetection: { consecutive5xx, baseEjectionTimeSecs,
  maxEjectionTimeSecs, maxEjectionPercent }`.  Exponential ejection backoff.
  Maximum ejection percentage enforcement prevents full cluster removal.

- **Half-open circuit breaker** — When an ejection period expires the first
  request is allowed through as a probe.  Successful probe → full recovery +
  reset ejection count; failed probe → re-eject at next backoff level.

- **Circuit Breaker** — `healthCheck.maxConnectionsPerUpstream`: when _all_
  healthy upstreams reach the connection limit Conduit returns `503` immediately
  (`LocalHandler::Overloaded`) rather than queuing.  Works with all LB strategies.

- **Service Failover** — `proxy.*.backup`: traffic is routed to the backup URL
  when all primary upstreams are unhealthy.

- **Upstream slow start** — `healthCheck.slowStartSecs`: traffic to a recovered
  upstream ramps up linearly over the configured window.

- **Connection pool warmup** — `healthCheck.prewarmConnections` (max 8):
  Conduit sends HEAD requests at startup to pre-establish keepalive connections.

- **Header-based routing with regex** — `routes[].match.headers`: route on
  the presence or regex value of a request header.

- **Cookie-based routing** — `routes[].match.cookies`: route on cookie presence
  or exact value.

- **Query parameter routing** — `routes[].match.query`: route on query parameter
  presence or regex value.

- **Priority routing / load shedding** — `proxy.*.priority` (0–100) +
  `limits.priorityThreshold` (default `0.8`).  When `inflight / maxInflight ≥
  threshold`, routes with effective priority < 50 receive `503 Load Shedding`.
  Trusted callers can raise priority via the `X-Priority` request header.

### Added — Reliability

- **Inflight request limit** — `limits.maxInflightRequests`: enforced by
  `LimitsGuard` before any auth processing; returns `503` when exceeded.

- **Per-IP connection limit** — `limits.maxConnectionsPerIp`: returns `429`
  when a single client IP exceeds simultaneous open connections.

- **Request body buffering for retry** — `limits.maxBodyBufferBytes`: body
  chunks accumulated in `RequestCtx.body_buffer` and replayed on retry (linkerd2
  ReplayBody pattern).  Overflow sets `body_too_large = true` (retry skipped).

- **Retry budget** — `retry.budgetPercent`: soft limit on the fraction of active
  requests that may be retries; prevents retry storms under mass failure.

- **Per-try timeout** — `timeout.perTryMs`: independent deadline for each
  retry attempt.

- **Retry exponential jitter** — `retry.backoffJitter: true`: applies ±50%
  randomness to `backoffMs` so retries from concurrent failures spread out in
  time rather than hitting the upstream in a synchronised wave.

- **Traffic Mirroring** — `proxy.*.mirror`: fire-and-forget copy of every
  request to a shadow URL via `tokio::spawn` + reqwest.  Primary response is
  unaffected.  `X-Mirrored-From` header added to the shadow copy.

- **Stale-while-revalidate** (RFC 5861) — `cache.staleWhileRevalidateSecs` +
  `cache.staleIfErrorSecs`.  Stale responses served while a background fetch
  refreshes the cache; stale responses on upstream 5xx.

- **Cache thundering herd prevention** — Pingora `CacheLock` (16 shards, 10 s
  timeout): the first request on a cache miss takes a Write permit; all others
  wait for the Read permit from the same fetched copy.

### Added — Extensibility

- **Rhai scripting middleware** (`--features rhai`) — `type: "script"` in the
  middleware array.  Request phase: inspect/modify headers, abort with status.
  Response phase: `phase: "response"` — inspect upstream status and headers,
  set/remove response headers.  Resource limits: 500 000 operations, 1 MiB string,
  65 536 array elements.  Configurable per-plugin `config` map.

- **WASM plugin middleware** (`--features wasm`) — `type: "wasm"` via Wasmtime.
  17 host functions: read/write request headers, set response, get URI, get
  request ID, abort with redirect, log.  Optional `on_response(status) -> i32`
  export for response-phase plugins.  Per-plugin fuel limiting (CPU cap).
  Module cache.  Fail-open: plugins without expected exports are silently skipped.

- **Request / Response Header Transforms** — `requestTransform` / `responseTransform:
  { setHeaders, removeHeaders }`.  JWT template substitution: `{{ jwt.sub }}`,
  `{{ jwt.email }}`, `{{ jwt.<any-claim> }}` in `requestTransform.setHeaders`.

- **Fault Injection** (`--features fault-injection`) — `faultInjection: { abort:
  { percent, status, body }, delay: { percent, ms } }`.  splitmix64 RNG.
  Intended for chaos / resilience testing only; not for production.

- **Phase-ordered response pipeline** — `ResponseFilterChain` with six phases:
  CrlfProtection → InjectExtraHeaders → ResponseTransform → ResponseTime →
  RetryOnError → ErrorMask.  New response-phase behaviour is added as a new phase
  struct, not by editing `upstream_response_filter`.

### Added — Observability

- **OpenTelemetry OTLP distributed tracing** (`--features otlp`) —
  `global.otlp: { endpoint, serviceName, sampleRate, timeoutMs }`.  One span per
  request: method, path, status, duration, upstream URL, request ID.  5xx → span
  status ERROR.  Compatible with Grafana Tempo, Jaeger, Honeycomb.

- **Structured access log** — JSON log format now includes `request_id`
  (from `X-Request-ID`), `upstream` (selected upstream URL), and `upstream_ms`
  (time from request forwarded to upstream response received).

- **X-Request-ID injection** — `XRequestIdGuard` (first guard in chain):
  generates a UUID v4 if absent, forwards existing value.  Exposed in OTLP spans
  and access logs.

- **Per-upstream Prometheus metrics** — Three new metrics per upstream URL:
  `conduit_upstream_requests_total{upstream,status}` (counter),
  `conduit_upstream_latency_seconds{upstream}` (histogram),
  `conduit_upstream_active_connections{upstream}` (gauge).

- **Additional site-level metrics** — `conduit_active_connections` (gauge),
  `conduit_upstream_errors_total{route,status}` (counter),
  `conduit_retry_attempts_total{route,condition}` (counter),
  `conduit_rate_limit_rejected_total{site}` (counter),
  `conduit_cache_hits_total{route}` / `conduit_cache_misses_total{route}`.

- **Extended health endpoint** — `/__health__?includeUpstreams=true` (or
  `healthCheck.includeUpstreams: true`) returns per-upstream `latency_ms`,
  `ejected` status, and `consecutive_5xx` count.

- **`conduit status --upstream`** — Prints an upstream health table (URL,
  healthy, latency, ejected, 5xx count) sourced from `GET /upstreams`.

- **`logging.stripQuery`** — Removes query string from the logged path to
  prevent PII or token leakage in access logs.

### Added — Caching

- **Disk cache** (`--features disk-cache`) — `cache.store: "disk:/path"`.
  Atomic write (temp → rename).  Persists across restarts.

- **Redis cache** (`--features redis`) — `cache.store: "redis://..."` /
  `"rediss://..."` (TLS).  Shared across multiple Conduit instances.  Fail-open:
  unreachable Redis silently disables caching for that request.

- **Cache purge API** — `DELETE /cache/purge?url=<url>` on the Admin API.
  Calls Pingora `force_expire()` on the matching cache key.

- **RFC 7234 compliance** — `s-maxage` directive respected from upstream
  `Cache-Control`; `s-maxage=0`, `no-store`, `private` prevent caching.
  Configured `ttlSecs` caps the upstream-supplied TTL.

- **`cache.varyHeaders`** — Vary cache key by named request headers
  (`Accept-Language`, `Accept-Encoding`, etc.) for content-negotiated responses.

- **`compression.types`** — Content-Type prefix allowlist for on-the-fly
  compression; binary content (images, video, archives) excluded by default.

### Added — Networking

- **TCP proxy mode** (`--features tcp`) — `type: "tcp"` site with
  `tcp: { targets, strategy, connectTimeoutMs }`.  Bidirectional relay via
  `tokio::io::copy_bidirectional`.  Round-robin and random strategies.

- **Zstd compression** — `algorithms: [zstd]`; client preference respected via
  `Accept-Encoding` negotiation.

- **HTTP/2 cleartext (h2c)** — `http2.h2c: true`; allows HTTP/2 over plain TCP
  for internal service mesh use.

- **Keepalive request limit** — `limits.keepaliveRequestLimit`: close and
  recycle connections after N requests (equivalent to nginx `keepalive_requests`).

- **Upstream TLS verification** — `proxy.*.upstreamTls: { verify, serverName }`:
  controls certificate and hostname verification for backend HTTPS connections.

- **`X-Forwarded-Host`** — Injected alongside `X-Forwarded-For` and
  `X-Forwarded-Proto` in `upstream_request_filter`.

### Added — Admin API

- **`POST /certs/reload`** — Submit new PEM cert + key pair; Conduit validates
  the pair (rustls cert/key match check), writes atomically to the configured
  paths.  Full zero-downtime hot-swap awaits Pingora 0.9+.

- **`POST /ip-deny` / `DELETE /ip-deny`** — Add or remove CIDRs from the
  runtime deny-list without a config reload.  `IpGuard` reads the dynamic list
  on every request alongside the static `ipFilter.deny` config.

- **Admin API only starts when configured** — The HTTP server binds only when
  `global.admin` is present in config.  Background tasks (health checks,
  rate-limiter cleanup, hot-reload watcher) always run regardless.

### Added — Configuration

- **YAML config support** — Auto-detects `conduit.yaml` / `conduit.yml`;
  `from_yaml()` in `parse.rs`; env interpolation and version checks work
  identically to JSON.

- **Provider pattern** — `Provider` trait in `src/config/provider.rs`.
  `FileProvider`: one-shot load + inotify/kqueue auto-reload via `notify`.
  Delivers `AppConfig` on a channel; hot-swap is driven by the existing ArcSwap
  mechanism.

- **Kubernetes CRD provider** (`--features kubernetes`) — `ConduitSite` custom
  resource via `kube::CustomResource`.  `KubernetesProvider`: list + watch pattern.
  `--kubernetes-namespace` CLI flag; `"*"` watches all namespaces.
  CRD manifest: `contrib/k8s/conduitsite-crd.yaml`.

- **Feature flag startup warnings** — When a config field requires a feature
  that was not compiled in (e.g. `jwtAuth` without `--features jwt`), Conduit
  logs a `WARN` at startup and on every hot-reload.  The `/reload` response
  includes a `warnings: [...]` field.

- **`ipFilter.dryRun`** — Log IP-filter violations without enforcing them.
  Useful for auditing a deny list before enabling enforcement.

- **`rateLimit.dryRun`** — Log rate-limit violations without rejecting requests.

- **`healthCheck.unhealthyStatus`** — Status codes from the health-check probe
  that count as failures (e.g. `[429, 500, 502, 503, 504]`).

- **`healthCheck.unhealthyLatencyMs`** — Probe responses slower than this
  threshold count as failures even when the status code is 2xx.

- **`securityHeaders.permissionsPolicy`** — Sets `Permissions-Policy` header.

- **`securityHeaders.allowedHosts`** — Rejects requests with a `Host` header
  not in the allowlist with `421 Misdirected Request`.

- **`securityHeaders.hstsIncludeSubDomains` / `hstsPreload`** — Fine-grained
  HSTS directive control.

- **`limits.maxConnectionsPerIp`** — Per-client IP simultaneous connection cap.

- **`logging.stripQuery`** — Strip query string from access log path field.

### Added — Build system

- **14 optional compile-time features** — `jwt`, `consumers`, `forward-auth`,
  `rhai`, `wasm`, `tcp`, `upload`, `redis`, `cache`, `disk-cache`, `acme`,
  `fault-injection`, `otlp`, `kubernetes`.  Default build (`default = []`) is
  the minimal standard proxy.  `--features full` enables everything.  Binary
  size reduction: ~30% smaller standard build vs full build.

- **Two Docker image variants** — `:latest` (standard, ~14 MB, no optional
  features) and `:latest-full` (all 14 features).  Multi-stage musl build,
  `FROM scratch`, runs as UID 65534.

- **CI full-features builds** — GitHub Actions matrix now includes a
  `--features full` build alongside the standard build for both Linux and macOS.

### Added — CLI

- **`conduit init --yes`** — Non-interactive mode; accepts `--port`, `--proxy`,
  `--static`, `--tls` flags for scripted config generation.  Supports YAML output.

- **`conduit fmt`** — Preserves input format: YAML files stay YAML, JSON stays
  JSON; `--write` overwrites in place.

- **`conduit probe`** — Parallel HEAD requests to all configured upstreams;
  results sorted by URL with ✓/✗ status and latency.

### Added — Documentation

- `docs/admin.md` — Complete Admin API reference with request/response examples
  for all endpoints.
- `docs/rhai.md` — Rhai middleware development guide.
- `docs/wasm.md` — WASM plugin development guide with examples in Rust, C, and Go.
- `docs/configuration.md` — Comprehensive reference: all config fields documented,
  including 14 fields that were previously missing from the reference.
- `docs/recipes.md` — 30+ configuration recipes covering common deployment
  patterns (SPA+API, mTLS, Auth0 JWT gateway, circuit breaker, file upload, etc.).
- `docs/cli.md` — Build features overview table; per-feature usage sections.
- `examples/` — 40+ JSON and YAML config examples for every major feature.

### Changed

- **`conduit init`** — Now generates YAML by default; `--json` flag outputs JSON.

- **Hot vs cold reload classification** — Port, TLS cert/key/versions/ciphers,
  workers, backlog, admin address → cold reload (returns error).  Everything
  else → hot reload via ArcSwap without dropping connections.

- **`GET /status`** — Response enriched with runtime stats: inflight count,
  uptime, config path, feature flags enabled.

- **`GET /upstreams`** — Response includes `latency_ms`, `ejected` status,
  `consecutive_5xx`, `half_open` flag, `ewma_latency_us`, and `conn_count`
  per upstream entry.

- **`conduit validate`** — Now also calls `feature_warnings()` and includes
  warnings in output for configs that reference disabled features.

### Fixed

- `WeightedRoundRobin` targets validated as `WeightedTarget` objects, not strings.
- `FallbackConfig` does not accept a `redirect` field (was silently ignored before).
- `conduit fmt` with `--write` now handles concurrent reload correctly.
- TCP proxy port conflict detection added to `conduit validate`.

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

[Unreleased]: https://github.com/lopatnov/conduit/compare/v1.1.0...HEAD
[1.1.0]: https://github.com/lopatnov/conduit/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/lopatnov/conduit/compare/v0.3.0...v1.0.0
[0.3.0]: https://github.com/lopatnov/conduit/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/lopatnov/conduit/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/lopatnov/conduit/releases/tag/v0.1.0
