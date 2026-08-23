# Conduit — Claude's reference

> Высокопроизводительный реверс-прокси на Rust · Cloudflare Pingora · v1.1.0
> Проект: `<projects-root>\conduit`

---

## Локальные репозитории (<projects-root>\) — всегда читать источники

### Rust (прямо применимо к Conduit)
| Путь | Что даёт |
|------|---------|
| `<projects-root>\pingora` | v0.8.1 — КРИТИЧНО. ProxyHttp, TlsSettings, CachePhase, все хуки. Main содержит незарелизенные фичи для 0.9.0 |
| `<projects-root>\tokio` | Async runtime, spawn, channels |
| `<projects-root>\tower` | Service/middleware traits (наш FilterChain построен похоже) |
| `<projects-root>\http` | HeaderMap, Request/Response типы |
| `<projects-root>\reqwest` | HTTP client (mirror, forwardauth, JWKS) |
| `<projects-root>\wasmtime` | v46 WASM engine source |
| `<projects-root>\linkerd2-proxy` | **Rust proxy** — `linkerd/http/retry/src/replay.rs` = ReplayBody (body buffering для retry) |
| `<projects-root>\azure-sdk-for-rust` | Azure SDK — `azure_identity` (Managed Identity), `azure_security_keyvault` (Key Vault). Источник для `--features azure` |

### Proxy/gateway (паттерны и идеи)
| Путь | Язык | Что даёт |
|------|------|---------|
| `<projects-root>\nginx` | C | mTLS, upstream TLS, buffering |
| `<projects-root>\angie` | C | nginx fork (российский, активно развивается) — HTTP/3, ACME, статистика |
| `<projects-root>\freenginx` | C | nginx fork от Igor Sysoev — community-driven, минималистичный |
| `<projects-root>\h2o` | C | HTTP/2 server — mruby scripting, aggressive H2 optimizations, QUIC/H3 |
| `<projects-root>\traefik` | Go | mTLS `ClientAuth`, middleware chain, OTLP |
| `<projects-root>\envoy` | C++ | CircuitBreaker `resource_manager.h`, queue |
| `<projects-root>\haproxy` | C | `src/queue.c` — request queue + backpressure |
| `<projects-root>\apisix` | Lua/Go | Consumer model, 12-phase response pipeline |
| `<projects-root>\oathkeeper` | Go | Authenticator→Authorizer→Mutator (наш ForwardAuth) |
| `<projects-root>\caddy` | Go | Auto-TLS, Let's Encrypt patterns |
| `<projects-root>\squid` | C | Cache patterns |
| `<projects-root>\unit` | C | nginx Unit, модульная архитектура |

---

## Language & Localization

- All code, comments, commit messages, user-facing docs (`docs/*.md`, `README.md`) —
  **English only**
- CLI output, error messages, log entries — **English only**
- No end-user UI to localize
- This applies to the *product* — not to internal maintainer notes. `CLAUDE.md` itself and
  `.claude/**` are the user's own operational tooling/notes and are written in the user's
  working language (Russian); they're excluded from the English-only bar.

---

## Архитектурные решения

Не пересматривать без явного обсуждения.

