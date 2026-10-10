# Changelog

All notable changes to Conduit are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

---

## [Unreleased]

### Security

- **Two kid-less keys in a JWKS are no longer collapsed into one** (issue #351). The second used to
  replace the first under the same synthesized id, so a kid-less token was checked against whichever
  key survived instead of being rejected as ambiguous.
- **A directory symlink inside a static path is no longer followed** (issue #400, CWE-59). Only the
  last path component was checked, so `assets/secret.txt` with `assets -> /etc` read outside the
  static root. Every directory between the root and the file is now checked.
- **The `Host` header must carry a numeric port, or none** (issue #474). `allowed.example:80@evil.com`
  used to pass `allowedHosts` and site routing as `allowed.example` while the value echoed into
  `X-Forwarded-Host` named another host; it now matches nothing.
- **`POST /certs/reload` keeps the key file's mode** (issue #481). Rotating a `0600` key no longer
  leaves a world-readable one, and the temporary file is created exclusively under an unpredictable
  name instead of following a planted `<path>.tmp` symlink.

### Fixed

- `Range` requests honour `If-Range` (issue #402, RFC 9110 §13.1.5): a stale validator now gets `200`
  with the whole file instead of a `206` splice of two versions.
- A route with `groups` no longer gets a false "slowStartSecs is ignored on this route" warning from a
  leftover `strategy`/`sticky` it never reads (issue #483).
- The log targets in `docs/rhai.md`, `docs/wasm.md` and `docs/cli.md` match the crates that emit them
  (issue #476).
- The Admin API and the metrics endpoint accept `bearer`/`BEARER` as well as `Bearer` (issue #484,
  RFC 9110 §11.1); the token itself is still compared exactly and in constant time.
- **WASM `on_response` no longer leaks the replacement body as headers** (issue #379). A plugin that
  called `conduit_set_response_body` there got two internal `x-conduit-wasm-body-*` headers on the
  client response, one carrying the whole body in base64. They are gone. The body itself is still
  **not** replaced in the response phase; Conduit logs one warning and `docs/wasm.md` says so.
- `docs/wasm.md` states what `conduit_get_header_names`/`_count` really return: distinct names, no
  guaranteed order, one value per repeated header (issue #380).
- Failing over to a route's `backup` upstream keeps the route's own settings (issue #417): timeouts,
  pool, HTTP/2, cache, WebSocket permission, passive-health thresholds, strip/rewrite/mirror/upstream
  TLS. A `websocket: true` route no longer answers 502 to upgrades after it fails over.
- The Kubernetes provider rebuilds the config once per resync instead of on every `Init`/`InitApply`
  event (issue #408), which cut M+2 list-and-rebuild cycles to one at start-up and watch recovery.

### Changed

- **JWT verification now uses the `aws-lc-rs` crypto backend instead of the pure-Rust `rsa` stack.**
  This removes the `rsa` crate (CVE-2023-49092, "Marvin", no upstream fix) from `Cargo.lock`; Conduit
  only ever verified signatures with public keys, so it was not exploitable. The supported algorithms are
  unchanged (HS256 for `secret`; RS256/384/512 and ES256/384 for `jwksUrl`). **RSA keys must be 2048 to
  8192 bits**: the `aws-lc-rs` verifier rejects a JWKS or PEM key outside that range (fails closed)
  instead of verifying with it.

---

## [2.0.0] — 2026-10-03

Conduit 2.0 splits the single `lopatnov-conduit` crate into a Cargo workspace
of one crate per feature (issue #114), so a build only compiles the code and
dependencies a chosen feature set actually needs — no behavior, config shape,
route, or CLI flag changed by the split itself. Issue #148 wires up real
multi-crate publishing: every `lopatnov-conduit-*` member crate is
`cargo publish --workspace`-able in dependency order (native cargo, not
`cargo-workspaces`/`release-plz`), pinned at `2.0.0` in lockstep, with its own
MSRV (`rust-version`, 1.89 workspace-wide / 1.96 for `conduit-plugin-wasm`
which needs wasmtime 49+); CI proves the publish would succeed
(`workspace-publish-dryrun`) on every PR. The security, correctness, and
behavior fixes below landed alongside the migration and apply to 2.0.0
regardless of which feature crates a given build pulls in.

### Changed

- **The npm package (`@lopatnov/conduit`) now installs the `full` feature-bundle
  binary instead of `standard`** (issue #239). Redis-backed rate limiting/caching,
  WASM plugin middleware, Rhai scripting, OpenTelemetry OTLP tracing, and TCP
  proxy mode all work immediately after `npm install` — no separate full-binary
  download needed. The download is correspondingly larger. A smaller binary is
  still available by building from source (`cargo install lopatnov-conduit
  --features standard`). This is npm-specific: the unsuffixed GitHub Release
  assets and the default `:latest`/`:2.0.0` GHCR Docker image are unchanged and
  still ship the `standard` bundle — only the `-full`-suffixed release assets
  and `:latest-full` Docker tag match what npm now installs.

### Security

- **`global.admin.token: ""` is now a validation error** (issue #480). The Admin API compares the
  bearer token in constant time, and a request with no `Authorization` header is an empty string,
  so an empty configured token authenticated every request — which is what an unresolved
  `$ADMIN_TOKEN` expands to. `metrics.token: ""` was already rejected for the same reason; omit the
  field to leave the Admin API unauthenticated.
- **A Redis URL whose password contains a raw `/` is no longer logged with the password.**
  Credentials are redacted before a Redis URL is logged, but the search for the `user:password@`
  part stopped at the first `/`, so in `redis://alice:pa/ss@host:6379` the `@` looked like part of
  the path and the URL was printed as it was — as were `redis://alice:/pw@host` and
  `redis://default:1234/abc@host`, and a password holding both `@` and `/` was cut in the middle.
  Everything before the last `@` is now treated as credentials, since a Redis URL's path
  (`/` or `/<db-number>`) never holds an `@`; a malformed URL such as `redis://host:6379/db@1` is
  therefore printed as `redis://***@1`. (The redaction helper is now shared by every crate that
  prints config values.)
- **The vulnerable `protobuf 2.28.0` (RUSTSEC-2024-0437 / CVE-2025-53605) is
  gone from the dependency tree.** It was pulled in unconditionally by
  `pingora-core 0.8` through `prometheus 0.13`; Pingora 0.9 no longer depends
  on `prometheus` from `pingora-core`, so only Conduit's own `prometheus 0.14`
  (with `protobuf 3.7.2`) remains and the `cargo-audit` ignore for it is
  removed. The unmaintained `daemonize` crate (RUSTSEC-2025-0069) is replaced
  by `daemonix` in the same upgrade, so its OSV ignore is removed as well.
- **Pingora 0.9 hardening applies to every proxied request** — stricter
  request-target and authority validation, hop-by-hop upstream header
  sanitisation (see *Changed*), and bounded default HTTP/2 server limits.
- **Per-route and per-consumer rate limiting no longer bypass the shared
  memory-exhaustion cap.** `rateLimit` at the site level has always refused
  to create more than 100,000 distinct token buckets, to stop an attacker
  sending unbounded unique `keyBy: "header:X-Name"` values from exhausting
  memory. Per-route and per-consumer rate limits shared the same underlying
  map but bypassed that cap entirely — confirmed as a real DoS vector on the
  documented usage pattern. All three layers (plus the Redis fallback path)
  now share one capacity-checked admission point.
- **Per-route `rateLimit` is now validated at config-load time.** Previously
  `windowSecs`/`limit`/`algorithm`/`keyBy`/`store` on a per-route rate limit
  were parsed but never checked — a malformed `keyBy: "header:bad name"`
  silently collapsed every client into one shared bucket at runtime instead
  of failing validation up front. Site-level and per-consumer rate limits
  already validated these fields; per-route now does too, matching them.
- **CORS `credentials: true` now requires an explicit, non-wildcard `origins`
  allowlist.** Previously, `credentials: true` with `origins` unset (or
  containing `"*"`) echoed the request's `Origin` header back verbatim with
  `Access-Control-Allow-Credentials: true` for *any* origin — a real
  CSRF/data-exfiltration vector (CWE-942), not just a spec nicety. Config
  validation now rejects this combination at startup/reload.
- **Forward-auth no longer lets a client-forged identity header survive
  when the auth service doesn't return it.** `forwardAuth.responseHeaders`
  only ever *inserted* headers the auth service's response actually
  contained — a header configured but not returned by the auth service
  (e.g. an anonymous/guest session) left the upstream trusting whatever
  value the client itself sent under that name. Configured header names are
  now stripped from the client request before the auth-service-returned
  values are inserted.
- **Redis-backed rate limiting no longer leaks a TTL-less key on a
  timeout/error between `INCR` and `EXPIRE`.** The two commands used to run
  as separate round-trips; a client-side timeout or connection error
  landing between them could leave a key at count 1 with no expiry — that
  key then persists forever, and once later requests push its count past
  the configured limit, that one client is rejected *permanently* instead
  of just for the current window (a transient blip degrading into a
  permanent fail-closed, contradicting the module's own fail-open design).
  Both commands now run as a single atomic Lua script (`EVAL`), which also
  self-heals any key already leaked by the old two-round-trip code the next
  time it's checked.

### Fixed

- **A config sourced from Kubernetes `ConduitSite` CRDs is now validated, both on startup and on every live
  update (issue #492).** The file-based config path has always rejected a config with duplicate `host:port`
  pairs, bad TLS config, etc. before starting or hot-swapping; the Kubernetes path swapped every update in
  unconditionally, with no validation at all — the initial config from `run_kubernetes` and every subsequent
  CRD-driven update from the live-update watcher. A rejected update now keeps the currently-serving config
  instead of applying the invalid one; a rejected initial config exits the process like the file-based path
  already did.
- **The Admin API's rate-limit-cleanup and event-loop-lag-gauge background tasks now stop when the process
  shuts down**, instead of running until process exit regardless of what `BackgroundService::start()`'s
  caller expects. Found while moving this code into `crates/conduit-server` (issue #147) — not new to that
  move, but a real pre-existing gap.
- **A proxied response no longer takes a thread hand-off unless a script or WASM plugin can run on it (issue
  #475).** The response filter chain ran through Tokio's `block_in_place` on every response, although it only
  edits headers unless the site configures response-phase script or WASM `middleware`. On a multi-thread runtime `block_in_place` gives the worker's
  task queue to another thread and takes it back afterwards: about 30 µs of CPU per request (mostly kernel
  time), 11–12 threads alive under load where `global.workers: 1` promises one worker, and a hand-off in
  every request's latency. Measured on a 5950X under WSL2 (`oha -c 50`): with one worker 9.2k → 11.2k req/s
  (+22%) and 120 → 90 µs CPU per request (A/B, six runs per arm); with 4 workers +18%, with 8 workers +20%.
  A chain that contains a response-phase script or a WASM filter still runs through `block_in_place` (a
  request-phase script does not run on the response, so it does not count); a filter is assumed to be able
  to block unless it declares otherwise (`ResponseFilter::may_block`), so a new filter that forgets to say is
  slower, not unsafe.
  Passthrough throughput scales with `global.workers` (default: one worker thread); the throughput figures
  in `docs/benchmarks.md` could not be reproduced and are annotated, and `scripts/bench/` holds the
  scripts used to measure requests per second and CPU per request.
- **HMAC-signed sticky sessions now actually route to the upstream their
  cookie names.** After verifying the cookie against a specific upstream,
  Conduit threw that result away and instead hashed the upstream's *URL
  string* back through `hash % len` — which lands on the pinned upstream
  itself only by coincidence. Measured across 2–8-upstream pools, that
  coincidence holds about 23% of the time (i.e. chance); with exactly four
  upstreams it never holds. So a signed session was usually served by a
  different upstream than the one it was pinned to, silently. The pin is now
  honored directly whenever its upstream is healthy and under
  `maxConnectionsPerUpstream`, with the previous relocate-and-self-heal
  behavior kept for when it isn't. Two knock-on effects are fixed with it:
  `strict: true` was checking the health of the pinned upstream while
  serving a different one, and the "capacity relocation" guard (which
  suppresses cookie re-signing) was firing on nearly every sticky request
  rather than only on real relocations.
- **A route with `retry` configured no longer ignores its load-balancing
  strategy.** The retry path bypassed strategy dispatch entirely and did
  plain round-robin, so `ipHash`/`consistentHash`, weighted round-robin,
  least-conn *and* sticky affinity were all silently inert the moment
  `retry` was added to a route. The first attempt now goes through the same
  strategy dispatch as a non-retry request, and the retry rotation is
  anchored to whichever upstream that produced.
- **Response compression now applies to the metrics endpoint and fallback
  responses, not just static files.** `compression`'s negotiation logic
  (`Content-Encoding` selection, `minBytes`/`types` thresholds) was fully
  implemented and tested but never actually wired into the `/__metrics__`
  handler or fallback (404/SPA-shell/custom-body) responses — both were
  always served uncompressed regardless of config. Each response type is
  still negotiated independently against the site's `compression` config, so
  a small response may stay uncompressed exactly as before.
- **A request to `/.well-known/acme-challenge/*` on a build without
  `--features acme` no longer surfaces as a 502.** The path matched
  unconditionally regardless of the compiled feature; without `acme` there
  was no handler to serve it, and the request fell through to Pingora's
  proxy path with no real upstream to select. The path now only matches
  when `acme` is actually compiled in.
- **`healthCheck.slowStartSecs` now actually ramps traffic to a
  recently-recovered upstream.** The field was parsed and the underlying
  fraction calculation existed, but nothing outside its own unit tests ever
  called it — a freshly-recovered upstream got 100% of its normal traffic
  share immediately, the exact thundering-herd scenario the feature exists
  to prevent (`LeastConn` was the worst-affected strategy: a recovered
  peer's drained connection count made it win every pick until real traffic
  caught it up). Every load-balance strategy now honors it except
  `ipHash`/`consistentHash` and sticky sessions, which are deliberately
  exempt (a probabilistic ramp would break their own consistency
  guarantee) — configuring both together now logs a warning instead of
  silently doing nothing.
- **A `routes[]`-array route with `retry` configured no longer defeats its
  own load-balancing strategy on the first attempt.** The retry candidate
  list was built independently of the peer the route's strategy actually
  chose, so `retry.urls[0]` was always the head of an unrotated list —
  round-robin never rotated, and per-peer stats (`conn_count`, EWMA,
  outlier detection, access logs) were attributed to the wrong upstream.
- **`retry.budgetPercent` no longer self-suppresses over the life of a
  long-running process.** The internal `retry_inflight` counter incremented
  once per retry *decision* but was only ever decremented once per
  *request* — a request that took 2+ retry attempts (e.g. `attempts: 3`
  fully exhausted) leaked a permanent +1 into the counter. Enough leaked
  requests eventually make the budget check deny all retries sitewide, with
  no error or warning.
- **Retry attempts no longer leak a `conn_count` slot on a connect-phase or
  proxy-phase-timeout failure.** Only the 5xx-retry path correctly released
  the connection-capacity slot before moving to the next attempt; a
  connection refusal/timeout, or a read/write timeout mid-response, left
  the slot held forever. Once enough slots leaked past
  `maxConnectionsPerUpstream`, the affected route returned `503` permanently
  until process restart — the opposite of the circuit breaker's intended
  behavior. Both failure modes now also feed passive health/outlier
  detection, which previously only the 5xx path did.
- **Retry attempts now actually respect `maxConnectionsPerUpstream`.**
  Capacity used to be evaluated once, at initial routing, and never
  re-checked as a request moved through its retry attempts — a retry could
  land on (and further overload) a peer already at its connection cap. Each
  retry attempt now forward-probes the retry candidate list for the next
  peer currently under the cap, skipping (not permanently removing) a
  saturated one — the same forward-probe shape `ipHash`/`consistentHash`
  already use for capacity. Fails open to the naive rotation target if
  every candidate is saturated, so a request that has already spent
  attempts is never 503'd purely because capacity deteriorated mid-request.
- **The "`forwardAuth.url` points at the Admin API" validation error now catches
  what it was meant to** (issue #447). It compared the URL's host with `"::1"`
  while the URL library returns IPv6 hosts in brackets, so `http://[::1]:2019`
  was never rejected; it also hard-coded port 2019. The rule now classifies the
  parsed host (`localhost` and `*.localhost`, IPv4 `127.0.0.0/8`, `::1`,
  IPv4-mapped IPv6, and `0.0.0.0` / `[::]`, which connect to the local host on
  Linux and macOS), treats a URL without a port as its scheme's default port,
  and follows the port of `global.admin.bind` (2019 when it is not set). A
  config that pointed `forwardAuth` at such an address on the Admin API's port
  used to load and now fails validation; a different service on port 2019 is no
  longer rejected once `global.admin.bind` uses another port, and a domain that
  merely starts with `127.` is no longer treated as loopback.
- **A site whose `host` is an IPv6 literal was never matched by its requests.** The proxy cut
  the `Host` header at its first `:`, so `[::1]:8080` became `[`; such a request fell through
  to the catch-all site (or found none) and all of them shared one cache namespace. The
  `allowedHosts` check (and the site-host fallback of `securityHeaders`) cut it the same way,
  so it would have rejected such a request once it matched. Both now keep the bracketed
  literal whole (`[::1]:8080` → `[::1]`, one shared function in `conduit-core`), so
  `host: "[::1]"` matches and is allowed.
- **`DELETE /cache/purge?url=…` with an explicit port purged nothing** (issue #444).
  `http://example.com:8080/x` was looked up under the cache namespace `example.com:8080`, while
  the request path stores an entry under the `Host` header *without* its port (`example.com`),
  so the purge answered `{"purged": false}` and the stale entry survived. The cache key now drops
  a port from its host in one place, used by both, so they cannot disagree again; no stored key
  changes, so a persistent (disk/Redis) cache is not made cold. Still not covered: a `Host`
  header with upper-case letters (the purge lower-cases the host) and entries cached with `Vary`.
- **`forwardAuth.url` pointing at the address the Admin API is bound to is now rejected**
  (issue #470). The validation rule that keeps `forwardAuth` away from the Admin API flagged
  loopback and "this host" addresses on the admin port; with a non-loopback
  `global.admin.bind` such as `192.0.2.10:2019`, that address itself was not flagged. It is now
  (an IP address written any way — `2001:db8:0::1` equals `2001:db8::1`, an IPv4-mapped form
  equals its IPv4 one — or a host name, compared case-insensitively). The message for a loopback
  URL is unchanged, and the admin port is taken from every bind form as before, an unbracketed
  IPv6 one such as `::1:3000` included.

### Changed

- **Pingora upgraded from 0.8.1 to 0.9.0.** Two behaviour changes are visible
  to operators:
  - *Response-cache keys hash differently.* Pingora 0.9 removed the separate
    `namespace` argument of `CacheKey`, so Conduit now joins the host and the
    rest of the key with an explicit `\0` boundary instead of relying on the
    old, ambiguous concatenation. A persistent `cache.store` (`disk:` /
    `redis://`) starts cold once after the upgrade. Redis entries expire on
    their TTL; old `disk:` files are never read again and are **not** removed
    automatically — delete the cache directory to reclaim the space. Nodes that
    share one Redis store see disjoint keys across the upgrade: during a rolling
    upgrade old and new nodes cache separately (no collision, no corruption),
    and a `DELETE /cache/purge` on one version does not remove the entry the
    other version wrote.
  - *Hop-by-hop request headers are no longer forwarded to the upstream.*
    Pingora's standard policy now drops `Keep-Alive`, `Proxy-Connection`,
    `Proxy-Authenticate`, `Proxy-Authorization`, `TE`, `Trailer`,
    `Transfer-Encoding` (re-framed by Pingora), `Connection`, `HTTP2-Settings`,
    any header named in the client's `Connection` header, and `Upgrade` unless
    the request is a valid WebSocket handshake (which is normalised and still
    forwarded). A request whose `Connection` header nominates `Host`,
    `X-Forwarded-For`, `X-Forwarded-Host` or `X-Forwarded-Proto` is rejected.
    An HTTP/2 client talking to an HTTP/2 upstream (e.g. gRPC) is not
    rewritten.

  The purge admin endpoint (`DELETE /cache/purge`) keeps its response shape.
  Internally the `Storage::purge` implementations of the disk and Redis cache
  backends follow Pingora 0.9's `PurgeTarget`/`PurgeOutcome` API, and Conduit's
  own header edits use `remove_header`/`append_header` (Pingora 0.9 no longer
  lets `RequestHeader`/`ResponseHeader` be mutated through `DerefMut`).
- **`--no-default-features` builds no longer contain any reverse-proxy code or
  its dependencies** (issue #144). `proxy` is now a real Cargo feature: with it
  off, upstream selection (capacity limits, slow-start, sticky sessions), retry,
  traffic mirroring and the active health checks are compiled out, together with
  `reqwest`, `url` (and its `idna`/`icu_*` tree) and `hmac`/`sha2` — 302 → 265
  crates for `--no-default-features --features static`. `default`, `standard`
  and `full` include `proxy` and are unchanged. Three config shapes change
  meaning in a build without it: a legacy top-level `proxy` shorthand next to a
  site-level `static` (the previously shadowed `static` root becomes live), a
  legacy `proxy` map next to `static` (requests under the proxied prefixes fall
  through to `static`/`fallback` instead of an upstream), and a `routes[]` entry
  with a `proxy` action (it ends in the site's `fallback`, never in its own
  `static` half). Each logs a startup warning naming the exact index. See
  `docs/building.md`.
- **The "`forwardAuth.url` points at the Admin API (`127.0.0.1:2019`)" validation
  error now applies only to builds that enforce forwardAuth** (`--features
  forward-auth`, part of `standard`/`full`). Without the feature the whole
  `forwardAuth` block is ignored (and already warned about), so such a config
  loads with that warning instead of an error; the rule itself is unchanged
  wherever forwardAuth runs.
- **`DELETE /cache/purge` answers `501 Not Implemented` in builds without the
  `cache` feature.** It used to answer `{"status":"ok","purged":false}` — for a
  cache such a build does not have. `cache` is not part of `default`, so a plain
  `cargo build` is affected; the published `standard`/`full` binaries and
  images are not.
- **The `cache` feature now implies `proxy`** (issue #144). The cache stores
  proxied responses only, so a cache build without its proxy was never
  meaningful, and its call sites (`cache.earlyRefreshSecs`, `/cache/purge`) need
  the HTTP client and URL parser that `proxy` brings in. `--features cache`
  therefore also compiles `proxy`; `default`, `standard` and `full` are
  unaffected (they already include both).
- **Two bundles for `--no-default-features` builds** (issue #144):
  `static-server` (`static` + `compression` + `hotreload` — the default set
  minus `proxy`) and `gateway` (`proxy` + `jwt` + `consumers` + `forward-auth` +
  `cache` + `acme` + `compression` — the `standard` set without static files
  and hot-reload). They add nothing to a default build.
- The integration tests that proxy real traffic (`proxy`, `lb_strategies`,
  `rewrite`, `upstream_groups`, `websocket`, and the proxy-dependent tests inside
  `dynamic_upstreams`, `routes`, `security`, `upstream_health`) now require the
  `proxy` feature, so `cargo test --no-default-features --features
  static-server` runs cleanly. A build with `proxy` runs exactly the same tests
  as before.
- `RateLimitConfig` moved to its own crate (`conduit-ratelimit`, issue
  #114/#137 slice 1) — no config shape or behavior change, this closes a
  code-duplication finding between the root crate and
  `conduit-auth-consumers`.
- Static-file serving and fallback (404/SPA-shell/custom-body) responses
  moved to their own crate (`conduit-static`, issue #114/#139) behind a new
  `static` Cargo feature. **Default-on**, like `compression` — a plain
  `cargo build` keeps serving static files and fallback responses exactly
  like before; only `--no-default-features` (without re-adding `static`)
  now produces a build with neither capability compiled in.
- **Config validation moved into the crates that own each config block** (issue
  #316): the `rateLimit`, `limits`, `ipFilter`, `cors`, `middleware`,
  `redirects`, `fallback`, `upload`, `metrics`, `cache`, `tcp`, `proxy`,
  `jwtAuth`, `consumers` and `forwardAuth` checks, and every "configured but
  this build lacks the feature" warning text, now live in those crates. The
  messages, their order and the output of `conduit validate` are unchanged. One
  operator-visible effect: the log lines emitted while validating a route (a
  `slowStartSecs` that is ignored on a hash-based or sticky route) or a cache
  block are now logged under the targets `conduit_proxy_http::validate` and
  `conduit_cache::validate` instead of `conduit::config::validate::proxy`. The
  default `warn` level still shows them, but a filter such as
  `RUST_LOG=conduit::config=debug` no longer matches them.
- **The request pipeline moved into a new workspace crate, `lopatnov-conduit-runtime`**
  (issue #145): the Pingora `ProxyHttp` implementation, `AppState`, the per-request state,
  the request/response/logging phases, request routing, the guard and response chains, the
  access log and the health handler. No config shape change and no new
  dependency (the shipped crate set is unchanged); the root re-exports every item at its
  old path. The one behaviour change made alongside is the IPv6 `Host` fix listed under
  *Fixed*. One operator-visible effect: the `tracing` log lines emitted by this code now
  carry the targets `conduit_runtime::proxy::…` / `conduit_runtime::filter::…` instead of
  `conduit::proxy::…` / `conduit::filter::…`, so a filter such as
  `RUST_LOG=conduit::proxy=debug` no longer matches them (the default `warn` level is
  unaffected). The `otlp`, `tokio-metrics` and the other feature names are unchanged.
- **The Admin API moved into a new workspace crate, `lopatnov-conduit-admin`** (issue #146): the
  axum server, the bearer-token layer and eleven of the twelve endpoints (everything except
  `POST /reload`, which needs the root's config validation and joins the router through its single
  extension point, merged before the authentication layer). No route, status code or JSON changed
  and every route still answers 401 without the token (now pinned by a test on all twelve). One
  operator-visible effect: the `tracing` lines from the moved code carry the targets
  `conduit_admin::api::…` instead of `conduit::admin::api`, so `RUST_LOG=conduit::admin=debug` no
  longer matches them. The crate has one feature, `cache` (the purge endpoint), enabled by the root's
  `cache`.
- **The server bootstrap, the Admin API's background supervisor and `POST /reload`, config
  validation, and the CLI's subcommand dispatch moved into two new workspace crates,
  `lopatnov-conduit-server` and `lopatnov-conduit-cli`** (issue #147): `run_server()` (Pingora
  bootstrap, TLS/plain listener wiring, ACME procurement, Redis rate-limiter connect, TCP/redirect
  services), `AdminApiService` (the rate-limit cleanup, health probe/warmup, Redis cache connect and
  hot-reload watcher background tasks, plus `POST /reload`), `validate()`/`feature_warnings()` and
  the file/Kubernetes config providers on one side; `main()`'s CLI subcommand dispatch and every
  `conduit <command>` implementation on the other. No config shape, route, or CLI flag changed. Two
  behaviour changes rode along, listed under *Fixed*: the Kubernetes config path is now validated,
  and two Admin API background tasks now stop on shutdown. One operator-visible effect: the
  `tracing` lines from the moved code carry the targets `conduit_server::…` instead of
  `conduit::server::…`/`conduit::admin::api`/`conduit::config::…`, so a filter such as
  `RUST_LOG=conduit::server=debug` no longer matches them (the default `warn` level is unaffected).
  `clap`/`clap_complete`/`clap_mangen`/`dialoguer` are no longer root-crate dependencies (`conduit-
  cli` owns them now); `indicatif`, `thiserror` and the root's direct `pingora-cache` edge were
  unused and are dropped from the workspace entirely. Ten root features (`proxy`, `redis`,
  `consumers`, `cache`, `acme`, `tcp`, `upload`, `hotreload`, `tokio-metrics`, `kubernetes`) now
  also forward into `conduit-server`, and `kubernetes` into `conduit-cli` too — every existing
  feature name and bundle (`standard`/`gateway`/`full`/etc.) is unchanged.
- **The config schema (`AppConfig`, `SiteConfig` and the types they contain) and
  the config-file parsing moved into a new workspace crate,
  `lopatnov-conduit-config`** (issue #222). No config shape or behaviour change:
  the crate has no Cargo features and gates no field, and the root crate
  re-exports everything from the same `config::schema` / `config::parse` paths.

---

## [1.2.0] — 2026-08-23

### Security

- **`AllowedHostsGuard` is now secure by default.** Previously, Host-header
  validation only applied when `securityHeaders.allowedHosts` was explicitly
  configured — a site with no `securityHeaders` block (or `allowedHosts`
  unset) accepted any `Host` header, which is then echoed into
  `X-Forwarded-Host` for the upstream. Applications that build absolute URLs
  (e.g. password-reset links) from that header are vulnerable to Host-header
  injection / poisoned-reset-link attacks. Conduit now falls back to
  validating the incoming `Host` against the matched site's own `host:`
  config value when `allowedHosts` is not set, rejecting anything else with
  `400 Bad Request`. Catch-all sites (`host` unset or `"*"`) are unaffected
  and continue to accept any `Host`. Explicit `allowedHosts` configuration
  still takes precedence when present.
- **JWT header-template claim spoofing on `skipPaths` routes fixed.** `{{
  jwt.<claim> }}` header-template substitution decoded and trusted
  `Authorization: Bearer` tokens unconditionally, even on `jwtAuth.skipPaths`
  routes where `JwtGuard` never verifies the token's signature. An attacker
  could forge an unsigned token on a skipped path and have arbitrary claim
  values spoofed into upstream-trusted headers.
- **Consumer authentication silent-bypass warning added.** `feature_warnings()`
  now warns when `sites[].consumers` is configured but Conduit was compiled
  without `--features consumers` (consumer auth is fully disabled and every
  request bypasses it) or when a consumer's `sharedJwt`/per-consumer `jwt`
  credential is configured without `--features jwt` (those consumers are
  permanently unreachable) — previously silent, unlike every sibling feature.
- **`anyhow` bumped `1.0.102 → 1.0.104`** — fixes RUSTSEC-2026-0190
  (unsoundness advisory).
- **IP filter hardening**: IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) are
  now normalized before CIDR matching, so an IPv4-only rule correctly matches
  a client arriving over an IPv4-mapped IPv6 socket; the dynamic deny-list
  now recovers from `RwLock` poisoning instead of failing open.

### Fixed

- **Hostname-based upstream targets work again.** Previously only IP-literal
  upstream addresses were accepted — any hostname target (Docker service
  names, `localhost`, etc.) failed every request.
- **`global.workers` is now actually applied.** The setting was parsed and
  validated but never reached Pingora's server construction — the proxy
  always ran with Pingora's default thread count regardless of what was
  configured.
- **Circuit breaker (`healthCheck.maxConnectionsPerUpstream`) now enforced
  for every load-balance strategy**, not just `LeastConn` — the other 7
  strategies (including the default `RoundRobin`) never checked connection
  load when selecting among healthy candidates. Also now enforced for the
  `routes[]` and `groups` config paths, which previously had no
  circuit-breaker logic at all.
- **Passive health tracking (Outlier Detection, Peak EWMA latency, per-peer
  response stats) now works for every load-balance strategy**, not just
  `LeastConn`.
- **`routes[]` retry no longer rotates into an already-unhealthy peer** — the
  retry candidate list is now health/capacity-filtered the same way the
  top-level `proxy` routing path already was.
- **TLS certificate near-expiry now warns instead of blocking startup.** A
  certificate within its expiry window previously hard-failed server
  startup/`/reload`; it now logs a warning and continues (`conduit validate`
  CLI is unchanged — still exits non-zero on any finding, by design).
  `POST /certs/reload` documentation corrected: the endpoint rewrites the
  cert/key files on disk but a restart (not `/reload`) is required to
  activate a rotated certificate, since Pingora 0.8 has no hot-swap API.
- **Consumer `rateLimit.limit`/`windowSecs` of `0` is now rejected at
  config-validation time**, instead of silently locking the consumer out of
  every request at runtime.

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

[Unreleased]: https://github.com/lopatnov/conduit/compare/v1.5.0...HEAD
[1.5.0]: https://github.com/lopatnov/conduit/compare/v1.4.0...v1.5.0
[1.4.0]: https://github.com/lopatnov/conduit/compare/v1.3.0...v1.4.0
[1.1.0]: https://github.com/lopatnov/conduit/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/lopatnov/conduit/compare/v0.3.0...v1.0.0
[0.3.0]: https://github.com/lopatnov/conduit/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/lopatnov/conduit/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/lopatnov/conduit/releases/tag/v0.1.0