1. **Обработка запросов:** статика/health/metrics/hot-reload/fallback → Pingora напрямую. Upload → Axum loopback `127.0.0.1:0`. Admin API → Axum порт 2019.
2. **Upload:** стартует только если `upload` в конфиге. Порт не конфигурируется.
3. **CLI:** только subcommands. `upstreams add/remove/weight` — только в памяти, сбрасываются при `conduit reload`.
4. **`ConfigFile` enum:** Full → Sites → Single — **НЕ МЕНЯТЬ ПОРЯДОК**. Single — catch-all (все поля Option).
5. **`static` поле:** `#[serde(rename = "static")]`. В коде: `static_files`.
6. **`ProxyConfig` untagged:** Single → Routes(IndexMap). `ProxyRouteTarget`: Url → RoundRobin → Full.
7. **Bool/object shorthand:** `logging`, `compression`, `securityHeaders`, `cors`, `hotReload`, `healthCheck`, `responseTime` — через `#[serde(untagged)]` enum.
8. **`serde_path_to_error`** — единственный способ парсинга конфига.
9. **Кэш** — только proxy ответы. `ConduitCacheKey` = host + scheme + path + query.
10. **Auto-TLS** — `tls.acme`, `instant-acme`, Phase 3.
11. **IP filter** — CIDR, применяется ДО auth и rate limit.
12. **Hot/cold reload:** port, tls.cert/key/versions/ciphers, workers, backlog, admin — cold. Всё остальное — hot через ArcSwap.
13. **`LogWriter`** — `Arc<LogWriter>` в `AppState.log_writer`; Mutex внутри.
14. **Rate limiter** — `DashMap` v6. Ключ: `"{site}\0{route}"` для per-site override, `"*\0{route}"` — wildcard.
15. **Graceful shutdown** — `Arc<AtomicUsize>` inflight. SIGTERM → перестать принимать → ждать нуля → exit.
16. **`FallbackConfig`:** нет поля `redirect`.
17. **LoadBalanceStrategy** — 8 вариантов (включая P2c). Веса статические. Для IpHash/CH — `hash_key: "ip" | "header:X-Key" | "url"`. P2C: splitmix64 RNG, O(1).
18. **Динамические upstream'ы** — только в памяти. `UpstreamRegistry` отдельно от конфига. `conduit reload` сбрасывает overrides.
19. **Upstream groups** — `groups` + `groupStrategy`. Phase 3.7b.
20. **Filter Chain (CoR)** — `src/filter/chain.rs`. Новый guard = `impl RequestFilter` + push в chain. `service.rs` не трогать. `phase: "response"` scripts пропускаются в request-фазе (`MiddlewareGuard::apply`).
20a. **Feature warnings** — `config::validate::feature_warnings()`. WASM (без `--features wasm`) + OTLP (без `--features otlp`) → `tracing::warn!` при старте и hot-reload. `/reload` response включает поле `warnings: [...]`.
21. **Handler Registry** — трейт `LocalHandlerImpl` в `src/handler/mod.rs`. 7 handler structs реализованы. `dispatch_local` → `build_handler()` + `handle()`.
22. **Routing Strategy** — трейт `LoadBalancingStrategy` в `src/proxy/strategy.rs`. Новая стратегия = новый struct + `from_config()` arm. `router.rs` не трогать.
23. **CLI Commands** — трейт `CliCommand` в `src/cli/mod.rs`. Новая команда = struct + arm в `dispatch_command()`. `main()` не трогать.
24. **YAML конфиг** — `serde_yaml`, `from_yaml()` в `parse.rs`, автопоиск `conduit.yaml/yml`.
25. **Provider pattern** — `Provider` trait в `src/config/provider.rs`. `FileProvider` (one-shot + auto-reload). `KubernetesProvider` (feature = "kubernetes") в `src/config/kubernetes.rs`.
26. **WASM middleware** — `type: "wasm"` в middleware array, feature = "wasm". Wasmtime, 12 host-функций, fail-open. `src/filter/wasm.rs`. Плагины экспортируют `on_request() -> i32`. Память должна быть экспортирована как `"memory"`.
27. **MiddlewareGuard** — объединяет Rhai ("script") и WASM ("wasm") в `src/filter/chain.rs`. Порядок entries соблюдается. `ScriptGuard` = type alias для совместимости.
28. **CGI** — не входит в Conduit, отдельный проект.
29. **Тесты** — port 0, rcgen, serial_test для Admin API, mock = `TcpListener` без Axum.
30. **`RequestCtx` per-request state (Conduit 2.0 migration, #114)** — поля остаются в корневом крейте
    (status quo), НЕ выносятся в type-erased extension slot и НЕ через отдельный trait в `conduit-core`.
    Каждое feature-specific поле — через `#[cfg(feature = "x")]` по образцу уже существующих
    `otel_span`/`early_refresh_upstream_url`. Решение пользователя 2026-08-21 по итогам `architect`-аудита
    Phase 2 facade-checkpoint (issue #128) — снимает блокировку с #129 (`conduit-otlp`) и последующих
    #131/#133/#135/#141/#142. Не пересматривать без явного обсуждения (см. заголовок раздела).

---

## Pipeline обработки запроса

```
request_filter()
  ├─ inflight++, active_connections.inc()
  ├─ FilterChain: XRequestIdGuard → IpGuard → CorsPreflight → HealthBypass → LimitsGuard
  │              → RateLimitGuard → ConsumersGuard (6) → BasicAuthGuard → ApiKeyGuard → JwtGuard (6c)
  │              → ForwardAuthGuard (6d) → RedirectGuard → FaultInjectionGuard
  │              → MiddlewareGuard (Rhai + WASM in order)
  ├─ Per-route rate limit check (post-routing, key "route:{key}:{ip}")
  ├─ Priority load shedding: if inflight/maxInflight ≥ threshold AND route.priority < 50 → 503
  ├─ Circuit breaker: if all upstreams at maxConns → LocalHandler::Overloaded → 503
  └─ JWT claims extraction (for {{ jwt.sub }} templates) → RequestCtx.jwt_claims
     build_handler() → handler.handle(session) / Ok(false) → Pingora продолжает

upstream_request_filter()
  ├─ append_forwarded_headers (XFF, XFP, X-Forwarded-Host)
  ├─ apply_upstream_path_transforms (strip_prefix, rewrite)
  ├─ requestTransform: setHeaders (with {{ jwt.claim }} expansion), removeHeaders
  └─ fire_mirror_request() if mirror_url set (fire-and-forget tokio task)

upstream_response_filter()                    ← тонкая обёртка над ResponseFilterChain
  ResponseFilterChain (src/filter/response_chain.rs):
  Phase 1  CrlfProtectionFilter   — strip CR/LF from upstream headers
  Phase 2  InjectExtraHeadersFilter — CORS + security + custom headers
  Phase 3  ResponseTransformFilter — responseTransform: set/remove
  Phase 4  ResponseTimeFilter     — X-Response-Time header
  Phase 5  RetryOnErrorFilter     — 5xx → RetryUpstream outcome → Pingora retry
  Phase 6  ErrorMaskFilter        — 5xx → MaskBody outcome → body replaced

upstream_response_body_filter()
  └─ mask_upstream_body: replace 5xx body with generic JSON if maskErrors=true

logging()
  ├─ inflight--, retry_inflight-- if is_retrying, active_connections.dec()
  ├─ conn_dec(url) for least-conn AND circuit-breaker-tracked upstreams
  ├─ EWMA + outlier detection update, upstream_errors_total counter
  ├─ access log (skipPaths respected), Prometheus
  └─ cache hit/miss counters
```

Health / ACME / HotReload — bypass всех guard-фильтров.

---

## Беклог технических улучшений

### Надёжность и устойчивость

#### Высокий приоритет

- [x] **X-Request-ID injection** — `XRequestIdGuard` в `filter/chain.rs`. UUID v4 если absent, forward если present. Первый guard в FilterChain.
- [x] **Outlier Detection** — `outlierDetection: { consecutive5xx, baseEjectionTimeSecs, maxEjectionTimeSecs, maxEjectionPercent }`. `maybe_eject()` + `UpstreamEntry.ejected_until_secs/ejection_count`. Exponential backoff. Max ejection % enforcement.
  **2026-08-03 caveat (found by integrity audit) — fixed 2026-08-17 by #214 (issue
  #155)**: was gated behind `RequestCtx.proxy_upstream_url`, only populated for
  `LeastConn`/circuit-tracking — #214 made it unconditional for every strategy, so this
  now tracks/ejects for all 8 load-balance strategies.
- [x] **Circuit Breaker** — `healthCheck.maxConnectionsPerUpstream: u64`. Enforced for
  every load-balance strategy, across `proxy: {}`, `routes[]`, and `groups` — a request
  only routes to an upstream currently under the cap; ALL healthy upstreams at/over it →
  `LocalHandler::Overloaded` → 503. `IpHash`/`ConsistentHash` (incl. sticky) forward-probe
  the unshrunk hash ring rather than filtering it, so only clients whose preferred peer is
  saturated relocate. Soft limit (TOCTOU overshoot accepted, matches `retry.budgetPercent`).
  New `src/proxy/capacity.rs` module (`Capacity`/`pick_bounded`) is the single evaluation
  point shared by all three routing paths — `router.rs`/`routes.rs` never match on
  `LoadBalanceStrategy` variants for capacity purposes, only `capacity.rs` does.
  **2026-08-03 correction (integrity audit) — fixed 2026-08-17 (issue #156)**: "works for
  all LB strategies" was inaccurate until this fix — only `LeastConn` (7/8 non-`LeastConn`
  strategies, including `RoundRobin`, never re-checked `conn_load`). `conn_inc_if_below()`/
  `pick_least_conn_with_max()` (the previously-dead mis-designed helpers) deleted outright
  rather than reused. Also fixed as a documented side effect: `cache.earlyRefreshSecs`
  (closed feature issue #31) was silently gated by the same `proxy_upstream_url` condition
  and was already fixed by #214 before #156 started — no code change needed for it.
  Follow-ups filed, not fixed in #156: #216 (retry attempts bypass the cap and undercount
  `conn_count`), #217 (`routes[]` retry list not health/capacity-filtered), #218
  (`failed_upstream_attempts` is write-only state).
- [x] **Forward Auth** — `forwardAuth: { url, requestHeaders?, responseHeaders?, timeoutMs?, skipPaths? }`. `ForwardAuthGuard` (6d в chain). Subrequest через `reqwest::Client` singleton. 2xx=allow+inject headers, 4xx/5xx=deny, unreachable=fail closed. 5 integration tests.
- [x] **Service Failover** — `ProxyRouteConfig.backup`. Когда все primary unhealthy → route to backup. Логика в `resolve_proxy()`.
- [x] **Inflight request limit** — `LimitsConfig.maxInflightRequests`. `LimitsGuard` проверяет `inflight` перед прочими лимитами. 503 при превышении.
- [x] **Traffic Mirroring** — `proxy.*.mirror: String`. Fire-and-forget через `tokio::spawn` + reqwest. V1: headers only (тело не буферируется). Заголовок `X-Mirrored-From` добавляется. `UpstreamTarget::Proxy.mirror_url`. `fire_mirror_request()` в service.rs.

#### Средний приоритет

- [🚫 BLOCKED] **Request queue + backpressure** — когда upstream на maxconn: ставить в очередь (не сразу 503). Priority queue по классу + timestamp. HAProxy: `queue.c`.
  **Причина:** `ProxyHttp` trait не имеет хука "upstream перегружен, подожди" — Pingora не предоставляет механизма задержки принятия соединения до освобождения upstream slot. Circuit Breaker (`maxConnectionsPerUpstream`) покрывает основной кейс. Ждём Pingora 0.9+.
- [x] **Upstream slow start** — `UpstreamEntry.recovery_time_secs` + `slow_start_fraction()` в health.rs. `UpstreamHealthCheck.slowStartSecs` config field.
- [x] **Sticky sessions** — `ProxyRouteConfig.sticky.cookie`. `extract_cookie()` в router.rs. Cookie value → consistent-hash key. `StickyConfig` в schema.

---

### Кэширование

#### Высокий приоритет

- [x] **Cache thundering herd prevention** — `CACHE_LOCK` singleton в `proxy/cache.rs`, передаётся в `session.cache.enable()`. Pingora `CacheLock` (16 шардов, 10s timeout): первый запрос — `Write`, остальные ждут `Read` lock.
- [x] **Stale-while-revalidate** — `cache.staleWhileRevalidateSecs + staleIfErrorSecs`. `CacheMeta::new(fresh_until, now, swr, sie, resp)`. `should_serve_stale()` hook в service.rs. `parse_cc_directive()` читает upstream `Cache-Control` header.
  Источник: `<projects-root>\pingora\pingora-cache\src\lib.rs:85`, `proxy_trait.rs:621`.

---

### Аутентификация и авторизация

#### Высокий приоритет

- [x] **JWT validation with JWKS URL** — `jwtAuth: { secret? | jwksUrl?, audience?, issuer?, skipPaths? }`. HS256 + RS256/ES256 (JWKS). `JwtGuard` в filter chain (6c, после apiKey). `src/filter/jwt.rs`. JWKS кэш per-URL с TTL. `jsonwebtoken = "10"`. 20 unit + 6 integration тестов.
  **Уточнение 2026-08-10 (integrity audit, Step 1c)**: все 20 unit-тестов и все 6
  integration-тестов покрывают только HS256 — RS256/ES256-путь через JWKS (парсинг
  `Jwk`, `kid`-матчинг, выбор алгоритма) вообще не тестируется, см.
  [issue #164](https://github.com/lopatnov/conduit/issues/164). Отдельно найдено: JWKS
  refresh — блокирующий синхронный fetch внутри async guard, без single-flight и без
  fallback на протухшие-по-TTL ключи при недоступности endpoint'а — см.
  [issue #163](https://github.com/lopatnov/conduit/issues/163).
- [x] **Conditional error responses** — `write_denied()` в `handler/response.rs`: Accept: application/json → JSON body {"error":"Unauthorized","status":401}; иначе empty.

#### Средний приоритет

- [x] **Consumer model для auth** — `consumers: { consumers: [{username, apiKey?, basicAuth?, rateLimit?, headers?}], idHeader?, apiKeyHeader?, skipPaths? }`. `ConsumersGuard` (step 6, before basicAuth). `identify_consumer()` in auth.rs. Injects `X-Consumer-ID`. Per-consumer rate limit key `"consumer:{username}"`. 8 integration tests.
- [x] (V2) JWT consumers: `consumer.jwt: { secret | jwksUrl }` — идентификация по факту валидации токена.
- [x] (V3) Shared JWT: `consumers.sharedJwt: { jwksUrl, usernameClaim }` — один JWKS для всех, идентификация по `sub` claim. Auth0/Cognito/Keycloak pattern.

---

### Производительность и наблюдаемость

#### Высокий приоритет

- [x] **P2C load balancing** — `P2cChoice` в `src/proxy/strategy.rs`. splitmix64 RNG. `LoadBalanceStrategy::P2c`. 3 unit-теста.
- [x] **Peak EWMA latency tracking** — `UpstreamEntry.ewma_latency_us`, α=0.1. `record_request_latency()` в health.rs, вызывается из `logging()`. Пассивный замер реального трафика.

#### Средний приоритет

- [x] **OpenTelemetry (OTLP) трейсинг** — `global.otlp: { endpoint, serviceName, sampleRate, timeoutMs }`. `src/server/otel.rs`. Feature `--features otlp`. Spans: method/path/status/duration/upstream/request_id. 5xx → span.status=ERROR. Grafana Tempo/Jaeger/Honeycomb.
- [x] **Structured access log fields** — `AccessLogContext { request_id, upstream_addr }` в `filter/logging.rs`. JSON format включает `request_id` (из X-Request-ID) и `upstream` URL.

#### Низкий приоритет

- [x] **Zero-allocation `logging()` hot path** — РЕАЛИЗОВАНО и СМЕРДЖЕНО
  ([PR #90](https://github.com/lopatnov/conduit/pull/90), merge-commit `a84b467`,
  [issue #88](https://github.com/lopatnov/conduit/issues/88) closed COMPLETED 2026-06-12).
  `logging()` теперь берёт `method`/`status`/`route` заимствованными `&str`
  (`StatusCode::as_str()` — `&'static str`, `uri.path()`/`method.as_str()` живут пока
  жива `Session`), `status_u16` считается один раз и переиспользуется. Zero-alloc
  свойства сохранены последующим рефактором 1a (PR #91). _(Чекбокс не отметили при
  мердже #90 — закрыто задним числом 2026-06-13.)_

- [ ] **Re-benchmark `--features standard`** — `docs/benchmarks.md` "Build Sizes" теперь
  содержит строку `--features standard`: Windows MSVC **21.2 MB** измерено локально
  (`cargo build --release --features standard`, та же "unstripped" методология что и
  у соседних строк), Linux musl **~17.8 MB** — оценка через коэффициент строки `default`
  (14.3/17.0 ≈ 0.84), помечена ¹, НЕ измерена напрямую. Нужно: реальный
  `cross build --release --target x86_64-unknown-linux-musl --features standard` (или
  взять артефакт из release.yml) для точной цифры, плюс прогон wrk-бенчмарков
  (latency/throughput/memory) против `--features standard` — таблица "Standard vs Full —
  Overhead per Feature" сейчас описывает только `default=[]`.
  Источник: rename-in-place `standard` feature bundle в release.yml/ci.yml/Dockerfile,
  ветка `ci/wire-standard-feature-pipeline`, 2026-06-11.

---

### Безопасность

#### Высокий приоритет

- [x] **mTLS (client certificate auth)** — `tls.clientAuth: { ca, optional }`. `make_tls_settings_with_client_auth()` в server/tls.rs. `WebPkiClientVerifier::builder()` + `load_ca_file_into_store()` из `pingora_core::tls`. TlsPortMap расширен для передачи `TlsClientAuth`.
  Источник: `<projects-root>\pingora\pingora-core\src\listeners\tls\rustls\mod.rs:97`.
- [x] **Error masking** — `maskErrors: bool` в SiteConfig. `upstream_response_body_filter` заменяет 5xx тело на `{"error":"Internal Server Error","status":500}`. Content-Type/Length обновляется.
- [x] **Upstream TLS verification** — `proxy.*.upstreamTls: { verify: bool, serverName: string }`. Stored in `UpstreamTarget::Proxy.upstream_tls`. Applied in `upstream_peer()`: sets `peer.options.verify_cert/verify_hostname/alternative_cn`.
  **⚠️ РАСШИРЕНИЕ в Pingora main (→ 0.9.0):** коммит `61febef` добавляет per-peer CA support —
  `peer.get_ca()` теперь реально используется в rustls connector, можно задавать отдельный
  CA bundle на каждый upstream. Разблокирует поле `upstreamTls.ca: string` в конфиге Conduit.

#### Средний приоритет

- [x] **Header injection protection** — CRLF: collect+remove headers от upstream с `\r`/`\n` в `upstream_response_filter` (Pingora HMap нет retain).
- [🚫 BLOCKED] **OCSP stapling config** — expose через конфиг, сейчас rustls обрабатывает внутренне.
  **Причина:** Pingora rustls backend не имеет публичного API для управления OCSP stapling. Rustls обрабатывает его внутренне без конфигурации. Ждём Pingora 0.9+.

---

### Retry и timeout

#### Средний приоритет

- [x] **Request body buffering для retry** — `limits.maxBodyBufferBytes: u64`. `request_body_filter()` в service.rs накапливает чанки в `RequestCtx.body_buffer`. При overflow → `body_too_large = true`. Паттерн linkerd2-proxy `ReplayBody`.
  Источник: `<projects-root>\pingora\pingora-proxy\src\proxy_trait.rs:132`, `<projects-root>\linkerd2-proxy\linkerd\http\retry\src\replay.rs`.
- [x] **Retry budget** — `retry.budgetPercent: f64`. `AppState.retry_inflight: AtomicUsize`. `retry_budget_allows()` в service.rs: мягкое ограничение. `RetryState.is_retrying` для декремента в `logging()`.
- [x] **Per-try timeout** — `ProxyTimeout.perTryMs` в schema.

---

### Расширяемость (API Gateway)

#### Средний приоритет

- [x] **Request/Response Transformation (static V1)** — `requestTransform`/`responseTransform: { setHeaders, removeHeaders }`. Applied in `upstream_request_filter`/`upstream_response_filter`. `HeaderTransformConfig` в schema.rs. `RequestCtx.response_transform`. V2: template engine ({{ jwt.sub }}, etc.).
- [x] **Header Transform V2 (JWT templates)** — `{{ jwt.<claim> }}` в requestTransform.setHeaders. `extract_claims()` в jwt.rs. `RequestCtx.jwt_claims`. `expand_jwt_templates()` в service.rs. 2 unit tests.
- [x] **Fault Injection** — `FaultInjectionConfig` в schema.rs, `FaultInjectionGuard` в chain.rs. Abort N% (status + body) + delay N% (ms). splitmix64 RNG. НЕ для production.

#### Низкий приоритет

- [x] **Phase-ordered response pipeline** — `ResponseFilter` trait + `ResponseFilterChain` в `src/filter/response_chain.rs`. 6 фаз: CrlfProtection → InjectExtraHeaders → ResponseTransform → ResponseTime → RetryOnError → ErrorMask. `ResponseFilterChain::build(req_ctx, config)`. `upstream_response_filter` — тонкая обёртка.
- [ ] **Middleware Stack** — только если появятся конкретные кейсы.

---

### Архитектурные

#### Высокий приоритет (уже реализованы)

- [x] **Routing Strategy трейт** (`src/proxy/strategy.rs`) — `LoadBalancingStrategy` trait, zero-sized structs, `from_config()` без аллокаций.
- [x] **Handler Registry** (`src/handler/`) — 7 handler structs, `dispatch_local` → 20 строк, новый handler = 1 arm в `build_handler()`.
- [x] **CLI Command Pattern** (`src/cli/mod.rs`) — `CliCommand` trait, 11 structs, `main()` → 3 строки.

#### Высокий приоритет (не реализованы)

- [x] **Provider pattern** — `Provider` trait в `src/config/provider.rs`. `FileProvider` (one-shot + auto-reload через notify). 12 unit-тестов включая авто-перезагрузку.
- [x] **Kubernetes / CRD** — `ConduitSite` CRD через `kube::CustomResource`. `KubernetesProvider`: list+watch паттерн. `spec_to_site_config()` через JSON round-trip. Feature: `--features kubernetes`. CRD манифест: `contrib/k8s/`. 10 unit-тестов без кластера.
- [x] **`--kubernetes-namespace` CLI flag** — `#[cfg(feature = "kubernetes")]` arg in `src/cli/args.rs`. `dispatch_command` starts `run_server_kubernetes(ns)` when flag is present and no subcommand given. Supports `"*"` for all namespaces. `docs/deployment.md` updated with usage examples.

#### Низкий приоритет

- [x] **WASM plugin system** — `type: "wasm"` вместе с Rhai (не вместо). Wasmtime, `--features wasm`. 17 host-функций (read/modify headers, set response, get_uri, get_header_names, abort_with_redirect, get_request_id). Module cache, fail-open. `src/filter/wasm.rs`, 15 unit-тестов inline WAT.

#### Запланировано (обсуждено 2026-06-06, issue #65) — порядок строго последовательный

> Решено: сначала рефактор `service.rs` (низкий риск, можно делать независимо), и только
> потом — затея с v2-архитектурой (она крупная и переосмысливает feature-систему целиком,
> не стоит начинать её прежде, чем устаканится база).

- [x] **1. Разбить `service.rs` (~4000 строк) на фазы** — РЕАЛИЗОВАНО и СМЕРДЖЕНО в main
  ([PR #82](https://github.com/lopatnov/conduit/pull/82), merge-коммит `6ce4597`, 2026-06-12). Итог:
  - `request_phase.rs` (3080 строк) — request_filter, guard chain, routing, retry, peer + helpers
  - `response_phase.rs` (450 строк) — upstream_response_filter / body / response cache
  - `logging_phase.rs` (295 строк) — logging() + access log + метрики
  - `service.rs` (549 строк) — ConduitMetrics, AppState, тонкий `impl ProxyHttp`-делегатор
  Поведение не изменилось: fmt/clippy (default + full, `-D warnings`) чисто,
  `cargo test` (unit + integration) и `cargo test --features full --lib` (1146) — зелёные.

- [x] **1a. SonarCloud Cognitive Complexity (rust:S3776) на новых phase-файлах** —
  РЕАЛИЗОВАНО и СМЕРДЖЕНО в main
  ([PR #91](https://github.com/lopatnov/conduit/pull/91), squash-merge `267ba51`,
  2026-06-13). Оба CRITICAL S3776 устранены: `logging_phase.rs::logging()` CC 41→0
  (плоский оркестратор), `request_phase.rs::do_request_filter()` CC 37→~6. Helper-функции
  вынесены как в PR #69; поведение не изменилось (zero-alloc свойства из PR #90 сохранены).
  SonarCloud Quality Gate на PR: PASSED, 0 new issues. Все 27 CI-чеков зелёные.
  Review-фидбек (Gemini ×4 — &str-borrow path + Option::take) применён коммитом `7a9dedb`.
  Вне scope остались 3 старых S3776: `router.rs::route_request` CC 79,
  `config/validate.rs` CC 21, `cli/init.rs` CC 16 (если делать — отдельным пунктом).
  **Все три закрыты 2026-08-17** — см. запись в конце этого файла
  ("Реализовано в сессии 2026-08-17"), включая поправку: CC 79 был не в
  `route_request` (плоский `match`, CC ~7), а в безымянном теле match-arm
  внутри `resolve_proxy`, теперь названном `resolve_proxy_routes`.

- [x] **1b. Config-snapshot drift в post-route хелперах `request_phase.rs`** (CodeRabbit
  на PR #91, Major) — РЕАЛИЗОВАНО и СМЕРДЖЕНО в main
  ([PR #92](https://github.com/lopatnov/conduit/pull/92), squash-merge `5cc1c59`,
  2026-06-13). `do_request_filter` берёт один `config.load_full()` (owned Arc) и
  использует его и для routing, и для резолва `site` (один раз) → прокидывает
  `Option<&SiteConfig>` в `store_ip_conn_slot` / `enforce_route_rate_limit` /
  `shed_low_priority_request`. Routing + 3 хелпера теперь на одном снапшоте —
  routing-vs-helper TOCTOU закрыт. **4 `config.load()` → 1 `load_full()`**.
  `load_full()` (owned Arc, без аллокации) безопасно держать через `.await`
  guard-чейна, в отличие от guard от `load()`. Поведение в steady state не
  изменилось; разница только при hot-reload (хелперы консистентны с routing).
  SonarCloud QG PASSED (0 new issues, без новых S3776), 27/27 CI зелёные,
  CodeRabbit/Gemini — без замечаний.

- [ ] **2. (V2, после пункта 1) Полностью feature-driven архитектура + Chain-of-Responsibility
  сборка по компиляции** — переосмысление feature-системы по аналогии с другим (Express-based)
  проектом пользователя:
  - **Всё** — статика, прокси/API, кэш и т.д. — становится compile-time Cargo-фичей.
  - Guard/response-chain собирается **только из звеньев скомпилированных фич** — никаких
    рантайм-проверок вида `if has_jwt { ... }`; отсутствующая фича = отсутствующий код
    (настоящий zero-cost abstraction, по аналогии с тем, как `tower` кодирует middleware-стек
    на уровне типов — см. `<projects-root>\tower`).
  - Поверх — **именованные бандлы** под конкретные сценарии вместо текущих `standard`/`full`:
    ```
    conduit-dev     = static + hot-reload + error-details + cors
    conduit-dotnet  = proxy + jwt + headers + websocket + health
    conduit-java    = proxy + duplicate-chunked-fix + actuator-health + headers
    conduit-full    = всё
    ```
  Это крупная переработка — требует отдельного проектного обсуждения (`business-analyst` +
  пользователь) перед началом, и логично делать на базе уже разбитого на фазы `service.rs`.

---

## Беклог из исследования репозиториев (<projects-root>\)

> ⚠️ Это результат предварительного анализа. Каждую задачу нужно детально изучить
> перед реализацией. Источники: pingora, linkerd2-proxy, traefik, nginx, envoy, haproxy,
> apisix, oathkeeper, caddy, rustls, tower, h2o, angie, freenginx.

### 🔓 Разблокированы (ранее считались заблокированы)

- [x] **mTLS — client certificate auth** — реализовано. `tls.clientAuth: { ca, optional }`. `make_tls_settings_with_client_auth()` в `server/tls.rs`. `WebPkiClientVerifier::builder()` + `load_ca_file_into_store()` из `pingora_core::tls`. `TlsClientAuth` в schema.rs. `TlsPortMap` расширен для передачи client_auth. `examples/mtls.yaml`.

- [x] **Stale-while-revalidate** — реализовано. `cache.staleWhileRevalidateSecs` + `cache.staleIfErrorSecs` в `CacheConfig`. `should_serve_stale()` hook в `service.rs`. `parse_cc_directive()` + `CacheMeta::new(fresh, now, swr, sie, resp)` в `cache.rs`. `examples/stale-while-revalidate.yaml`.

- [x] **Request body buffering для retry** — реализовано. `limits.maxBodyBufferBytes: u64` в `LimitsConfig`. `request_body_filter()` в `service.rs` накапливает чанки в `RequestCtx.body_buffer`. При overflow `body_too_large = true`. Паттерн linkerd2-proxy `ReplayBody`.

---

### 🔒 Безопасность

- [x] **Certificate rotation** — `POST /certs/reload`. Принимает `{ cert, key }` PEM, валидирует пару через rustls (cert/key match), записывает атомарно в `tls.cert`/`tls.key` файлы. После — `conduit reload` или рестарт процесса активирует новый серт. `validate_cert_key_pem()` в `server/tls.rs`. 5 unit-тестов + 4 integration-теста. **Zero-downtime hot-swap заблокирован Pingora 0.8** — нет `ResolvesServerCert` API для rustls backend. Ожидаем Pingora 0.9+.

- [x] **IP rate limit с burst** — `rateLimit.burst: u32`.
  Сейчас токен-бакет без burst. Добавить burst capacity.
  Паттерн: nginx `limit_req zone=... burst=5 nodelay`.
  Источник: `<projects-root>\nginx\src\http\modules\ngx_http_limit_req_module.c`.

- [x] **Deny list / CIDR block API** — Admin API `POST /ip-deny { cidr: "1.2.3.0/24" }`.
  Динамическое добавление/удаление deny-CIDRs без reload.
  Хранить в `Arc<RwLock<Vec<IpNet>>>` в AppState. IpGuard читает.
  Паттерн: envoy Network RBAC filter.

---

### 📊 Производительность и наблюдаемость

- [x] **Per-upstream Prometheus метрики** — `conduit_upstream_active_connections{upstream}` (gauge),
  `conduit_upstream_requests_total{upstream, status}` (counter),
  `conduit_upstream_latency_seconds{upstream}` (histogram).
  Сейчас только per-route. Нужно per-URL метрики для диагностики.
  Паттерн: envoy cluster stats.

- [x] **Access log `$upstream_response_time`** — сколько upstream отвечал (мс).
  Сейчас `duration_ms` = полное время запроса. Нужен отдельный upstream_time.
  Хранить `upstream_start: Instant` в RequestCtx, записывать в logging().
  Паттерн: nginx `$upstream_response_time`.

- [x] **Health check endpoint расширение** — `/__health__?full=1` возвращает upstream статусы.
  Уже есть `includeUpstreams: true`. Добавить: latency, ejection status, consecutive_5xx.
  Паттерн: traefik `/api/rawdata`.

- [x] **`conduit status --upstream`** — CLI команда показывает upstream health из Admin API.
  `conduit status --upstream` → таблица с URL, healthy, latency_ms, ejected.
  Данные из `GET /upstreams` admin endpoint.

---

### ⚡ Надёжность

- [x] **Half-open circuit breaker** — после ejection period пропускать 1 тестовый запрос.
  Сейчас: Outlier Detection eject → после timeout снова all traffic.
  Нужно: eject → timeout → 1 probe request → если OK full traffic, если нет → re-eject.
  Паттерн: traefik circuit breaker (half-open state), envoy `successive_5xx`.
  `UpstreamEntry.half_open: bool` флаг.

- [ ] **Graceful upstream drain** — при `conduit reload` дать старым соединениям завершиться.
  Сейчас hot-reload просто меняет config через ArcSwap.
  Нужно: если upstream URL изменился, дождаться нуля active connections на старом URL.
  Паттерн: nginx `upstream_zone` + drain.
  **⚠️ ЧАСТИЧНО РАЗБЛОКИРОВАНО в Pingora main (→ 0.9.0):** коммит `ee387f4` добавляет
  `daemon_wait_for_ready = true` — новый процесс шлёт SIGUSR1 когда готов, старый только
  тогда начинает shutdown. Устраняет 502s при zero-downtime деплое. Пример:
  `<projects-root>\pingora\pingora\examples\graceful_upgrade.rs`.

- [x] **Upstream connection pool warmup** — `healthCheck.prewarmConnections: u8` (макс 8). `spawn_connection_warmup()` в `health.rs` запускает N HEAD-запросов к upstream при старте через reqwest. Вызывается из `AdminApiService::start()`. Значения выше 8 обрезаются.

- [x] **Retry с экспоненциальным jitter** — `retry.backoffMs` + jitter ±50%.
  Сейчас backoffMs фиксированный. Thundering herd при массовом retry.
  `retry.backoffJitter: bool`. `sleep(backoff_ms ± rand(0..backoff_ms/2))`.
  Паттерн: AWS SDK exponential backoff with jitter.

---

### 🌐 Маршрутизация

- [x] **Header-based routing** — `routes[].match.headers` с regex matching реализовано. `routes[].match.cookies: { "beta": "1" }` — cookie routing добавлен (`cookies_match()` в `routes.rs`). Regex паттерны: `"v2"` (точное), `"blue|green"` (regex). Тесты включены.

- [x] **Query parameter routing** — `routes[].match.query` с regex + multiple params уже реализовано в `routes.rs` через `query_params_match()` + `regex_match()`. Тесты есть.

- [x] **Priority routing** — `proxy.*.priority: u8` (0=low, 100=high). `limits.priorityThreshold: f64` (default 0.8). Post-routing check in service.rs: when `inflight/maxInflight ≥ threshold` AND `effective_priority < 50` → 503 Load Shedding. Effective priority = `max(route.priority, X-Priority header)`. `find_route_priority()` in router.rs. 4 unit-тесты. Examples: `priority-routing.yaml/json`.

- [x] **TCP proxy mode** — `type: "tcp"` → `tcp: { targets, strategy, connectTimeoutMs }` в SiteConfig. `TcpProxy` implements `ServerApp` в `src/proxy/tcp.rs`. `tokio::io::copy_bidirectional` для bidirectional relay. Round-robin + random strategies. ListeningService в builder.rs. 6 unit-тестов.

---

### 🔌 Extensibility

- [x] **WASM `on_response()` hook** — опциональный export `on_response(status: i32) -> i32` в WASM-плагинах. 7 новых host-функций: `conduit_get_response_status`, `conduit_get_response_header`, `conduit_set_response_header`, `conduit_remove_response_header`, `conduit_set_response_body`, `conduit_get_plugin_config`, `conduit_log`. `WasmResponseContext/State` в `wasm.rs`. Phase 7 `MiddlewareResponseFilter` в `response_chain.rs`. Fail-open: плагины без `on_response` export молча пропускаются.

- [x] **Rhai `on_response` script** — `phase: "response"` в `MiddlewareEntry`. Scope: `upstream.status`, `upstream.header("Name")`, `response.set_header()`, `response.remove_header()`. `ScriptResponseBuilder`, `ScriptUpstreamView`, `run_script_response()` в `script.rs`. Отдельный движок `engine_response()`. Интегрирован в Phase 7 ResponseFilterChain.

- [ ] **External processing filter (ext_proc)** — gRPC stream для внешней модификации req/resp.
  Conduit отправляет запрос/ответ внешнему gRPC сервису для обработки.
  Config: `{ "type": "ext_proc", "grpc": "grpc://filter-service:9000" }`.
  Паттерн: envoy External Processing filter (`<projects-root>\envoy\source\extensions\filters\http\ext_proc`).
  Feature `--features ext-proc`. Требует tonic (`<projects-root>\tonic`).

- [ ] **Lua скрипты** — `type: "lua"` middleware (менее приоритетно чем Rhai/WASM).
  Используется в nginx/OpenResty/apisix. Mlua crate.
  Только если появится конкретный кейс.

---

### 🗂️ Кэширование (расширение)

- [x] **Disk cache** — `cache.store: "disk:/path"`. `DiskCacheStorage` в `cache_disk.rs` реализует `Storage` trait. Атомарная запись: `.tmp` → rename в `.cache`. Формат: `[u32 len(meta0)][u32 len(meta1)][meta0][meta1][body]`.

- [x] **Redis cache** — `cache.store: "redis://..."` и `"rediss://..."` (TLS). `RedisCacheStorage` в `cache_redis.rs` реализует `Storage` trait. HMGET/HSET+EXPIRE через `ConnectionManager`. Fail-open: недоступный Redis не крашит сервер — кэш молча отключается. Валидация `cache.store` добавлена в `validate.rs`. 8 unit-тестов включая fail-open на порту 1.

- [x] **Cache purge API** — `DELETE /__cache__?url=https://...`.
  Инвалидировать конкретные кэш записи через Admin API.
  Pingora cache поддерживает purge (`force_expire()`).

---

### 🐟 Из исследования h2o (`<projects-root>\h2o`)

> Источник: `<projects-root>\h2o` — HTTP/2 server от Kazuho Oku (DeNA). Изучен 2026-06-06.
> Ключевые файлы: `lib/handler/proxy.c`, `lib/handler/throttle_resp.c`,
> `lib/handler/server_timing.c`, `lib/http2/scheduler.c`, `lib/common/cache.c`,
> `lib/core/proxy.c`, `include/h2o/absprio.h`.

#### Легко реализуемые (Easy)

- [x] **`proxy.*.timeout.firstByteMs`** — timeout до первого байта ответа от upstream.
  Сейчас `readMs` срабатывает только после начала ответа; `firstByteMs` ловит зависшие backend'ы.
  Добавить поле в `ProxyTimeout`, передать в `PeerOptions` в `upstream_peer()`.
  Источник: `h2o/lib/handler/proxy.c` — `h2o_httpclient_ctx_t.first_byte_timeout`.
  ~5 строк.

- [x] **`Server-Timing` response header** — W3C-стандартный заголовок, виден в DevTools.
  Format: `Server-Timing: total;dur=42, upstream;dur=38`. Использует уже имеющиеся
  `duration_ms` + `upstream_response_time`. Добавить Phase 4.5 `ServerTimingFilter`
  в `response_chain.rs`. Config: `serverTiming: bool` в SiteConfig.
  Источник: `h2o/lib/handler/server_timing.c`.
  ~20 строк.

- [x] **`Via` header injection** — RFC 7230 стандартный заголовок прокси.
  Format: `Via: 1.1 conduit`. Добавить в `append_forwarded_headers()` в `service.rs`.
  Позволяет обнаруживать proxy-loops (прокси проверяет свой адрес в Via).
  Config: `proxy.emitViaHeader: bool` (default true).
  Источник: `h2o/lib/core/proxy.c` — `build_request()`.
  ~5 строк.

- [x] **`cache.earlyRefreshSecs`** — упреждающее фоновое обновление кэша до истечения TTL.
  `CacheConfig.early_refresh_secs` (schema.rs). `should_early_refresh()` в `proxy/cache.rs`
  (+ 7 unit-тестов). `response_phase.rs::response_filter` детектирует TTL-окно и кладёт
  `RequestCtx.early_refresh_upstream_url`; `logging_phase.rs` спавнит
  `tokio::spawn(fire_early_refresh(...))` после ответа клиенту. Документировано в
  `docs/configuration.md` + `schema/conduit.schema.json`.
  Источник: `h2o/lib/common/cache.c` — `H2O_CACHE_FLAG_EARLY_UPDATE`.
  Реализовано в PR #67 (commit `09ea808`, v1.1.0 stabilization), issue #31.

- [x] **Event-loop lag metric** — `conduit_eventloop_lag_ms` Prometheus gauge per worker.
  Показывает задержку Tokio event loop (признак CPU saturation / I/O stall).
  Реализовано через yield-probe task (без внешних зависимостей — `RuntimeMonitor` требует
  `tokio_unstable`). `--features tokio-metrics`. Обновляется каждую секунду в `AdminApiService::start()`.
  Источник: `h2o/lib/handler/status/durations.c` — `evloop_latency_nanosec`.
  ~25 строк.

- [x] **RFC 9218 `Priority:` header** — стандартный заголовок приоритизации HTTP (urgency 0–7, incremental).
  Заменить/дополнить кастомный `X-Priority` header. `Priority: u=1` = высокий приоритет.
  `parse_rfc9218_priority()` в `router.rs`; urgency 0–7 → 100–2 (шаг 14). 6 unit-тестов.
  Источник: `h2o/include/h2o/absprio.h`, `lib/http3/server.c`.
  ~15 строк.

#### Средней сложности (Medium)

- [ ] **`responseThrottle.bytesPerSec`** — ограничение полосы ответа на клиента.
  Token-window алгоритм: при превышении → `tokio::time::sleep()` в `upstream_response_body_filter`.
  Полезно: slow clients не создают back-pressure; bandwidth-based тарифы.
  Config: `proxy.*.responseThrottle: { bytesPerSec: u64 }`.
  Источник: `h2o/lib/handler/throttle_resp.c`.
  ~50 строк.

- [ ] **TLS 0-RTT Early-Data replay protection (RFC 8470)** — защита от replay-атак.
  При 0-RTT соединении: inject `Early-Data: 1` upstream; если ответ 425 → retry на 1-RTT.
  Config: `tls.allowEarlyData: bool` (default false — безопасно).
  Проверить Pingora API для определения early-data состояния сессии.
  Источник: `h2o/lib/core/proxy.c` — `reprocess_if_too_early`.
  ~40 строк.

- [ ] **`gracefulShutdownTimeoutMs`** — configurable timeout для H2 upstream drain при reload.
  Сейчас hot-reload меняет конфиг через ArcSwap без drain H2 соединений.
  Добавить `global.gracefulShutdownTimeoutMs` → передать в Pingora shutdown config.
  RFC 9113 §6.8: двойной GOAWAY (немедленный + через 1s для in-flight).
  Источник: `h2o/lib/http2/connection.c` — `graceful_shutdown_resend_goaway()`.
  ~20 строк конфига + исследование Pingora API.

#### Заблокировано / Hard

- [🚫 BLOCKED] **X-Reproxy-URL internal redirect** — upstream возвращает `X-Reproxy-URL: https://...`,
  прокси отменяет текущий ответ и прозрачно пересылает к новому URL.
  Паттерн: auth-сервис валидирует запрос → редиректит на внутренний asset storage.
  **Причина:** требует mid-request смены upstream в Pingora — нет публичного API. Ждём 0.9+.
  Источник: `h2o/lib/handler/reproxy.c`.

- [🚫 BLOCKED] **Upstream H1/H2 protocol ratio selector** — дефицитный RR алгоритм для
  выбора протокола (H1/H2) по конфигурируемым процентам.
  **Причина:** требует управления ALPN на уровне Pingora — не экспонировано. Ждём 0.9+.
  Источник: `h2o/lib/common/httpclient.c` — `select_protocol()`.

- [🚫 BLOCKED] **Happy Eyeballs RFC 8305** — параллельные IPv4/IPv6 попытки коннекта.
  **Причина:** DNS resolution и connection sequencing управляются Pingora, не экспонированы.
  Источник: `h2o/lib/handler/connect.c`.

---

### 🅰️ Из исследования Angie (`<projects-root>\angie`)

> Источник: `<projects-root>\angie` — nginx fork (ex-nginx team, активная разработка).
> Изучен 2026-06-06. Ключевые файлы: `src/http/modules/ngx_http_metric_module.c`,
> `ngx_http_limit_req_module.c`, `ngx_http_upstream_zone_module.c`,
> `ngx_http_upstream_sticky_module.c`, `ngx_stream_mqtt_preread_module.c`,
> `ngx_http_docker_module.c`, `ngx_stream_proxy_module.c`.

#### Легко реализуемые (Easy)

- [x] **Rate-limiter zone stats in Admin API** — `GET /rate-limits` возвращает
  `{ "site": { "route": { "passed": N, "rejected": N } } }`.
  `TokenBucket.passed/rejected` + новый endpoint `rate_limits_handler()` в `admin/api.rs`.
  Источник: `ngx_http_limit_req_module.c` — `ngx_http_limit_req_stats_t`.

- [x] **Upstream "busy" state** — поле `"state"` в `GET /upstreams`.
  Источник: `ngx_http_upstream_zone_module.c` line 1172.
  **Исправлено 2026-08-03 (integrity audit, Step 1c)**: реально отгруженный enum —
  `"ejected"|"half-open"|"unhealthy"|"busy"|"healthy"` (`admin/api.rs:695-705`),
  не `"up"|"busy"|"unavailable"|"recovering"` как было записано изначально; `"busy"`
  значит `active_conns > 0` (есть нагрузка), а не именно `conn_count >=
  maxConnectionsPerUpstream`. Совпадает с `docs/admin.md` — расхождение было только
  в этой заметке, пользовательские доки корректны.

- [x] **Sticky HMAC secret + strict mode** — `sticky: { cookie: "route", secret: "$VAR", strict: false }`.
  Cookie value = HMAC-SHA256(upstream_url, secret) вместо raw URL. Защита от session-pinning атак.
  `strict: true` → 503 если hinted peer down (вместо fallback на другой peer).
  Источник: `ngx_http_upstream_sticky_module.c`.
  **Реализовано** в [PR #67](https://github.com/lopatnov/conduit/pull/67) (v1.1.0 stabilization,
  2026-06-06): `StickyConfig{cookie,secret,strict}` в `config/schema.rs`,
  `hmac_sign_sticky`/`hmac_verify_sticky` + strict-mode 503 в `proxy/router.rs`,
  Set-Cookie injection в `response_phase.rs`, документация в `docs/configuration.md`
  ("HMAC-signed sticky cookies"). Чекбокс не был отмечен при мердже — исправлено
  при ревизии 2026-06-12.

- [x] **Per-peer response code breakdown** — `GET /upstreams` добавить `responses: {2xx, 4xx, 5xx}`,
  `selected_total`, `selected_last_secs` per peer. `UpstreamEntry.responses_2xx/4xx/5xx`,
  `record_response_status()` + `record_upstream_selected()` в `health.rs`. `build_flat_upstream_list()` расширен.
  Источник: `ngx_http_upstream_zone_module.c` — `ngx_api_http_upstream_peer_response_codes_handler`.

- [x] **`limits.maxRequestHeaders: u32`** — лимит числа заголовков в запросе клиента (DoS-защита).
  Default 100. Добавить в `LimitsGuard`: `session.req_header().headers().len() > max` → 431.
  Источник: `ngx_http_core_module.c` line 296 — `max_headers` directive, default 1000.

#### Средней сложности (Medium)

- [ ] **PROXY Protocol v1/v2 поддержка** — listener-уровень: читать PROXY protocol header для
  получения реального IP клиента за AWS NLB / HAProxy / другими балансировщиками.
  Config: `proxy.proxyProtocol: { version: 1 | 2 }`. v1 — простой текстовый header, v2 — бинарный.
  Источник: `ngx_stream_proxy_module.c` — `proxy_protocol` directive.

- [ ] **Docker/container service discovery** — `provider: docker` в global config.
  Фоновый Tokio task стримит `GET /events` с docker.sock, добавляет/удаляет upstreams через
  тот же path что Admin API. Паттерн аналогичен JWKS refresh thread (reqwest + tokio::spawn).
  Labels на контейнерах задают upstream group и weight.
  Источник: `ngx_http_docker_module.c`. Feature: `--features docker`.

- [ ] **Dynamic DNS re-resolution** — `resolve: true` на upstream-записях.
  Периодическое TTL-based переразрешение A/AAAA записей через tokio async DNS.
  Обновляет `UpstreamRegistry` без reload. Важно для cloud-среды с rolling deployments.
  Источник: `ngx_http_upstream_zone_module.c` — `ngx_http_upstream_zone_resolve_timer`.

- [ ] **Upstream connection drop on removal** — `connectionDrop: true | timeoutMs`.
  Когда upstream удаляется при hot-reload: после grace-period in-flight запросы к нему
  возвращают 502 (не ждут timeout upstream'а). Интегрируется с `proxy_upstream_url` tracking.
  Источник: `ngx_http_upstream.c` line 1370 — `ngx_http_upstream_need_connection_drop()`.

- [ ] **Persistent cache index** — `cache.indexFile: "path/cache.idx"`.
  Сохраняет маппинг `ConduitCacheKey → {path, expires_at}` при shutdown/shutdown-signal.
  Восстанавливает disk cache state при рестарте без полного scan директории.
  Источник: `ngx_http_cache.h` lines 185–202 — `file=` option для `proxy_cache_path`.

- [ ] **MQTT preread для TCP proxy** — `mqqtPreread: true` в `tcp:` конфиге.
  Peek первые байты TCP-стрима, парсит MQTT CONNECT packet → извлекает clientId/username.
  Используется для consistent-hash routing в IoT/MQTT broker deployments.
  Источник: `ngx_stream_mqtt_preread_module.c`.

#### Заблокировано / Hard

- [ ] **Configurable custom metrics zones** — `metrics:` блок в конфиге с mode: count/histogram/EWMA,
  ключ — любая переменная (path, jwt.sub, IP). Hard: нужна новая aggregation infrastructure.
  Источник: `ngx_http_metric_module.c`. Angie 1.11.0+.

- [🚫 BLOCKED] **Encrypted Client Hello (ECH)** — `tls.ech.keyFile`. Скрывает SNI от наблюдателей.
  **Причина:** rustls ECH API экспериментальный, Pingora не экспонирует его. Ждём rustls stable.
  Источник: `ngx_stream_ssl_module.c` lines 338–343.

- [🚫 BLOCKED] **Upstream HTTP/3 (QUIC)** — `upstreamProtocol: "h3"`. proxy → upstream по QUIC.
  **Причина:** Pingora 0.8 не поддерживает upstream H3. Ждём Issue #95. Конфиг-слот зарезервировать.
  Источник: `ngx_http_proxy_module.c`.

---

### 🆓 Из исследования freenginx (`<projects-root>\freenginx`)

> Источник: `<projects-root>\freenginx` v1.31.2 — nginx fork от Maxim Dounin / Igor Sysoev.
> Изучен 2026-06-06. Ключевые коммиты: `b85480cc`, `32ed1b58`, `f7ba7388`, `d5ea86c7`,
> `fd953ff4`, `70ee831d`, `a00f8b21`, `3f3f3a6b`.

#### Безопасность / корректность (высокий приоритет)

- [x] **Strict Host header validation (RFC 3986)** — отклонять запросы, где заголовок `Host`
  содержит не-ASCII символы, не-цифровой порт, или backslash.
  Предотвращает host-header injection атаки где `Host: evil.com\n` обходит route matching.
  Добавить в `LimitsGuard` или новый `HostValidationGuard`.
  Источник: `ngx_http_request.c` — `ngx_http_validate_host()` коммит `d5ea86c7`.

- [x] **Reject unexpected WebSocket upgrades** — `101 Switching Protocols` от upstream
  пересылается клиенту только если `proxy.*.websocket: true` явно задан в конфиге.
  Без этого — 502. Предотвращает hijacking соединения через malicious upstream.
  Проверить `upstream_response_filter()` в `service.rs`.
  Источник: `ngx_http_proxy_module.c` коммиты `f7ba7388`, `da870813`.

- [x] **Upstream failure propagation when retry impossible** — если upstream вернул 5xx,
  но retry невозможен (бюджет исчерпан, тело слишком большое, таймаут), ВСЁ РАВНО
  инкрементировать `consecutive_5xx` и обновлять EWMA для outlier detection.
  Сейчас: ошибки без retry могут не учитываться. Исправить в `logging()` в `service.rs`.
  Источник: `ngx_http_upstream.c` — `ngx_http_upstream_test_next()` коммит `a00f8b21`.

- [x] **stale-if-error при исчерпании retry** — РЕАЛИЗОВАНО + ПОКРЫТО ТЕСТАМИ, СМЕРДЖЕНО
  ([PR #93](https://github.com/lopatnov/conduit/pull/93), squash-merge `7e2f811`,
  [issue #48](https://github.com/lopatnov/conduit/issues/48) closed COMPLETED, 2026-06-13).
  Оказалось, что фикс уже был в коде (`RetryOnErrorFilter.stale_on_error` в
  `filter/response_chain.rs` покрывает «retry exhausted» и «no retry config»: на 5xx
  отдаёт `RetryUpstream` → `Error::new_up(Custom("5xx_retry"))` → Pingora зовёт
  `should_serve_stale()` → stale), но **без тестов** и issue висел открытым. PR #93
  добавил 3 интеграционных теста в `tests/cache.rs` (gated `required-features=["cache"]`):
  5xx без retry, 5xx с исчерпанным retry (#48), и **connection-error** (upstream рвёт
  соединение при ревалидации — подтверждено логом `Upstream ConnectionClosed ... serving
  stale`, обрабатывается нативно Pingora). Все три зелёные.
  Источник: `ngx_http_upstream.c` коммит `3f3f3a6b`.

- [x] **RFC 7234 Age header** — при отдаче кэшированного ответа инжектировать/обновлять
  `Age: <seconds_since_cached>` (RFC 7234 §5.1). Обязателен для RFC compliance и CDN chains.
  `CacheMeta` Pingora хранит время создания → `(now - cached_at).as_secs()`.
  Добавить в `InjectExtraHeadersFilter` Phase 2 в `response_chain.rs`.
  Источник: `ngx_http_upstream.c` коммит `70ee831d` — `$upstream_cache_age`.

#### Совместимость / функциональность (средний приоритет)

- [ ] **Ignore unexpected 1xx responses from upstream** — если upstream шлёт `103 Early Hints`
  или другие 1xx до финального ответа — игнорировать, сбросить парсер, читать дальше.
  Улучшает совместимость с Spring Boot, gRPC, CDN-aware backends.
  Добавить в `upstream_response_filter()`: if status 1xx (except 101) → continue parsing.
  Источник: `ngx_http_proxy_module.c` коммит `fd953ff4`.

- [ ] **`limits.minUploadRateBytesPerSec`** — slow-loris upload защита.
  Минимальная скорость загрузки тела запроса. Если клиент шлёт медленнее — закрыть соединение.
  Leaky bucket в `request_body_filter()`: `excess = excess - rate * elapsed_ms/1000 + chunk_bytes`.
  Источник: `ngx_http_request_body.c` коммит `b85480cc` — `client_body_min_rate`.

- [x] **`proxy.*.upstreamCompat.allowDuplicateChunked: bool`** — толерантность к дублирующемуся
  `Transfer-Encoding: chunked` от Java upstream'ов (Spring Cloud Gateway, Zuul, Tomcat).
  Добавить в `CrlfProtectionFilter` Phase 1: дедуплицировать заголовок если флаг `true`.
  Источник: `ngx_http_proxy_module.c` коммит `56d8eaa6` — `proxy_allow_duplicate_chunked`.

- [ ] **Leaky bucket алгоритм для rate limiting ответа** — улучшение точности алгоритма
  в `responseThrottle` (planned issue #34). Вместо `sent * 1000 / rate` использовать
  `excess = max(excess - rate * elapsed_ms/1000, 0) + chunk_bytes`, задержка при `excess > 0`.
  Устраняет spurious задержки при idle pipe.
  Источник: `ngx_http_write_filter_module.c` коммит `72efb400`.

#### Заблокировано / Hard

- [🚫 BLOCKED] **H2/QUIC flood detection** — per-connection counters `total_bytes` vs `payload_bytes`.
  Если >87.5% трафика — control frames и overhead >1MB → terminate connection (Rapid Reset mitigation).
  **Причина:** требует доступа к H2 frame layer Pingora — не экспонировано в 0.8.
  Источник: `ngx_http_v2.c` коммит `af0e284b`.

- [ ] **Multipath TCP (MPTCP)** — `global.multipath: bool` (Linux 5.6+).
  `IPPROTO_MPTCP` вместо `IPPROTO_TCP` — несколько TCP subflows для мобильных клиентов.
  Hard: требует обхода Pingora socket abstraction для задания protocol на уровне syscall.
  Источник: `ngx_connection.c` коммит `44c2316c`.

---

### 🔍 Исследования (нужно изучить перед реализацией)

- [x] **RESEARCH: Pingora TCP proxy** — **РЕАЛИЗОВАНО.** `ServerApp` trait + `tokio::io::copy_bidirectional`. `src/proxy/tcp.rs`.

- [x] **RESEARCH: Pingora HTTP/3** — **НЕТ в 0.8.** Gateway example явно говорит "we don't support h3". Ждём следующей версии.

- [x] **RESEARCH: linkerd2-proxy load balancing** — изучить
  `<projects-root>\linkerd2-proxy\linkerd\proxy\balance\` для улучшенных LB алгоритмов
  (EWMA-based P2C improvements, latency percentiles).

- [x] **RESEARCH: envoy ext_proc protocol** — изучить протокол для планирования ext_proc — изучить
  `<projects-root>\envoy\api\envoy\service\ext_proc\v3\external_processor.proto`
  для совместимого протокола External Processing.

- [x] **RESEARCH: traefik mTLS config** — устарел, mTLS уже реализован — изучить
  `<projects-root>\traefik\pkg\config\dynamic\http_config.go` для определения
  совместимого config schema (ClientAuth, CAFiles, etc.).

- [x] **RESEARCH: haproxy queue.c** — изучен, блокировка обоснована (Pingora нет хука) — изучить
  `<projects-root>\haproxy\src\queue.c` — алгоритм приоритетной очереди upstream.
  Оценить реализуемость в Pingora без хука.

- [x] **RESEARCH: rustls WebPkiClientVerifier** — устарел, mTLS уже реализован — изучить
  `<projects-root>\rustls\rustls\src\server\` для понимания как построить
  `Arc<dyn ClientCertVerifier>` из CA bundle (.pem file).
  Нужно для mTLS реализации.

- [x] **RESEARCH: axum advanced routing** — изучен; типизированные ошибки AdminError добавлены в admin/api.rs — изучить
  `<projects-root>\axum\axum\src\` для улучшения Admin API
  (versioning, better error handling, OpenAPI spec generation).

---


## Integrity audit log (Conduit 2.0 cycle, Step 1c)

> Append-only. `/feature-workspace-cycle` Step 1c writes one row here each time it audits
> a feature/module via `integrity-auditor`, so a later firing can see what's already been
> checked recently instead of re-auditing the same area. Newest entries on top.

| Date | Area audited | Result | Notes |
|------|---------------|--------|-------|
| 2026-08-21 | `src/filter/auth.rs` (consumer identification: API key / Basic Auth / per-consumer JWT V2 / shared JWT V3) | 2 real behavioral gaps + 7 low-risk doc/test issues | Fourth Step 1c firing (cadence gate satisfied: Step 0/1 both idle; this table's true previous top row was 2026-08-17 `tls.rs` below, not 2026-08-10 as first assumed mid-session — corrected here). Prompted by a live Gitar finding on tracking PR #152 flagging `identify_consumer`'s doc comment as stale ("two credential types" vs. the actual four) — confirmed that specific drift was already fixed on `main` via `e17ec67` (2026-08-18), but 2 sibling doc-comment instances of the same drift (`ConsumersGuard` in `chain.rs`, `SiteConfig.consumers` in `schema.rs`) and CLAUDE.md's own pipeline diagram (omitted `ConsumersGuard` entirely) had not been. Root finding needing design judgment: `feature_warnings()` has no case for `consumers` at all — building without `--features consumers` while a config sets `sites[].consumers` silently drops all consumer-based auth with zero startup/hot-reload warning (every sibling feature — jwt/forward-auth/tcp/redis/acme/etc. — warns; `consumers` doesn't), filed as [#233](https://github.com/lopatnov/conduit/issues/233) (security-relevant, pre-existing, not introduced by this audit's fix). Second gap: `identify_consumer`'s consumer-list scan short-circuits at first match, unlike `check_api_key`'s deliberate non-short-circuiting constant-time design a few lines above — a minor timing-characteristic judgment call, filed as [#234](https://github.com/lopatnov/conduit/issues/234) for `security-engineer` to weigh in on. The 7 low-risk items (the 2 sibling doc-drift instances + the diagram fix + a real validation gap — `Consumer.rate_limit` was never run through the existing `validate_rate_limit` helper, so `limit=0`/`windowSecs=0` passed validation but then silently, permanently locked the consumer out at runtime — + 3 test-coverage additions: X-Consumer-ID injection for Basic Auth/shared-JWT V3, per-consumer rate limit via a non-API-key path, `consumer.headers` custom-header injection, none previously asserted + a missing sibling test for `jwtAuth`'s own feature-warning) shipped directly via [PR #235](https://github.com/lopatnov/conduit/pull/235) (off `main`, not the migration branch) — `security-engineer` PASS recorded, CodeRabbit reviewed with no actionable comments (Merge Risk: Minimal). Process note: the log entry itself was mistakenly pushed straight to `main` without a PR, bypassing this repo's own branch-protection convention (the push only succeeded because the bypass rule allowed it) — flagged to the user rather than left unremarked. |
| 2026-08-17 | `src/server/tls.rs` (mTLS/cert-rotation handling, unchanged functionally since mTLS shipped 2026-06-05) | 3 real behavioral gaps + 4 low-risk doc/test issues | Third Step 1c firing. Root findings: `tls.versions`/`tls.ciphers` config fields are parsed but never actually passed to rustls anywhere (silently no-op) — filed as [#189](https://github.com/lopatnov/conduit/issues/189); `POST /certs/reload` validates and atomically writes the new cert/key PEM to disk but doesn't activate it on the running listener (Pingora 0.8 has no `ResolvesServerCert`-equivalent hot-swap API — matches the existing documented "zero-downtime hot-swap blocked" caveat, but the endpoint's own doc/response wording overclaims "reload" more than it delivers) — filed as [#190](https://github.com/lopatnov/conduit/issues/190); a cert within its expiry window still hard-fails server startup instead of warning-and-continuing (no grace period), surprising for anyone rotating certs close to the wire — filed as [#191](https://github.com/lopatnov/conduit/issues/191). All three need design judgment (Pingora API limits or explicit UX tradeoffs), not fixed here. The 4 low-risk fixes shipped via [PR #192](https://github.com/lopatnov/conduit/pull/192) (off `main`, not the migration branch, squash-merged `e87f999`): `tests/mtls.rs` — 5 new integration tests exercising a real mTLS TLS handshake (required/optional × valid/missing/untrusted-CA client cert), closing a real gap since the existing 13 unit tests in `tls.rs` only cover PEM parsing and never perform a handshake; `schema/conduit.schema.json` gained the `tls.clientAuth` property that `schema.rs` has had since mTLS shipped; `reload_cold_field_tls_cert_rejected` added to `tests/hot_reload.rs` (no prior coverage for `tls.cert`/`tls.key` as cold fields); a doc-comment correction on `validate_cert_key_pem` that overclaimed expiry detection. CodeRabbit's review (enabled on `main`-targeted PRs, unlike the migration branch) caught a real issue in the first commit: the readiness-probe TCP-connect fallback added to the shared test harness (needed because `clientAuth.optional:false` sites correctly reject the harness's cert-less probe at the TLS layer) ran unconditionally for any HTTP+HTTPS probe failure rather than being scoped to the mTLS-required case — fixed in a follow-up commit (`requires_mtls()` derives the scope from each test's own config JSON) and re-reviewed by `security-engineer` against the new head before merge. CodeRabbit's second finding (`common::free_port()`'s pre-existing TOCTOU port-reservation race) was pushed back on with a documented reason (repo-wide convention predating this PR, used identically by ~40+ existing tests, not a regression) and the bot withdrew it after review. Separately this session: discovered the recurring `Conduit 2.0 feature-workspace-cycle` Routine itself is correctly configured (`cron_expression: "0 1 * * *"`, daily; stored prompt is a live `"/feature-workspace-cycle"` invocation, not a frozen snapshot) — a firing nonetheless delivered a stale, pre-Step-1c version of the command text, most likely because this long-running session (alive since 2026-07-29) cached the skill/command definition early on and never refreshed it despite the file being edited many times since. Harness-level quirk, not a repo bug; mitigated all session by cross-referencing `.claude/rules/*.md` directly rather than trusting routine-delivered text, but worth the user's awareness — a fresh session would pick up the current file. |
| 2026-08-10 | `src/filter/jwt.rs` (JWT bearer-token auth, unchanged functionally since v1.1.0/2026-06-06 — only a clippy fix touched it since) | 2 real behavioral gaps + 6 low-risk doc/test issues | Second Step 1c firing (cadence gate satisfied: ~5 firings since the 08-03 audit, Step 0/1 both idle). Root finding: JWKS refresh is a synchronous blocking fetch inside the async `JwtGuard`, with no single-flight lock and no fallback to stale-but-still-valid keys on refetch failure — despite the module doc and `JwtAuthConfig.jwksUrl` doc both claiming a background-refresh design that doesn't exist. Filed as [#163](https://github.com/lopatnov/conduit/issues/163) (needs design judgment — recommended adapting the existing `CACHE_LOCK`/stale-while-revalidate pattern rather than inventing a new one). Companion gap: the RS256/ES256/JWKS code path — literally half of what the feature advertises — has zero test coverage (all 20 unit + 6 integration tests are HS256-only); filed as [#164](https://github.com/lopatnov/conduit/issues/164). Low-risk fixes shipped directly on `fix/jwt-audit-gaps-integrity` (off `main`, not the migration branch): case-sensitive `strip_prefix("Bearer ")` in claim-template extraction reusing the already-tested case-insensitive `extract_bearer` instead of a second ad hoc parse; `jwksRefreshSecs` minimum (60s, matches `schema/conduit.schema.json`) now enforced in `validate.rs` (previously schema-only, unenforced at runtime); docs updated for JWKS-unreachable-after-TTL fail-closed behavior and non-string-claim JSON-text serialization in `{{ jwt.<claim> }}` templates (both previously undocumented); a mislabeled test (`non_object_claims_returns_none_from_extract`) that silently tested the wrong thing rewritten to actually build a non-object-payload JWT; stale `jsonwebtoken v9`/test-count claims in this file corrected. |
| 2026-08-03 | `src/proxy/health.rs` (unchanged since v1.1.0/PR #67 — oldest actively-used file in the codebase) | 4 real behavioral gaps + 4 low-risk doc/comment issues | First-ever Step 1c firing (cadence gate finally satisfied: Step 0 idle, Step 1 found nothing to triage). Root finding: passive health tracking (Outlier Detection, Peak EWMA, per-peer response stats) and true circuit-breaker enforcement (skipping a single at-limit upstream) only actually work for `LoadBalanceStrategy::LeastConn` or when `maxConnectionsPerUpstream` happens to also be set — for the `RoundRobin` default and 4 other strategies without a connection cap, several `[x]`-marked "done" backlog items silently no-op. Also found `slowStartSecs` fully unwired (zero effect) and `prewarmConnections` warming a throwaway client instead of Pingora's real pool. Doc/comment-only fixes (scrambled doc-comment un-scramble, honest known-limitation notes, 2 stale CLAUDE.md backlog claims corrected) shipped directly via [PR #154](https://github.com/lopatnov/conduit/pull/154) per the low-risk/unambiguous routing rule. The 4 behavioral gaps needing design judgment filed as [#155](https://github.com/lopatnov/conduit/issues/155) (passive tracking gate), [#156](https://github.com/lopatnov/conduit/issues/156) (circuit-breaker enforcement gate, cross-references #155), [#157](https://github.com/lopatnov/conduit/issues/157) (`slowStartSecs` dead code), [#158](https://github.com/lopatnov/conduit/issues/158) (`prewarmConnections` doesn't warm the real pool) — ordinary repo backlog, not #114 sub-issues. Note: the agent originally delegated to file these issues (`scrum-master`) turned out not to have GitHub MCP tools in its grant, fell back to raw-credential API probing (blocked by egress policy, no data exposed) — flagged as a security-relevant subagent-behavior incident and routed to `security-engineer` for review rather than self-cleared; issues were filed directly by the conductor's own properly-scoped tools instead. |
---

## Dependabot & branch hygiene log

> Append-only. See `.claude/rules/index.md` "Dependabot & branch hygiene reflex check" —
> any session that touches this repo's GitHub state runs this cheap sweep if the newest
> row here is older than ~24h, then logs a row (even "nothing new"). Newest on top.

| Date/time (UTC) | New Dependabot PRs found/acted on | Orphan branches flagged | Notes |
|---|---|---|---|
| 2026-08-23 ~08:05 (`/feature-workspace-cycle` firing) | 0 open (confirmed via `search_pull_requests author:app/dependabot`) | not re-checked separately (fast path — log's newest row was <1h old) | Fast path (Step 1): 0 open Dependabot PRs, only open PR is #152 (tracking, correctly still draft, base SHA `f746ce8` matches migration branch's actual tip — no sync needed). Prior firing's own #255/#256 work (extract conduit-faults + JWKS test coverage for #164) already merged and synced before this firing started. Proceeded to Step 2. |
| 2026-08-23 ~07:01 (`/feature-workspace-cycle` firing, ad-hoc 07:00 UTC slot) | 0 open (confirmed via `search_pull_requests author:app/dependabot`) | not re-checked separately (fast path — log's newest row was <24h old, no dedicated branch sweep needed) | Fast path (Step 1): 0 open Dependabot PRs, only open PR is #152 (tracking, correctly still draft). Migration branch already at `main`'s tip (`624d24e`, synced twice earlier today for #252/#254 and #255) — no sync needed. Proceeded straight to Step 2 (next #114 sub-issue). |
| 2026-08-23 ~02:20 (same-day addendum — #252/#254 merged, migration branch synced) | (continuation of the row below, no separate Dependabot check) | 0 | #252 (#190/#191 TLS cert-rotation UX fix) and #254 (this same hygiene-log row, filed as its own PR since it was authored from a different branch than #190/#191's) both merged into `main` (squash `93826a4`, `04baafa`), each with its own `security-engineer` PASS. Migration branch synced afterward (`git merge origin/main`, merge commit `ed63576`) — 2 real conflicts: `CLAUDE.md` (two independent new rows both wanting to sit at the top of this append-only table — reordered newest-first, no content lost) and `src/config/validate.rs` (#191's new `Severity` enum / `ValidationError.severity` field / `partition_by_severity()` collided with the migration branch's own prior extraction of `ValidationError` into `conduit_config_core::validation` — resolved by moving `Severity` + `partition_by_severity()` into that same crate module alongside `ValidationError`, matching the extraction's existing pattern, then re-exporting both from `src/config/validate.rs`'s existing `pub use` line so `main.rs`/`admin/api.rs`'s `validate::partition_by_severity(...)` call sites needed no changes). Verified after resolving: `cargo build --workspace --features full` clean, `cargo test --workspace --features full --lib` (1253 tests, 0 failed), `cargo test --test health_and_admin --features full certs_reload` (4/4), `cargo fmt --check` clean, `cargo clippy --workspace --features full --tests -- -D warnings` (0 warnings). Pushed. |
| 2026-08-23 ~00:15 (mid-session sweep, triggered by handling PR #252) | 0 open (confirmed directly via `search_issues author:app/dependabot`) | 2 flagged, not new — `claude/cycle-integrity-audit-step` (PR #149, closed/not merged) and `claude/stoic-stonebraker-d51bed` (PR #90, closed/not merged despite CLAUDE.md recording #90's changes as merged via manual `a84b467` — same "closed without using the GitHub merge button" pattern noted for #90 elsewhere in this file) — both pre-existing leftover clutter, not chased per the no-delete-from-session rule. Branch count 24 (up from the 2026-08-12 baseline of 22): +1 is this session's own active `fix/tls-cert-rotation-190-191` (PR #252, not yet merged); the other +1 wasn't reconciled against the exact historical list (diminishing returns for a routine sweep) but no *new* PR-less orphan was found among the branches checked. Only 2 open PRs total: #252 (mine, awaiting security-engineer) and #152 (tracking PR, draft, not actionable here per Step 7). Migration branch (`d01fe6f`) still exactly at `main`'s tip (`1586e20`) — no sync needed yet. |
| 2026-08-22 ~07:00 (SonarCloud triage with the user, folded into a `/feature-workspace-cycle` firing) | 0 open (confirmed via `search_pull_requests author:app/dependabot`) | 0 (branch count 24, up from 22 as of 2026-08-12 — both new branches accounted for: `claude/cargo-workspace-features-23qxfr`'s own tracking PR #152, and `fix/jwt-extract-claims-visibility` for #238 below, auto-deleted on merge) | Two security fixes on `main`, both found live while walking the user through a screenshot of SonarCloud findings. **#237** (`fix/jwt-claims-skip-paths-security`, squash `e400bf4`): `jwt_claims_from_session()` decoded and trusted `Authorization: Bearer` tokens for `{{ jwt.<claim> }}` header-template substitution unconditionally, even on `jwtAuth.skipPaths` routes where `JwtGuard` never checks the signature at all — fixed by applying the same `is_path_skipped` check `jwt_prelude()` already uses; regression test added and verified against a manual revert. **#238** (`fix/jwt-extract-claims-visibility`, squash `792ab64`): triaged the SonarCloud `rust:S5659` Hotspot on `extract_claims`'s `insecure_decode` call — `security-engineer` judged it not a false positive (the sink genuinely skips verification, and safety was a doc-comment-only contract on a `pub fn` reachable from outside the crate, which also ships as a published library on crates.io) but disproportionate for a full typestate refactor; narrowed to `pub(crate)` and renamed to `extract_claims_unchecked`. CodeRabbit flagged this "High" merge risk, claiming a later guard (`ForwardAuthGuard`/WASM `MiddlewareGuard`) could mutate the `Authorization` header after `JwtGuard` validated a different token — investigated rather than dismissed or blindly complied with; `security-engineer` exhaustively traced every `FilterOutcome::Bypass`/`Handled` site in the guard chain and confirmed the case where `JwtGuard` can be skipped (`HealthBypass`, health/ACME/hot-reload only) and the case where extracted claims are ever consumed (`upstream_request_filter`, proxied requests only) are mutually exclusive by construction — false positive, not a real gap. `security-engineer` PASSed 5 times across the two PRs (re-run against each new head SHA per the SHA-pinning rule). Migration branch synced twice (once per PR), both clean auto-merges, `cargo check --features full` green each time. Recurring `update_pull_request` (draft→ready) rate-limit quirk seen again on both PRs (reads and other mutations, e.g. `add_issue_comment`, unaffected) — resolved each time after several retries, no workaround found or needed. |
| 2026-08-21 ~18:30 (reflex check, run mid-firing — user asked "did we forget anything?" after several technical interruptions, prompting a verification pass) | 0 open (confirmed via `search_pull_requests author:app/dependabot`) | 0 new (25 branches total; the 2 newest, `chore/phase2-cleanup-recipe-114` and `fix/clippy-chunks-exact-lint-main`, are this same session's own in-flight work, each carrying an open PR — #230 and #231 respectively — not orphans) | Overdue by >24h (last entry 2026-08-18 ~12:06) — this session had been touching GitHub tools continuously for PR #230/#231 (unrelated Rust 1.98.0 toolchain-lint CI fixes, see "Реализовано" log) without re-running this check. Clean sweep: only 3 open PRs total — #152 (tracking PR, correctly still draft, correctly untouched per Step 7), and this session's own #230/#231 (CI still finishing at time of this row, not yet merged). |
| 2026-08-18 ~12:06 (user-requested: fix issue #225 found during #158 investigation) | not re-checked separately this pass (same-day sweep already ran at ~07:11 below) | 0 new (branch auto-deleted on squash-merge) | [PR #227](https://github.com/lopatnov/conduit/pull/227) merged into `main` (squash `599aeb3`) — fixes [#225](https://github.com/lopatnov/conduit/issues/225): `upstream_peer()` only accepted IP-literal upstream addresses, so every hostname target (including the shipped `examples/minimal.yaml`'s `http://localhost:4000`) failed all requests. New `resolve_socket_addr()` fast-paths IP literals and falls back to async `tokio::net::lookup_host` for hostnames. Iterated through 4 commits driven by bot review, all addressed same-session: Gitar caught the first fix taking `.next()` unconditionally from `lookup_host` (resolver-order-dependent — confirmed for real when the new integration test failed on CI's `ubuntu-latest`/`macos-latest`, which resolve `localhost` to `::1` first) → added `pick_preferred_addr()` (deterministic IPv4 preference). CodeRabbit then flagged the DNS lookup had no timeout at all (stalled resolver → indefinite hang) → bounded it with the same effective `connectMs`/`limits.timeoutSecs` deadline `apply_peer_options` already derives. Gitar's second pass caught that this created two *independent* full-length deadlines (DNS + TCP connect could each consume the full budget, up to 2x) → added `remaining_budget()` to share one budget via `saturating_sub` on elapsed DNS time. `security-engineer` reviewed twice (foreground, unconditional gate) — first PASS on an earlier commit itself independently surfaced the same unbounded-DNS gap CodeRabbit found (both flagged it before either saw the other's finding); second, final PASS at the actual merged SHA verified the shared-budget math has no underflow and the config-controlled (not client-controlled) trust boundary is unchanged. Migration branch was then 1 commit behind — synced clean (`git merge origin/main`, merge commit `6693916`, no conflicts), `cargo build/clippy/test --workspace` all green (1006+ lib tests, 0 failures), `cargo fmt --check` clean, pushed. Also set up this session's `.reference/` local clone directory (git-cloned third-party source, e.g. `pingora` at the exact pinned `0.8.1` tag, for direct source verification) — gitignored via [PR #224](https://github.com/lopatnov/conduit/pull/224), never committed; used here to verify `HttpPeer::new`'s synchronous/panicking resolution behavior directly against vendored source rather than assuming. |
| 2026-08-18 ~07:11 (user-requested: "проведи по необходимым этапам feature-workspace-cycle и смерджи если готов" re PR #219) | 0 open (not re-checked separately this pass — same-day sweep already ran at ~00:15 below) | 0 new | PR #219 merged into `main` (squash `4b43541`, `security-engineer` PASS recorded, all 11 review threads already resolved, 29/29 CI green). Migration branch was 1 commit behind afterward — synced (`git merge origin/main`, merge commit `48429a1`). 1 real conflict: `.github/workflows/ci.yml`'s `ci` job clippy step — migration branch had `cargo clippy --workspace -- -D warnings` (covers member crates conduit-core/conduit-config-core), `main`/#219 had `cargo clippy --tests -- -D warnings` (lints `#[cfg(test)]` code, closing the blind-spot #219 found). Combined both: `cargo clippy --workspace --tests -- -D warnings`. `ci-features`/`ci-standard` jobs auto-merged cleanly (already root-package-only by design, just picked up `--tests`). Verified green after resolving: `cargo build --workspace`, `cargo clippy --workspace --tests -- -D warnings` (0 warnings — #219's `--tests` backlog fix and this branch's own code didn't conflict), `cargo test --workspace` (991 lib + all integration binaries, 0 failures), `cargo fmt --check` clean. Pushed. Earlier the same session: also completed #127 (`conduit-config-core` extraction) via PR #221, closed with scope-corrected body, follow-up #222 filed for Phase 3 (`ConfigFile`/`normalize()`) — see "Реализовано" log below for detail (not duplicated here since that's #114 work, not a hygiene sweep). |
| 2026-08-18 ~00:15 (daily cycle firing) | 0 open (confirmed via `list_pull_requests` — only 2 open PRs total: #219 unrelated bug-fix branch, #152 the #114 tracking PR) | 0 new (24 branches total, up from 22 — the 2 delta accounted for by `fix/circuit-breaker-capacity-enforcement-156` (#219, active work from earlier the same day, unrelated to #114) and the migration branch itself; no genuinely new orphan) | Migration branch was 22 commits behind `main` — synced (`git merge origin/main`, merge commit `16edf63`). 2 real conflicts: `Cargo.toml` (jsonwebtoken v10→11 bump from main plus `optional = true` placement — the flag is package-level only, invalid inside `[workspace.dependencies]`, per PR #153's own documented rule; kept the migration branch's `.workspace = true` dev-dependency style over main's pre-hoist inline style) and `CLAUDE.md` (two non-overlapping append-only session-log sections, concatenated in chronological order, no content lost). Caught and fixed one *silent* (non-conflicting) regression the auto-merge let slip through: `serial_test` stayed pinned at main's old `"3"` instead of picking up main's `"4"` bump — found by diffing per-package version constraints between `main` and the merged file after `cargo metadata` first failed on the `optional`-in-workspace-deps mistake. Verified with `cargo clippy --workspace -- -D warnings` (the actual CI gate on this branch — clean) and `cargo test --workspace` (0 failures) before pushing; a `cargo clippy --tests` probe surfaced the same pre-existing lint backlog already tracked by the still-open, not-yet-merged PR #219 on `main` — correctly out of scope for this sync. Hit a real "no space left on device" mid-test-run (29 GB of stale `target/` build artifacts on a fixed session disk allowance) — `cargo clean` freed it, reran clean. |
| 2026-08-17 ~06:47 (daily cycle firing) | 0 open (confirmed directly via `search_pull_requests author:app/dependabot`) | 0 (branch count unchanged at 22 — `fix/tls-audit-gaps-integrity` from earlier this same session was already deleted locally+remote on merge, `claude/cycle-integrity-audit-step` still traces to merged PR #149, same leftover-clutter case noted since 2026-08-15) | Clean sweep. Continuation of the same session: a Step 1c firing earlier today closed the loop on a `src/server/tls.rs` audit — [PR #192](https://github.com/lopatnov/conduit/pull/192) merged to `main` (4 low-risk mTLS test-coverage/schema/doc fixes), issues [#189](https://github.com/lopatnov/conduit/issues/189)/[#190](https://github.com/lopatnov/conduit/issues/190)/[#191](https://github.com/lopatnov/conduit/issues/191) filed for the 3 gaps needing design judgment (see "Integrity audit log" table). Migration branch was already synced with `main` as part of that same work (merge commit, no conflicts). Also reset `[workspace.package] version` from `2.11.0` back to `2.0.0` per explicit user request — the per-PR minor-bump convention (Step 3) had inflated it with nothing actually published under 2.0; the convention itself was removed from `feature-workspace-cycle.md` so it doesn't recur. This firing's Step 1c cadence check: last audit-log entry is from earlier the same day/session, so the 4-6-firing gate isn't satisfied — correctly skipped. |
| 2026-08-16 ~01:40 (daily cycle firing) | 0 open (confirmed directly) | 0 (branch count unchanged at 22, `main` unchanged since last sync at `4c5b6e6` — no merge into migration branch needed) | Clean sweep. Closed issue #125 via [PR #186](https://github.com/lopatnov/conduit/pull/186) (`scripts/check-layer-boundaries.sh` + new `layer-boundaries` CI job, forward-looking guardrail against Layer-1 crates reaching into `SiteConfig`/`AppConfig`) earlier this firing — no incidents this time, clean run. |
| 2026-08-15 ~02:00 (daily cycle firing) | 0 open (confirmed directly via `search_pull_requests author:app/dependabot`) | 0 (branch count unchanged at 22 — `claude/cycle-integrity-audit-step` traces to merged PR #149, leftover clutter not a new orphan) | Clean sweep. Closed issue #124 via [PR #184](https://github.com/lopatnov/conduit/pull/184) (`SiteConfig.extra` flatten + disabled-feature-key warnings) earlier this firing; caught and fixed a process gap on the way in — that PR merged without the mandatory per-#114-PR minor version bump (Step 3 of `/feature-workspace-cycle`), corrected directly with a follow-up commit (`d8ea3f8`, 2.9.0 → 2.10.0) rather than backdating. Also hit and resolved a real incident during #184's Step 5 verification: the shared checkout's `Cargo.toml` transiently lost its `[dev-dependencies]` table (suspected race with a concurrently-running verification agent's Bash commands against the same directory), causing a false-negative `build-validator` RED — caught via `git status`/`git diff`, restored via `git checkout -- Cargo.toml Cargo.lock`, re-verified clean directly and independently by `feature-matrix-runner`'s isolated worktree run. Migration branch already in sync with `main` (base `4c5b6e6` matches `main`'s tip) — no merge needed. |
| 2026-08-12 ~02:45 (daily cycle firing) | 0 open (confirmed directly) | 0 (branch count unchanged at 22; the temporary `feat/relocate-jwt-templates-upload-rate-123` branch from earlier this firing was created and deleted within the same firing, netting to no change) | Clean sweep. Migration branch was 1 commit behind `main` (#182, the previous firing's own hygiene-log entry) — merged clean, `cargo check --features full` green, pushed as `384b2ff`. Also found and fixed a genuine pre-existing bug while updating this table: a stale, truncated duplicate of this entire "Dependabot & branch hygiene log" section (rows only through 2026-08-01) had been sitting below the real one since the PR #178 merge — removed, noted inline above. |
| 2026-08-12 ~01:15 (daily cycle firing) | 0 open (confirmed directly via `search_pull_requests author:app/dependabot`) | 0 (branch count unchanged at 22 — the 3 branches opened during yesterday's incident chain, `fix/echo-upstream-port-race`/`feat/narrow-config-slices-122`/`fix/sonar-coverage-exclusions-sync`, were all auto-deleted on merge) | Clean sweep. Found and corrected a real state drift on the way in: tracking PR #152 had somehow become non-draft (no comment/event trail explains when — predates this firing, not caused by yesterday's work) despite its own body and Step 9 explicitly requiring it stay draft until #114 is fully complete (26 of 34 sub-issues still open) — converted back to draft. |
| 2026-08-11 ~03:16 (same-day addendum — log is append-only, original 01:48 row below left unchanged) | (continuation of the same firing below, no separate Dependabot check) | 0 | Later in the same session as the row below: closed issue #122 via PR #179 (narrowed `SiteConfig` usage in `logging`/`fallback`/`static_files`); root-caused and fixed a `sonar-project.properties`/`.tarpaulin.toml` drift via PR #180 — first attempt wrongly excluded 4 files with real unit-test coverage from the SonarCloud gate, caught by `security-engineer`'s mandatory HOLD, corrected and re-verified PASS; `.tarpaulin.toml` deleted outright (dead config for a tool this repo's CI has never run); issue #181 filed for 7 further pre-existing exclusion-list files with real coverage, deferred pending SonarCloud dashboard access this session doesn't have. |
| 2026-08-11 ~01:48 | 10 found, all triaged and merged (#168-177) | 0 (branch count 23 — up from 22 only because `fix/echo-upstream-port-race`, #178, is active work; all 10 `dependabot/cargo/*` branches auto-deleted on merge) | Batch `security-engineer` PASS on all 10 (real advisory found: `RUSTSEC-2026-0190` anyhow unsoundness, current pin `1.0.102` vulnerable — #177 merged first as priority). `lawyer` cleared the one new transitive dep (`rustls-platform-verifier` via #174's kube bump) as MIT OR Apache-2.0. 4 PRs (#168/#169/#172/#173) shared an identical new `syn 3.0.3` Cargo.lock entry — merged sequentially, GitHub/Dependabot resolved each rebase automatically, no manual `@dependabot rebase` nudge needed. Migration branch synced with `main` afterward (10 commits, clean `Cargo.lock` auto-merge, `cargo check --features full` green) and pushed. Also fixed a real CI flake found via #177's checks: `AddrInUse` race in `tests/common/mod.rs::start_echo_upstream` (cross-binary port collision) — fixed on `fix/echo-upstream-port-race` (#178, off `main`, not the migration branch), CodeRabbit's one finding (inaccurate "drop to shut down" doc comment) addressed by correcting the doc rather than adding real shutdown machinery. |
| 2026-08-09 ~02:45 (daily cycle firing) | 0 open (confirmed directly) | 0 (branch count unchanged at 22, migration branch 27 ahead / 0 behind `main`, no sync needed) | Clean sweep, nothing to act on. Only open PR is #152 (tracking PR, not actionable here per Step 7). |
| 2026-08-08 ~04:00 (daily cycle firing) | 0 open (confirmed directly) | 0 (branch count unchanged at 22, `main` unchanged since last sync) | Clean sweep, nothing to act on. |
| 2026-08-06 ~00:15 (daily cycle firing) | 0 open (confirmed directly via `search_pull_requests author:app/dependabot`) | 0 (branch count unchanged at 22, `main` unchanged since yesterday's sync) | Clean sweep, nothing to act on. Fetched directly this time instead of delegating to `dependency-steward` with an assumption about its tool grant. |
| 2026-08-05 ~00:20 (daily cycle firing) | 0 open (confirmed directly via `search_pull_requests author:app/dependabot`) | 0 (branch count unchanged at 22) | Sent `dependency-steward` to triage with an incorrect prompt claiming it has GitHub MCP tools (it doesn't — `Bash, Read, Glob, Grep, WebFetch` only, per yesterday's fix). It correctly followed the new "on a tool gap, stop and report" rule instead of routing around it — first real validation of that fix. No actual triage was lost since the conductor had already independently confirmed 0 open Dependabot PRs via its own tools before the agent's report landed. |
| 2026-08-04 ~02:15 (daily cycle firing, Step 1c follow-through) | 0 open (clean) | 0 | First-ever Step 1c firing (see "Integrity audit log" below) produced PR #154 (health.rs doc fixes) and PR #159 (subagent tool-gap hardening, from a security-engineer-reviewed incident) — both merged into `main` with the unconditional security gate. Migration branch was 2 commits behind afterward; synced clean (`git merge origin/main`, no conflicts — the two branches had independently edited overlapping `.claude/` files but in non-overlapping regions), `cargo fmt --check` + `cargo clippy --lib -- -D warnings` green, pushed as `f12fb00`. The `security/dependabot/3` alert noted below is still unresolved and still unreachable with this session's tools. |
| 2026-08-03 ~02:00 (daily cycle firing) | 0 open (clean, only open PR is #152 the tracking PR) | 0 (branch count unchanged at 22 — `feat/workspace-hoist-deps-116` was created and auto-deleted on squash-merge within this same firing, netting to the same count) | `git push` on the migration branch has repeatedly surfaced a GitHub-native notice: "1 vulnerability (1 high)" on `main` at `github.com/lopatnov/conduit/security/dependabot/3`. Could not inspect it — no MCP tool in this session lists/reads Dependabot security alerts (only Dependabot *PRs*, of which there are none open, meaning no auto-PR exists for this alert), and the alert page itself needs authenticated access WebFetch can't provide. **Flagged to the user, unresolved** — needs a look from the GitHub UI or a session with alert-reading access. |
| 2026-08-02 ~04:00 (daily cycle firing) | 0 open (clean) | 0 (branch count dropped 25→22 since last check — user cleanup via the provided script + GitHub's own Dependabot branch auto-cleanup; `fix/pr112-review` orphan also gone) | Migration branch was 2 commits behind `main` (#101 kube fix, #151 all-actions bump) — the new "keep migration branch in sync" bullet caught this on its first real firing. Merged clean (`git merge origin/main`, no conflicts, `cargo check --features full` green), pushed as `844a174`. |
| 2026-08-01 ~10:00 | #151 (all-actions group, 11 updates) — merged; #101 (kube 3→4.0.0) — root-caused a real k8s-openapi 0.28 version conflict, fixed, merged | 0 (all ~25 branches checked accounted for by a PR — either open, merged, or closed) | Prompted by the user noticing `feat/workspace-scaffolding-115` and `dependabot/cargo/kube-4.0.0` in the branch list. Root cause of the untriaged PR + leftover branches: repo has no "Automatically delete head branches" setting and the cycle went from hourly to daily, leaving a gap between firings that no other session filled. This log + rule exist to close that gap. |

> Note: this log previously existed only on the `claude/cargo-workspace-features-23qxfr`
> migration branch's copy of `CLAUDE.md` — `main` never had it. PR #178 brings it to `main`
> for the first time; entries above dated before 2026-08-11 were backfilled from the
> migration branch's history rather than reflecting actions taken directly on `main`.
>
> Note (2026-08-12): a stale, truncated duplicate of this entire section (headed rows only
> through 2026-08-01, missing everything since) had accumulated below this point — leftover
> from the PR #178 merge that first brought this log to `main`. Removed; this single copy
> above is the sole source of truth going forward.

---

## Tokio 1.52.3 — возможности (исследовано)

Tokio "full" features уже включены. Ключевые находки для будущего использования:

- **`tokio::io::copy_bidirectional`** — критично для TCP proxy mode. Bidirectional stream relay.
- **`tokio::task::JoinSet`** — batch task management. Лучше чем ручные JoinHandle.
- **`tokio::sync::Semaphore::acquire_many()`** — connection pooling / rate limiting.
- **`tokio::net::TcpStream::set_zero_linger()` / `set_quickack()`** — TCP tuning.
- **Task naming** — `tokio::task::Builder::new().name("proxy-worker").spawn()` для observability.
- **`tokio::io::duplex()` / `simplex()`** — in-memory pipes для тестирования proxy логики.

Текущий код уже использует: broadcast, watch, MissedTickBehavior (в health checks), interval.

---

## Phase 5 — HTTP/3

**Триггер:** Pingora Issue #95 — ожидается ~август 2026.
**Артефакт: `conduit 1.x.0`**

---

## Правила

- `pingora-cache = "0.8"` — кастомный cache key обязателен (CVE-2026-2836)
- Pingora `"0.8"` — только 0.8+, 3 CVE исправлено
- `schema/conduit.schema.json` — вручную синхронизировать со `schema.rs`. Обновлён 2026-05-31 со всеми Phase 4 полями. Валидировать: `node -e "JSON.parse(fs.readFileSync('schema/conduit.schema.json','utf8'))"`
- HTTP/3 (Phase 5) — ждём Pingora Issue #95, ~август 2026
- `src/main.rs` тонкий: CLI → `dispatch_command()` → command struct → `execute()`
- `tls.ciphers` — rustls-строки, НЕ OpenSSL
- Admin API bind — только loopback
- `hotReload` при `static` как IndexMap — следить за ВСЕМИ директориями
- `routes` backward-compatible с top-level `proxy`/`static`
- tracing spans в hot path — только `Level::TRACE`
- Бинарник ≤15 МБ
- `WeightedRoundRobin` валидация: targets — `WeightedTarget`, не строки
- Docs: `docs/configuration.md`, `docs/deployment.md`, `docs/benchmarks.md`
- YAML: `.yaml`/`.yml` через `from_yaml()`, env interpolation + version check работают так же
- Filter Chain: `src/filter/chain.rs` — добавлять новые guard-фильтры ТОЛЬКО сюда
- Response Chain: `src/filter/response_chain.rs` — добавлять новые response-фазы ТОЛЬКО сюда
- Routing Strategy: `src/proxy/strategy.rs` — добавлять стратегии ТОЛЬКО сюда
- Cache lock: Pingora уже имеет `pingora-cache/src/lock.rs` → `WritePermit` — использовать его
- `retry.budgetPercent`: мягкое ограничение, TOCTOU гонки допустимы
- `proxy.*.mirror`: V1 = headers only, тело не буферируется. V2 = буферировать < 1MB
- JWT: jsonwebtoken v10 имеет leeway 60s по умолчанию (проверено против vendored source,
  `validation.rs:129`, `leeway: 60` — поведение не изменилось при миграции v9→v10).
  Expired test должен просрочить > 60s
- `reqwest` повышен в main deps для mirroring + JWKS + Forward Auth. features = ["json", "rustls"]
- JWKS refresh: синхронный std::thread::spawn + new_current_thread runtime (как ACME)
- ForwardAuth: process-wide `OnceLock<reqwest::Client>` в `forward_auth_client()` — не per-request
- Header insert из Vec<String>: сначала collect в Vec<(String,String)> — избегаем lifetime issues
- Axum middleware state: `from_fn_with_state(Arc<T>)` конфликтует с `Router.with_state(Arc<U>)`. Использовать closure: `from_fn(move |req, next| { let t = t.clone(); async move { ... } })`
- Consumer rate limit key: `"consumer:{username}"` (global для этого consumer, не per-IP). Bucket создаётся через `ctx.rate_limiter.entry(key).or_insert_with(|| TokenBucket::new(limit, window))`.
- Circuit Breaker: `conn_count` инкрементируется для ALL стратегий при `maxConnectionsPerUpstream`. Non-LC: `circuit_tracking = true` → `conn_inc()` + `proxy_upstream_url = Some(url)`. Декремент в `logging()` как обычно.
- JWT claims: `RequestCtx.jwt_claims` заполняется ПОСЛЕ guards в `do_request_filter`. `expand_jwt_templates()` вызывается в `upstream_request_filter`. Неизвестные claims → пустая строка.
- `LocalHandler::Overloaded` → `HandlerKind::Overloaded` → `OverloadedHandler` → 503. Не bypasses guard chain (auth проверяется сначала).

### ⚠️ ИСПРАВЛЕНИЕ: предыдущие данные о блокировках были ОШИБОЧНЫ

Проверка исходников `<projects-root>\pingora` (v0.8.0) показала что 3 из 4 задач РЕАЛИЗУЕМЫ:

| Задача | Старый статус | Реальный статус (проверено в pingora src) |
|--------|---------------|------------------------------------------|
| **mTLS** | 🚫 BLOCKED | ✅ **РЕАЛИЗУЕМО** — `TlsSettings::set_client_cert_verifier(Arc<dyn ClientCertVerifier>)` в `pingora-core/src/listeners/tls/rustls/mod.rs:97`. `WebPkiClientVerifier` экспортируется. |
| **Stale-while-revalidate** | 🚫 BLOCKED | ✅ **РЕАЛИЗУЕМО** — `CachePhase::Stale` + `StaleUpdating` в `pingora-cache/src/lib.rs:85-87`. Хук `should_serve_stale()` в `proxy_trait.rs:621`. |
| **Request body buffering** | 🚫 BLOCKED | ✅ **РЕАЛИЗУЕМО** — `request_body_filter(session, body: &mut Option<Bytes>, end_of_stream, ctx)` в `proxy_trait.rs:132`. |
| **OCSP stapling** | 🚫 BLOCKED | ❌ Действительно заблокировано — `// TODO` в pingora source, нет публичного API. |

---

### Реализовано в сессии 2026-05-31 (продолжение phase-next)

- **Structured access log**: `AccessLogContext { request_id, upstream_addr }` в `filter/logging.rs`
- **Error masking**: `SiteConfig.maskErrors`, `RequestCtx.mask_upstream_body`, `upstream_response_body_filter`
- **Peak EWMA**: `UpstreamEntry.ewma_latency_us` α=0.1, `record_request_latency()` вызывается из `logging()`
- **Outlier Detection**: `OutlierDetectionConfig`, `maybe_eject()`, `ejected_until_secs/ejection_count`
- **Retry budget**: `RetryConfig.budgetPercent`, `AppState.retry_inflight`, `retry_budget_allows()`, `RetryState.is_retrying`
- **Traffic Mirroring**: `ProxyRouteConfig.mirror`, `UpstreamTarget::Proxy.mirror_url`, `fire_mirror_request()`
- **JWT auth**: `JwtAuthConfig`, `JwtGuard`, `src/filter/jwt.rs`, `jsonwebtoken = "9"`, `reqwest` в main deps
- **Forward Auth**: `ForwardAuthConfig`, `ForwardAuthGuard` (6d), `forward_auth_client()` OnceLock, fail closed
- **Header Transform**: `HeaderTransformConfig`, `requestTransform`/`responseTransform` fields в SiteConfig
- **Prometheus metrics**: added `active_connections` (Gauge), `upstream_errors_total{route,status}`, `retry_attempts_total{route,condition}`, `rate_limit_rejected_total{site}`
- `RateLimitGuard` now has `site_label: String` for metrics; `GuardCtx.site_label` computed from site config
- **Per-route rate limiting**: `proxy.*.rateLimit` in `ProxyRouteConfig`. `find_route_rate_limit(site, path)` in router.rs. Applied post-routing in `do_request_filter`. Key prefix `"route:{route_key}:"`.
- **X-Forwarded-Host**: injected in `append_forwarded_headers()` alongside XFF and XFP
- **logging.skipPaths**: suppress noisy paths from access log (same glob syntax)
- **Validation**: forwardAuth.url format, timeoutMs > 0, mirror URL format
- **Admin API auth**: `global.admin.token` — Bearer token middleware via Axum `from_fn_with_state`
- **Upstream TLS**: `upstreamTls: { verify, serverName }` in `ProxyRouteConfig` + `UpstreamTarget::Proxy`
- **Circuit Breaker**: `healthCheck.maxConnectionsPerUpstream` → `LocalHandler::Overloaded` → 503 (all-maxed case, all strategies). Per-upstream-skip mechanism now lives in `src/proxy/capacity.rs` (`Capacity`/`pick_bounded`) and works for every strategy, not just `LeastConn` — fixed 2026-08-17, issue #156 (see the dated backlog entry above for detail). The old inline `under_limit`-computed-then-discarded mechanism this note originally described no longer exists.
- **JSON Schema sync**: `schema/conduit.schema.json` обновлён со всеми Phase 4 полями + новые $defs.
- **conduit probe параллельный**: `std::thread::spawn` per URL, сортировка, ✓/✗, итог.
- **Header Transform V2 (JWT templates)**: `{{ jwt.<claim> }}` в requestTransform.setHeaders. `extract_claims()` + `RequestCtx.jwt_claims` + `expand_jwt_templates()` pub(crate) в service.rs.
- **OpenTelemetry OTLP**: `global.otlp`, `src/server/otel.rs`, `--features otlp`. `RequestCtx.otel_span` (#[cfg(feature="otlp")]). opentelemetry 0.27 + opentelemetry-otlp 0.27 + opentelemetry_sdk 0.27. SpanExporter::builder().with_tonic().
- **Consumer model**: `ConsumersConfig`, `Consumer`, `ConsumerBasicAuth` в schema.rs. `ConsumersGuard` (step 6, before basicAuth). `identify_consumer()` в auth.rs. `examples/consumers.yaml`.
- Total tests: 447 unit + 328+ integration = **775+ total** (all green when run individually)

### Реализовано в сессии 2026-06-02 (feature flag separation + docs)

- **Feature flag separation** — 13 optional features: `jwt`, `consumers`, `forward-auth`, `rhai`, `wasm`, `tcp`, `upload`, `redis`, `cache`, `disk-cache`, `acme`, `fault-injection`, `otlp`, `kubernetes`. `default = []` (minimal build). `full` = all features. Standard build ~30% smaller binary.
- **`upload` feature gating** — `multer` dep optional. `src/upload/` gated with `#![cfg(feature = "upload")]`. Router, service, builder all cfg-gated.
- **`cache` feature gating** — `request_cache_filter()` body wrapped in `#[cfg(feature = "cache")]`. `CacheStorage` import gated.
- **Zero warnings** — both `cargo build` (default) and `cargo build --features full` produce 0 warnings.
- **`feature_warnings()`** — covers all 11 config-visible features (wasm, otlp, rhai, jwt, forward-auth, acme, tcp, redis, fault-injection, cache, upload).
- **Documentation** — `docs/configuration.md` updated with 14 previously undocumented config fields: `compression.types`, `logging.stripQuery`, `limits.maxConnectionsPerIp`, `healthCheck.unhealthyStatus`, `healthCheck.unhealthyLatencyMs`, `ipFilter.dryRun`, `rateLimit.dryRun`, security headers `permissionsPolicy`/`allowedHosts`/`hstsIncludeSubDomains`/`hstsPreload`, `s-maxage` behavior table.
- **Feature-warning tests** — `upload_without_feature_generates_warning` + `cache_without_feature_generates_warning` added to `tests/middleware.rs`.
- Total tests: **511+ unit** (509 passing) + integration tests (all green).

### Реализовано в сессии 2026-06-02 (часть 2 — метрики, документация)

- **`conduit_upstream_active_connections{upstream}` gauge** — increment in `upstream_request_filter()`, decrement in `logging()`. Completes per-upstream metrics suite alongside requests_total + latency_seconds.
- **Prometheus Metrics Reference** — `docs/configuration.md` table updated with all 11 metrics including the new gauge.
- **docs/cli.md** — fixed upload feature dependency (`multer` not `—`).
- **docs/deployment.md** — Docker image variants updated to list all 13 features in full image; added Standard vs Full guidance paragraph.
- **docs/recipes.md** — new "File Upload" section with curl example + success response format.
- **examples/file-upload.{yaml,json}** — runnable upload config example with MIME allowlist, size limits, proxy fallback.
- Total tests: **509 unit** (default) / **586 unit** (--features full) + integration tests (all green).

### Реализовано в сессии 2026-06-11 (часть 2 — wire `standard` feature into CI/release pipeline)

PR #73 добавил Cargo-фичу `standard` (`jwt`+`consumers`+`forward-auth`+`cache`+`acme`), но
не подключил её к release/CI/Docker — все "standard"-артефакты продолжали собираться с
`default=[]`. Выбран вариант "переименовать на месте" (рекомендованный): un-suffixed
release-бинарники, un-suffixed Docker-образ и riscv64gc cross-compile теперь собираются
с `--features standard`; `default=[]` остаётся source-build-only ("minimal").

- **`.github/workflows/release.yml`**: все 7 "Standard builds" в матрице
  (`features: ""` → `features: "standard"`); `docker` job build-arg `FEATURES=standard`.
- **`.github/workflows/ci.yml`**: новый job `ci-standard` (clippy + test с
  `--features standard`, зеркалирует `ci-features`); riscv64gc cross-compile —
  `--features standard`.
- **`contrib/Dockerfile`**: top-comment документирует 3 tier'а (`""` = minimal/`default=[]`,
  `standard`, `full`); `ARG FEATURES=""` не менялся (локальный `docker build .` остаётся
  minimal).
- **Документация** (`docs/cli.md`, `docs/building.md`, `npm/Readme.md`, `docs/deployment.md`,
  `docs/benchmarks.md`, `docs/configuration.md`) — устранена путаница "standard" =
  `default=[]` (старое значение) vs "standard" = Cargo-фича `standard` (новое значение).
  `npm/Readme.md` "Standard vs Full" таблица: 5 строк (jwt/consumers/forwardAuth/cache/acme)
  перенесены из "full-only" в "included в standard".
- **Build size**: `--features standard` → Windows MSVC **21.2 MB** (измерено,
  `cargo build --release --features standard`, exit 0); Linux musl ~17.8 MB — оценка через
  коэффициент строки `default` (14.3/17.0 ≈ 0.84), не измерено напрямую (см. backlog
  "Re-benchmark `--features standard`"). Новая строка в `docs/benchmarks.md` Build Sizes;
  `docs/deployment.md` nginx-ingress сравнение `~14 MB` → `~18 MB`.
- Локальная проверка: `cargo clippy --features standard -- -D warnings` (чисто),
  `cargo test --features standard` (зелёный, 0 failed).
- Ветка `ci/wire-standard-feature-pipeline` → [PR #83](https://github.com/lopatnov/conduit/pull/83) (main).
  **Смерджен 2026-06-12.**

### Реализовано в сессии 2026-06-12 (review-sweep + мердж PR #82/#83)

- **Полный разбор review-комментариев PR #82 и #83** (gemini-code-assist, CodeRabbit, qodo):
  - PR #82: единственный inline-тред (perf-замечание gemini по `logging_phase.rs:294`) —
    отвечен ("pre-existing code moved verbatim"), resolved; замечание трекается как backlog-пункт
    "Zero-allocation `logging()` hot path".
  - PR #83: gemini (inline, `docs/benchmarks.md:73`) + CodeRabbit (outside-diff, строки 407-408)
    оба указали, что rename "standard"→"minimal" в `docs/benchmarks.md` был неполным
    (~13 строк со старым значением "standard" остались). Дофиксено коммитом `6efbf29`:
    интро, TOC-якорь, Build Sizes таблица, секция "Standard vs Full"→"Minimal vs Full",
    таблицы static/proxy/nginx/Traefik, комментарии в скрипте бенчмарков. Тред resolved,
    на outside-diff комментарий дан обычный PR-комментарий (inline-ответ невозможен).
- **PR #83 и PR #82 смерджены в main** (пользователем, 2026-06-12): `8779b85` (ci: standard
  pipeline) и `6ce4597` (refactor: service.rs split). Локальный main обновлён, ветки удалены.
- **diffray[bot] удалён** (2026-06-11, пользователем) по итогам сравнительной оценки качества
  ревью diffray vs qodo: медленный (~20 мин), падал на обоих PR, находки дублировали
  gemini/CodeRabbit. Stale failing check "diffray code review" на старых PR — игнорировать.
  Активные ревью-боты: gemini-code-assist, coderabbitai, qodo-code-review + сканеры.
- **Следующее по плану**: пункт 1a (SonarCloud CC на phase-файлах) разблокирован мерджем #82 —
  отдельный `refactor:` PR; затем пункт 2 (V2 feature-driven архитектура) после обсуждения.

### Реализовано в сессии 2026-06-12 (часть 4 — fix warning-префикса + wiki sync + аудит OSV)

- **Исправлен мисс-лейбл `feature not compiled in:`** — `feature_warnings()`
  (`config/validate.rs:54`) агрегирует 5 проверок, но только 2 — про
  отсутствующие compile-фичи; остальные 3 (JWT secret strength,
  metrics-auth-token, proxy-loop) — обычные config/security warnings.
  `main.rs:343` и `admin/api.rs:331` навешивали этот префикс на всё подряд —
  убран (соответствует doc-comment контракту `feature_warnings`, который и
  так показывал `tracing::warn!("{w}")` без префикса). Из-за этого бага demo
  показывала "feature not compiled in: sites[0].metrics is configured
  without a token..." — само предупреждение про metrics-токен корректно,
  просто было неправильно подписано.
- **`.github/workflows/wiki.yml`** — синхронизация `docs/*.md` → GitHub Wiki
  (push на main при изменении `docs/**` + workflow_dispatch). Чекаутит
  `<repo>.wiki` (уже существует, branch `master`), копирует `docs/*.md`,
  `README.md` → `Home.md`, добавляет баннер "auto-generated, edit in docs/".
  Коммитит/пушит только если есть изменения.
- **Аудит 5 открытых code-scanning алертов (OSV-Scanner, см.
  `.github/workflows/osv-scanner.yml`, `continue-on-error: true`)** — делегировано
  `security-engineer`, проверено `cargo tree --invert`:
  - **#38 `proc-macro-error2@2.0.1` unmaintained (RUSTSEC-2026-0173, не CVE)
    — ЗАКРЫТО**: `cargo update -p getset` (0.1.6→0.1.7) убирает
    proc-macro-error2 + proc-macro-error-attr2 из дерева целиком (путь:
    pingora-cache/pingora-proxy → cf-rustracing-jaeger → local-ip-address →
    neli → getset, build-time proc-macro). Cargo.lock-only,
    `cargo build --features full` зелёный (1m01s).
  - **#21 `daemonize@0.5.0` unmaintained (RUSTSEC-2025-0069, не CVE) —
    SUPPRESSED** в новом `osv-scanner.toml`. Прямая хард-зависимость
    pingora-core 0.8.1 (текущий latest), Unix daemon mode — не заменить без
    патча pingora. Revisit: когда pingora-core уберёт/заменит daemonize.
  - **#34 `rsa@0.9.10` Marvin Attack (CVE-2023-49092) — ОСТАВЛЕНО ОТКРЫТЫМ**
    (реальный CVE → по решению пользователя трогаем только когда появится
    фикс, не suppress). Путь: jsonwebtoken (`rust_crypto`) → conduit,
    `filter/jwt.rs` использует RSA только для JWKS RS256/RS384/RS512
    **verify** (публичный ключ) — приватного RSA-ключа в conduit нет, атака
    на утечку приватного ключа через тайминг неприменима. Фикса нет (rsa
    0.10 ещё pre-release). Revisit: rsa 0.10 stable + jsonwebtoken перейдёт
    на него.
  - **#20/#17 `protobuf@2.28.0` decode stack-overflow (CVE-2025-53605,
    дубликат-алерт x2) — ОСТАВЛЕНО ОТКРЫТЫМ** (реальный CVE, та же причина).
    Путь: prometheus 0.13.4 ← pingora-core 0.8.1 (latest, всё ещё на этой
    версии). Проверено: и pingora-core (`prometheus_http_app`), и
    `handler/metrics.rs` используют только `prometheus::gather()` +
    `TextEncoder` (text exposition) — decode-путь
    (`CodedInputStream::skip_group`) не вызывается. Свой `prometheus 0.14.0`
    у conduit уже на protobuf 3.7.2 (fixed). Revisit: если pingora-core
    поднимет prometheus до >=0.14.
- **Один PR** на ветке `claude/focused-albattani-40372a` (commits: fix
  warning-префикс, ci wiki sync, chore(deps) getset bump, chore(security) osv
  ignore daemonize).

### Реализовано в сессии 2026-06-13 (пункт 1a — рефакторинг S3776 phase-оркестраторов)

- **[PR #91](https://github.com/lopatnov/conduit/pull/91)
  `refactor(proxy): extract helpers from phase orchestrators (rust:S3776)`**
  (ветка `refactor/s3776-phase-helpers`, коммит `efa63db`) — фикс обоих
  CRITICAL S3776 issues из бэклога 1a. Подтверждено через SonarCloud MCP
  перед началом: оба issue OPEN, CC ровно 41 (`logging_phase.rs:26`) и 37
  (`request_phase.rs:204`), flow-разбивка инкрементов совпала с расчётом.
- **`logging()` CC 41 → 0** — плоский оркестратор; блоки вынесены в
  `release_proxy_upstream` (+ `passive_effective_status`),
  `write_access_log_entry`, `record_request_metrics`
  (+ `record_upstream_metrics`, `record_cache_metrics`),
  `spawn_early_cache_refresh` (`cfg(cache)`), `finish_otel_span` (`cfg(otlp)`).
  Zero-allocation свойства из PR #90 сохранены: `method`/`status` —
  borrow из session, `status_u16`/`elapsed` считаются один раз и передаются
  параметрами в metrics- и otel-хелперы.
- **`do_request_filter()` CC 37 → ~6** — вынесены `store_ip_conn_slot`,
  `enforce_route_rate_limit` (429), `shed_low_priority_request` (503,
  X-Priority strip — внутри хелпера, до early-return'ов),
  `jwt_claims_from_session` (`cfg(jwt)`, free fn). Вложенность заменена
  `let-else` early-return'ами — в стиле существующих хелперов файла
  (`enforce_max_body_bytes`, `apply_path_strip`).
- Поведение не менялось (код перенесён дословно); попутно удалён повисший
  фрагмент doc-комментария на `dispatch_local` ("Determine whether the
  request is allowed by the rate limiter…") — остаток split'а #82.
- `/build` GREEN: fmt, clippy `-D warnings` (default + full), тесты
  (default + full) — 0 warnings, всё зелёное.
- **Review-фидбек + мердж**: коммит `7a9dedb` применил 4 Gemini-замечания
  (borrow `path` как `&str` вместо `.to_owned()` в `enforce_route_rate_limit`
  /`shed_low_priority_request`; `proxy_upstream_url.take()` вместо clone в
  `finish_otel_span`) — минус 3 String-аллокации и 1 clone на hot path, без
  изменения поведения. CodeRabbit (Major, config-snapshot drift) — отклонён с
  обоснованием (предсуществующее, не регрессия #91; занесён в бэклог как 1b).
  Все 5 review-тредов отвечены + resolved. SonarCloud QG PASSED (0 new issues),
  27/27 CI зелёные. **Squash-merge `267ba51` в main, 2026-06-13.** Ветка
  (remote+local) удалена. ⚠️ `gh pr merge --delete-branch` упал на локальном
  шаге checkout (main занят основным worktree) — мердж на GitHub при этом
  прошёл; remote-ветку удалил вручную через `gh api -X DELETE`.
- Вне scope: 3 старых S3776 (`router.rs::route_request` CC 79,
  `config/validate.rs` CC 21, `cli/init.rs` CC 16) — зафиксировано в
  пункте 1a бэклога. **Закрыты 2026-08-17**, см. соответствующую запись
  ниже — `route_request` в этой заметке был мислейблом, реальная функция —
  `resolve_proxy`/`resolve_proxy_routes`.

### Реализовано в сессии 2026-06-13 (пункт 1b — единый config-снапшот в post-route хелперах)

- **[PR #92](https://github.com/lopatnov/conduit/pull/92)
  `refactor(proxy): share one config snapshot across post-route helpers`**
  (ветка `refactor/config-snapshot-helpers`, squash-merge `5cc1c59`) — закрывает
  config-snapshot drift (CodeRabbit Major на #91, бэклог 1b).
- `do_request_filter` теперь берёт **один** `config.load_full()` (owned `Arc`)
  и использует его и для `route_request`, и для резолва `site` (один раз) →
  прокидывает `Option<&SiteConfig>` в `store_ip_conn_slot` /
  `enforce_route_rate_limit` / `shed_low_priority_request`. Routing + 3 хелпера
  теперь на одном снапшоте; routing-vs-helper TOCTOU закрыт. **4 `load()` → 1
  `load_full()`.** `SiteConfig` добавлен в `use crate::config::schema::{…}`.
- `load_full()` (owned Arc, рефкаунт-инкремент без аллокации) безопасно держать
  через `.await` guard-чейна — именно поэтому хелперы раньше перезагружали
  конфиг (guard от `load()` нельзя долго держать). Заодно убран held-guard-across
  -await smell.
- Поведение в steady state не изменилось; разница только при hot-reload —
  хелперы консистентны с routing-решением вместо гонки с ним.
- `/build` GREEN: fmt (был 1 fix — две строки превысили лимит после нового
  параметра, поправлено `cargo fmt`), clippy `-D warnings` (default + full),
  тесты 1341 (default) / 1534 (full). SonarCloud QG PASSED (0 new issues, без
  новых S3776). 27/27 CI зелёные; CodeRabbit "no actionable comments", Gemini —
  без замечаний. Ветка (remote+local) удалена (тот же worktree-gotcha с
  `--delete-branch`, см. [[worktree-merge-gotcha]]).

### Реализовано в сессии 2026-06-13 (тесты stale-if-error #48 + бенчмарк-тулинг + worktree guards)

- **[PR #93](https://github.com/lopatnov/conduit/pull/93)
  `test(cache): cover stale-if-error on retry exhaustion + connection error`**
  (squash-merge `7e2f811`, [issue #48](https://github.com/lopatnov/conduit/issues/48)
  CLOSED, смерджен пользователем) — детали в чекбоксе «stale-if-error при исчерпании
  retry» выше. 3 интеграционных теста в `tests/cache.rs`; gemini нашёл реальный баг в
  тесте (`{ path_prefix: route }` → литерал-ключ вместо значения, `(path_prefix)` фикс),
  no-retry ассерты ужаты до `== 2`, retry оставлен `>= 2` (счётчик retry —
  implementation detail, боты разошлись 3 vs 4). Все треды resolved.
- **Бенчмарк-тулинг (дешёвый, для будущих сессий)**: создан агент
  `.claude/agents/benchmark-runner.md` (haiku) + команда `.claude/commands/benchmark.md`
  (`/benchmark [default|standard|full] [size|throughput|both]`) — делегирует агенту,
  правит только `docs/benchmarks.md`. Реестр агентов/команд грузится на старте → доступны
  со следующей сессии.
- **Re-benchmark `--features standard` (бэклог) — НЕ завершён в этом окружении**: `cross`
  0.2.5 не ставит linux-тулчейн на Windows (`toolchain ... may not be able to run`), wrk
  не установлен и требует Linux-рантайма. Точную musl-цифру взять из артефакта
  `release.yml` (`conduit-x86_64-unknown-linux-musl`), throughput — на Linux. Цифры в
  `benchmarks.md` НЕ выдуманы, оценка `~17.8 MB ¹` оставлена с пометкой.
- **Worktree-guards** (после того как .claude-тулинг дважды оказывался в эфемерной
  worktree-копии): правило «Worktree persistence» в `.claude/rules/index.md` + `Stop`-хук
  в user-настройках (`<user-home>\.claude\settings.json`, `shell: powershell`),
  аддитивно зеркалит worktree `.claude/{agents,commands,skills,rules}` → main checkout
  (robocopy /XO, без удалений). См. [[worktree-dotclaude-split]].

### Релиз v1.1.2 (2026-06-13)

- [PR #94](https://github.com/lopatnov/conduit/pull/94) `chore: bump version to 1.1.2`
  (squash-merge `a31b00925`) — version lockstep (`Cargo.toml`/`Cargo.lock`/
  `npm/package.json`/`docs/{benchmarks,cli,deployment}.md`).
- Тег `v1.1.2` → [`release.yml` run 27466039300](https://github.com/lopatnov/conduit/actions/runs/27466039300)
  — все 21 джоба зелёные (кросс-компиляция ×10 платформ, Docker `:1.1.2`/`:1.1.2-full` +
  Trivy, crates.io, npm, GitHub Release).
- Артефакты проверены: [GitHub Release v1.1.2](https://github.com/lopatnov/conduit/releases/tag/v1.1.2)
  (бинарники + `SHA256SUMS.txt`), оба Docker-манифеста резолвятся, `lopatnov-conduit = "1.1.2"`
  на crates.io, `@lopatnov/conduit@1.1.2` на npm.
- Ветка `chore/bump-version-1.1.2` удалена (локально + remote) после мерджа.

### Процессные правки (2026-06-13, не в git — `.claude/`)

- **Лимит длины файла**: `rules/conventions.md` «Code quality» — мягкий лимит 400 строк,
  жёсткий 1000. При превышении — вызывать новый агент `architect` (opus, advisory-only,
  `.claude/agents/architect.md`) за планом разбиения.
- **Новый агент `architect`** (opus, только `Read/Glob/Grep/Bash`, не редактирует файлы) —
  для планов разбиения файлов и декомпозиции крупных архитектурных задач. Добавлен в
  `rules/workflow.md` (триггер-таблица) и `rules/index.md` (реестр субагентов).
- При разборе пункта 2 (V2 feature-driven архитектура) выявлено: `request_phase.rs`
  (3157 строк) и `router.rs` (2642, CC 79) уже втрое превышают новый жёсткий лимит —
  естественные кандидаты на разбиение через `architect` как часть V2-дизайна.

### Реализовано в сессии 2026-08-01 (Conduit 2.0 migration — Phase 0.1: workspace scaffolding)

- **[PR #150](https://github.com/lopatnov/conduit/pull/150)
  `feat(workspace): add [workspace] scaffolding to root Cargo.toml`**
  (ветка `feat/workspace-scaffolding-115` → `claude/cargo-workspace-features-23qxfr`,
  squash-merge `c746cd9`, [issue #115](https://github.com/lopatnov/conduit/issues/115)
  CLOSED) — первая реальная имплементационная задача эпика #114 (первые 5 сессий
  после создания эпика ушли на PR #112/#149 tooling и Dependabot-триаж). Root
  `Cargo.toml` получил `[workspace]` (`members = ["crates/*"]`, `resolver = "2"`) и
  `[workspace.package]` (version/edition/license/repository); `[package]` теперь
  наследует эти поля через `.workspace = true` вместо дублирования — проверено
  через `cargo metadata` (`workspace_members` резолвится корректно), а не просто
  задекларировано. `crates/README.md` — плейсхолдер, сама директория пустая до
  Phase 2 (#126, `conduit-core`). Код не двигался, `cargo build`/`check` output
  не изменился. Версия workspace поднята до `2.1.0` (per-PR minor bump на этой
  ветке, `main`/1.x не затронуты).
  Перед началом сама ветка `claude/cargo-workspace-features-23qxfr` смерджена с
  `main` (была позади на #111 security fix + `.claude/` tooling + 10
  Dependabot-бампов) — во избежание накопления конфликтов.
  `feature-matrix-runner`: `cargo hack check --each-feature --no-dev-deps` —
  20/20 комбинаций зелёные, `resolver = "2"` не ломает feature isolation.
  Два finding'а Qodo (version lockstep vs 1.x release artifacts; workspace glob
  matches README) — оба ложные срабатывания, отклонены с обоснованием
  (проверено эмпирически через `cargo metadata` + зелёный CI), Qodo подтвердил
  (strikethrough). CodeRabbit не ревьюит PR в non-default branch — авто-ревью
  отключено оргой для веток кроме `main`.
  Следующий шаг эпика: #116 (hoist third-party deps в `[workspace.dependencies]`).

### Реализовано в сессии 2026-08-03 (Conduit 2.0 migration — Phase 0.2: hoist deps + security-gate hardening)

- **[PR #153](https://github.com/lopatnov/conduit/pull/153)
  `refactor(workspace): hoist every third-party dep into [workspace.dependencies] (#116)`**
  (ветка `feat/workspace-hoist-deps-116` → `claude/cargo-workspace-features-23qxfr`,
  squash-merge `1124d1d`, [issue #116](https://github.com/lopatnov/conduit/issues/116)
  CLOSED) — каждая third-party зависимость перенесена в новую `[workspace.dependencies]`
  таблицу; `[dependencies]`/`[dev-dependencies]` корневого пакета теперь ссылаются через
  `name.workspace = true` (`optional = true` остаётся на уровне пакета — внутри
  `[workspace.dependencies]` он не валиден). Чистый рефактор объявлений, `src/` не тронут,
  дрейфа резолюции зависимостей нет за пределами версии пакета `2.1.0 → 2.2.0`.
  `feature-matrix-runner`: 20/20 `cargo hack --each-feature --no-dev-deps` зелёные.
  **Инцидент по пути**: первый `Write` черновик `Cargo.toml` случайно потерял всю
  таблицу `[dev-dependencies]` (молча, причина не установлена) — сломал CI на
  ubuntu/macos/windows/ACME/All-features/Standard-bundle (`cannot find blocking in
  reqwest`/`cannot find crate tempfile`). Пойман только через реальный `cargo test`
  в CI (не через локальный `cargo check`/`clippy`, которые не компилируют test-таргеты).
  Первый фикс был **молча откачен** гонкой с параллельно запущенным
  `feature-matrix-runner` (агент с Bash-доступом, свои `git checkout` в той же
  директории) — переприменён и закоммичен немедленно; задокументировано как новое
  правило Step 5 (`isolation: "worktree"` для фоновых верификационных агентов, если
  conductor планирует продолжать редактировать файлы параллельно), коммит `58da726`.
- **`security-engineer` unconditional-gate — первый реальный HOLD**: первый проход
  вернул HOLD не по содержимому рефактора (оно было чистым на всех проверках), а
  из-за устаревшей относительно `claude/cargo-workspace-features-23qxfr` ветки PR —
  агент через double-dot diff (`target..head`) увидел, что PR "трогает"
  `.claude/commands/feature-workspace-cycle.md`, и предупредил, что squash-merge может
  откатить 2 недавних коммита в этом файле. Conductor независимо проверил через
  реальный `git merge --squash` в изолированном clone — тот тронул только
  `Cargo.toml`/`Cargo.lock` (squash использует merge-base semantics, не raw double-dot
  diff) — но вместо спора о диффах просто смёржил актуальный tip target-ветки в PR
  (коммит `5830f37`), закрыв вопрос однозначно. Второй foreground-проход
  `security-engineer` против нового head дал **PASS**; verdict запощен как обязательный
  sign-off комментарий на PR перед мерджем (per `.claude/rules/workflow.md`).
- **Хардening процесса по итогам** (коммит `333385c`, вызван реальными findings
  CodeRabbit на трекинг-PR #152, а не самоинициативой): `.claude/rules/workflow.md` и
  `.claude/commands/feature-workspace-cycle.md` теперь явно требуют, что PASS
  `security-engineer` валиден только для той SHA, что он реально ревьюил — любой
  новый коммит после PASS (фикс, ребейз, merge-forward) инвалидирует его и требует
  повторного прохода перед мерджем; и что результат worktree-изолированного
  background-валидатора покрывает только то, что было закоммичено в этот worktree
  на момент spawn — не более поздние правки conductor'а в общем чекауте. Третий
  finding CodeRabbit (историческая версия `2.1.0` в записи Phase 0.1 выше по этому
  же файлу) — ложное срабатывание, отклонён с обоснованием (дневниковая запись, не
  текущая документация); CodeRabbit сам отозвал finding и записал learning.
  Все 3 треда на #152 отвечены и resolved.

### Реализовано в сессии 2026-08-17 (закрытие "3 старых S3776" + прочее на `main`)

- **PR #193** (мигрейшн-ветка) — `crates/conduit-core` добавлен как первый Layer-0
  workspace-член (`FilterOutcome`/`FilterContext`/`RequestFilter`,
  `ResponseFilterOutcome`/`ResponseCtx`/`ResponseFilter`, `is_path_skipped`,
  `LocalHandlerImpl`, `write_denied`/`write_redirect`/`write_response`,
  `AcceptEncoding`, `content_type`, `LogWriter`), `src/` держит тонкие facade
  ре-экспорты. По ходу найден и исправлен реальный баг в
  `scripts/check-layer-boundaries.sh` (#125/#186) — неверные имена крейтов в
  `ALLOWED_CRATES` и небезопасная эвристика распознавания комментариев
  (исключение строк с `*` ловило валидный `*guard = ...` код, а не только
  block-comment continuation) — поймано `security-engineer`'s ревью.
  CI/coverage довинчены под новый workspace-член (`ci.yml --workspace`,
  `sonar.yml --workspace`, `sonar-project.properties`).
- **PR #204/#206/#208** — все 3 давних CRITICAL rust:S3776, отложенных
  PR #91 (2026-06-13), закрыты: `config/validate.rs::validate_site` CC 21→0,
  `cli/init.rs::run_init` CC 16→2, `proxy/router.rs::resolve_proxy` CC 79→~4.
  `architect` (opus) дал план разбиения для всех трёх. Поправка, найденная
  при разборе: CLAUDE.md 2026-06-13 назвал CC-79 функцию
  `router.rs::route_request` — та функция плоский `match`, CC ~7; реальное
  тело было безымянным match-arm внутри `resolve_proxy`, теперь названным
  `resolve_proxy_routes`. Перед рефактором `resolve_proxy` отдельным PR #207
  добавлены 4 unit-теста на sticky/HMAC-роутинг и malformed-backup-URL —
  путей без покрытия выше HMAC-примитивов не было вообще; один тест поймал
  реальный неверный assumption (`"not-a-url"` парсится нормально через
  `url_to_host_port`, понадобился `"http://"` для настоящего failure path).
  `security-engineer` дал PR #208 повышенное внимание (независимый построчный
  разбор диффа, не просто доверие тестам) — само по себе поймал слабый
  assert в новом sticky-тесте (CodeRabbit) на #207, исправлено до мерджа.
- **12 Dependabot PR смерджены** (#194-203, включая `jsonwebtoken` 10→11 и
  `redis` 1.3→1.5, оба MAJOR/значимые minor на security-relevant крейтах —
  `security-engineer` проверил changelog'и, PASS на оба; `base64` 0.22→0.23
  смерджен вместе с jsonwebtoken как transitive dep).
- **Issue #181 закрыт** (PR #205) — 7 файлов в `sonar.coverage.exclusions`
  исключали реально протестированный код (85 `#[test]` суммарно, включая
  security-sensitive `tls.rs`/`cache_disk.rs`/`cache_redis.rs`). SonarCloud
  dashboard/API недоступны из этого окружения (тот же блокер, что и у автора
  issue) — проверено напрямую через `cargo llvm-cov --lib --features full`
  локально, реальное покрытие 57–85% на всех 7 файлах.
  Итого за сессию: 8 PR смерджено в `main` (#193 на мигрейшн-ветку,
  #204-208 + #199/#200/#203 отдельно среди 12 dependabot).

### Реализовано в сессии 2026-08-17 (часть 2 — issue #155 и #156, passive-health + circuit breaker)

- **#155 закрыт** ([PR #214](https://github.com/lopatnov/conduit/pull/214), squash-merge
  `1264312`) — `RequestCtx.proxy_upstream_url` теперь заполняется безусловно для любой
  стратегии во всех трёх routing-путях (`resolve_proxy_routes`, `resolve_grouped`,
  `routes.rs::full_cfg_to_result`), так что Peak EWMA/Outlier Detection/per-peer stats
  реально работают вне `LeastConn`. Новое поле `RequestCtx.upstream_conn_slot: bool`
  (зеркалируется на `router.rs::RouteResolution`) отдельно трекает, держит ли запрос
  реальный `conn_count`-слот — иначе два маршрута на общий upstream (один `least-conn`,
  другой нет) портили бы общий счётчик фантомными декрементами. 4 unit + 3 integration
  теста, включая `attribution_only_route_does_not_corrupt_shared_conn_count`, которая
  специально доказывает отсутствие этого фантомного декремента.
- **#156 закрыт** (ветка `fix/circuit-breaker-capacity-enforcement-156`) — `maxConnectionsPerUpstream`
  теперь реально enforced для всех 8 стратегий (issue называл 6, на деле было 7 —
  `LeastResponseTime` тоже пропущен — плюс sticky-роуты, которые принудительно используют
  `ConsistentHash`), и во всех трёх форматов конфига (`proxy: {}`, `routes[]`, `groups` —
  `routes[]`/`groups` раньше вообще не имели circuit-breaker кода). Новый модуль
  `src/proxy/capacity.rs`: `Capacity` enum (`Unlimited`/`Under`/`Exhausted`) + единая точка
  диспетчеризации `pick_bounded`/`BoundedPick` — ни `router.rs`, ни `routes.rs` не матчатся
  по вариантам `LoadBalanceStrategy` для целей capacity, весь match — только внутри
  `capacity.rs` (сохраняет гарантию decision #22 "router.rs не трогать при добавлении
  стратегии", а не нарушает её, как предполагал один из промежуточных планов).
  Для `IpHash`/`ConsistentHash` — forward-probing по несужаемому hash-кольцу
  (`hash_pick_bounded`), а не наивная фильтрация кандидатов: `pick_by_hash` — наивный
  modulo, не настоящий hash ring с virtual nodes, так что сужение домена на один элемент
  ремапнуло бы почти всех клиентов, а не только тех, чей peer выбыл — особенно опасно
  здесь, поскольку conn_count меняется на каждый запрос (в отличие от health, который
  меняется раз в ~10s). Cap — мягкий (soft limit, TOCTOU overshoot допустим, тот же
  trade-off что и `retry.budgetPercent`). Мёртвый код `conn_inc_if_below`/
  `pick_least_conn_with_max` (+ 4 их теста) удалён — после фикса живой механизм ровно
  один. ~13 новых тестов (4 unit в `router.rs`, 3 unit в `routes.rs`, ~19 unit в новом
  `capacity.rs`, 2 integration в `tests/upstream_health.rs`).
  Попутно подтверждено и задокументировано: `cache.earlyRefreshSecs` (закрытый
  feature-issue #31) был гейтирован тем же условием `proxy_upstream_url` и уже
  автоматически починен побочным эффектом #214 — отдельного кода не потребовалось.
  Найдены и заведены 3 отдельных issue, не в этот PR: **#216** (retry-попытки обходят
  cap и недоучитываются в `conn_count`), **#217** (`routes[]` retry-список не
  health/capacity-фильтрован), **#218** (`RequestCtx.failed_upstream_attempts` —
  write-only состояние, doc-comment утверждал обратное — поправлен на месте).
- **Процессная находка**: план для #156 прогонялся через `architect` дважды — первый
  прогон (до мерджа #214, доступен только по моему пересказу в чате, не raw-отчёт) и
  второй (после #214, свежий против актуального кода) разошлись в нескольких местах
  (где жить диспетчеру стратегий, статус `cache.earlyRefreshSecs`, WeightedRoundRobin,
  один PR vs отдельный PR C для `routes[]`/`groups`). Пользователь заметил расхождение и
  остановил реализацию; потребовался третий, явно реконсиляционный прогон `architect`
  с обоими планами целиком в промпте, который разрешил все 4 спорных пункта с
  аргументацией и явно указал, где какой план был прав/неправ. Урок: не полагаться на
  собственный пересказ прошлого agent-вызова как на источник истины, когда есть
  расхождение с новым прогоном — давать обоим полный текст и просить явную реконсиляцию.

### Реализовано в сессии 2026-08-21 (Phase 2 facade re-audit + RequestCtx decision #30)

- **[PR #230](https://github.com/lopatnov/conduit/pull/230)
  `chore(workspace): Phase 2 facade audit follow-up + crate-extraction recipe`**
  (ветка `chore/phase2-cleanup-recipe-114` → `claude/cargo-workspace-features-23qxfr`,
  squash-merge `0f6b921`) — по итогам независимого `architect`-аудита Phase 2
  (Layer-0 crate extraction) facade-checkpoint (issue #128, закрыт ранее): фасад
  реально держит форму, но найдены 2 небольших пробела + 1 недодокументированный
  паттерн. Исправлено: `conduit_core::filter::path::path_matches` был случайно
  расширен с `pub(crate)` (до миграции) до `pub` при извлечении `conduit-core`
  (#126) без единого re-export — вернули `pub(crate)` (единственный вызывающий —
  `is_path_skipped`, тот же модуль); задокументирована коллизия имён с
  `src/proxy/cache.rs`'s собственным `path_matches` (иная семантика — префиксное
  совпадение без `/**`); `Provider<C>` задокументирован в `crates/README.md` как
  намеренный слом API 2.0; новый раздел "Cargo Workspace Crate Extraction Recipe"
  в `CONTRIBUTING.md` (4 правила извлечения крейтов — раньше не существовал нигде,
  хотя агент `crate-extractor` в своём же описании ссылался на него).
- **`CLAUDE.md` decision #30** — `RequestCtx` per-request state: поля остаются в
  корневом крейте (status quo), НЕ через type-erased extension slot и НЕ через
  отдельный trait в `conduit-core`. Каждое feature-specific поле — через
  `#[cfg(feature = "x")]`, по образцу уже существующих `otel_span`/
  `early_refresh_upstream_url`. Решение пользователя, снимает блокировку с #129
  (`conduit-otlp`) и последующих #131/#133/#135/#141/#142.
- **[PR #231](https://github.com/lopatnov/conduit/pull/231)
  `fix(tests): unblock CI after Rust 1.98.0 toolchain-lint upgrade`** (ветка
  `fix/clippy-chunks-exact-lint-main` → `main`, squash-merge `9d3d1e6`) — CI-раннеры
  подхватили Rust 1.98.0 с двумя новыми clippy-линтами под `-D warnings`, ломающими
  несвязанный код: `clippy::chunks_exact_to_as_chunks` в SHA-1 test helper'е
  `tests/websocket.rs` (`chunks_exact(N)` → `as_chunks::<N>().0`, поведенчески
  идентично — проверено на `sha1_rfc6455_test_vector`) и `clippy::result_large_err`
  в `src/upload/server.rs` (`#[allow]` на `process_upload_field`/
  `save_upload_file`, по образцу уже существующего на `check_mime_type` в том же
  файле). Идентичные фиксы применены на обеих ветках (`main` через #231, миграционная
  ветка — прямо в #230, т.к. содержала тот же непочиненный код).
  Обе PR прошли обязательный `security-engineer` gate (оба PASS, вердикты записаны
  комментариями на PR). CodeRabbit на #230 упёрся в собственный review-rate-limit
  ("next review available in 58 minutes") — пользователь явно разрешил не ждать;
  Gitar одобрил оба PR ("No issues found").
- **Отдельно найден и исправлен процессный gap**: "Dependabot & branch hygiene
  reflex check" простаивал >24ч (последняя запись 2026-08-18) — прогнан вручную
  (0 открытых Dependabot PR, orphan-веток нет за пределами собственной работы этой
  сессии), залогирован отдельной строкой в таблице выше.
- Миграционная ветка синхронизирована с `main` после мерджа #231 (merge, без
  конфликтов — идентичные фиксы в обоих файлах), `cargo build --workspace` +
  `cargo test --workspace` зелёные после синка.
- **Запланировано пользователем**: 17 разовых (`run_once_at`) вызовов
  `/feature-workspace-cycle` каждые ~5 часов с 2026-08-21 20:00 UTC по
  2026-08-25 19:00 UTC (self-bind в эту же сессию, как и штатный ежедневный
  Routine) — 3 слота из исходных 20 пропущены намеренно из-за коллизии по времени
  с уже существующими Routine (штатный ежедневный `feature-workspace-cycle` в
  01:00 UTC, `Mise /evolve` в 06:00 UTC, `doc2html` QA в 11:00 UTC 2026-08-22),
  чтобы не создавать одновременные срабатывания на один и тот же слот сессии.

### Реализовано в сессии 2026-08-23 (#132 conduit-faults + #164 JWKS test coverage)

- **[PR #255](https://github.com/lopatnov/conduit/pull/255)
  `feat(workspace): extract conduit-faults crate (#132)`** (ветка
  `feat/extract-conduit-faults-132` → `claude/cargo-workspace-features-23qxfr`,
  squash `624d24e`) — `FaultInjectionConfig`/`FaultAbort`/`FaultDelay` и
  `FaultInjectionGuard` в `crates/conduit-faults`, за существующей фичей
  `fault-injection`. Конфиг-структуры остаются всегда скомпилированными (чтобы
  `feature_warnings()` продолжал предупреждать при конфиге без фичи), гейтится
  только сам guard. Facade re-export на прежних местах — вызывающий код не менялся.
  `crate-extractor` + независимая проверка кондактором напрямую (диск кончился у
  четырёх параллельных verification worktree — освобождено удалением уже
  завершённых worktree, затем полный `cargo build/clippy/test --features full`
  вручную) + `feature-matrix-runner` + `footprint-auditor` (бинарник практически
  не изменился, -384 байта). `security-engineer` PASS.
- **[PR #256](https://github.com/lopatnov/conduit/pull/256)
  `test(jwt): cover the JWKS/RS256/ES256 code path (#164)`** (ветка
  `fix/jwt-jwks-test-coverage-164` → `main`, squash `f746ce8`, issue #164 CLOSED)
  — прерог для #133 по coupling-таблице #114 (перенос кода без тестов на половину
  путей сделал бы "не сломал ли перенос?" непроверяемым). 11 unit-тестов
  (`fetch_jwks` против raw-TCP мок JWKS-эндпоинта + `validate_with_jwks`
  full round-trip, включая тест на RS256→HS256 algorithm-confusion атаку — подписание
  токена HS256 с использованием опубликованного в JWKS RSA `n` как HMAC-секрета,
  отклоняется) + 3 integration-теста (реальные RS256/ES256 токены через полный
  guard chain). По ходу — два реальных review-finding'а, оба исправлены до мерджа:
  - **SonarCloud "E Security Rating"**: первая версия PR встраивала статические
    RSA/EC private-key PEM-константы как тестовые фикстуры — триггернуло правило
    hardcoded-credentials, хотя ключи одноразовые и нигде больше не используются.
    Исправлено — генерация RSA-2048/P-256 ключей в рантайме теста (`rsa`/`p256`
    как dev-dependencies, версии уже разрешены транзитивно через `jsonwebtoken`'s
    `rust_crypto` backend, в граф зависимостей ничего нового не добавилось), по
    аналогии с `rcgen` "no checked-in cert fixtures". SonarCloud Quality Gate
    после фикса — PASSED (0 new issues, 0 hotspots).
  - **Gitar: `jwt.rs` превысил жёсткий лимит 1000 строк** — вызвано ростом файла
    из-за инлайн JWKS-тестов. Разбито через план `architect`: продакшн-код
    (365 строк) остался в `jwt.rs` без изменений, старые HS256-тесты перенесены
    в `src/filter/jwt/tests.rs`, новый JWKS-материал — в
    `src/filter/jwt/tests/jwks.rs`. Видимость не расширялась
    (`pub(crate) fn extract_claims_unchecked` осталась как есть).
  - Попутно (по явному запросу пользователя) поправлен сам лимит в
    `.claude/rules/conventions.md`: правило 400/1000 строк всегда имелось в виду
    только для продакшн-кода, не для тестов — большой инлайн `mod tests` сам по
    себе не повод для разбиения.
  - Gitar отдельно поймал, что integration-тесты в `tests/auth.rs` генерировали
    RSA-2048 ключ заново в каждом тесте вместо кэша через `OnceLock` (как unit-тесты)
    — исправлено, время прогона файла упало с ~7.8с до ~3.7с.
  - `security-engineer` PASS (независимо перепроверил алгоритм confusion-теста
    против реальной логики `validate_with_jwks`, а не только текста теста) +
    `lawyer`-проверка двух новых dev-only транзитивных крейтов (`pem` MIT,
    `simple_asn1` ISC) — без блокирующих находок.
- Миграционная ветка синхронизирована с `main` дважды за сессию (после #255 — без
  конфликтов; после #256 — один реальный конфликт в `src/filter/jwt.rs`: миграционная
  ветка уже независимо перенесла `expand_jwt_templates` в `crate::util::jwt_template`
  (issue #123), так что её копия тестов `jwt.rs` уже отличалась от версии на `main`
  до PR #256 — разрешено взятием уже корректного содержимого тестов миграционной
  ветки + добавлением нового `jwks`-подмодуля). `Cargo.lock`-конфликт разрешён не
  через full re-lock (`cargo generate-lockfile` неожиданно предлагал bump версий
  несвязанных пакетов), а через инкрементальный `cargo check` поверх "нашей" копии
  лока — добавился ровно один новый пакет (`simple_asn1`), без постороннего churn'а.
  `cargo build/clippy/test --workspace --features full` (1156 lib-тестов + 54
  integration в `auth.rs`) зелёные после синка, запушено.
