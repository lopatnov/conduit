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

---

## Pipeline обработки запроса

```
request_filter()
  ├─ inflight++, active_connections.inc()
  ├─ FilterChain: XRequestIdGuard → IpGuard → CorsPreflight → HealthBypass → LimitsGuard
  │              → RateLimitGuard → BasicAuthGuard → ApiKeyGuard → JwtGuard (6c)
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
  **Caveat found 2026-08-03 (integrity audit, Step 1c)**: only actually tracks/ejects
  when strategy is `LeastConn` or `healthCheck.maxConnectionsPerUpstream` is also set
  (gated by `RequestCtx.proxy_upstream_url`) — no-ops for the other 6 strategies
  otherwise, including the `RoundRobin` default. Tracked as a GitHub issue, not yet fixed.
- [x] **Circuit Breaker** — `healthCheck.maxConnectionsPerUpstream: u64`. When ALL healthy upstreams ≥ limit → `LocalHandler::Overloaded` → 503 — this part holds for every strategy. 2 integration tests.
  **Correction 2026-08-03 (integrity audit, Step 1c)**: "Works for all LB strategies"
  was inaccurate — the *all-maxed → 503* case holds everywhere, but skipping a single
  at-limit upstream in favor of under-capacity peers is only guaranteed for `LeastConn`
  today (it always picks true min-load). `conn_inc_if_below()`/`pick_least_conn_with_max()`
  are dead code (never called outside their own unit tests) — the real live mechanism is
  inline in `router.rs` (`under_limit` computed then discarded, `:374-397`). Tracked as a
  GitHub issue, not yet fixed.
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

- [x] **JWT validation with JWKS URL** — `jwtAuth: { secret? | jwksUrl?, audience?, issuer?, skipPaths? }`. HS256 + RS256/ES256 (JWKS). `JwtGuard` в filter chain (6c, после apiKey). `src/filter/jwt.rs`. JWKS кэш per-URL с TTL. `jsonwebtoken = "9"`. 8 unit + 6 integration тестов.
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
| 2026-08-03 | `src/proxy/health.rs` (unchanged since v1.1.0/PR #67 — oldest actively-used file in the codebase) | 4 real behavioral gaps + 4 low-risk doc/comment issues | First-ever Step 1c firing (cadence gate finally satisfied: Step 0 idle, Step 1 found nothing to triage). Root finding: passive health tracking (Outlier Detection, Peak EWMA, per-peer response stats) and true circuit-breaker enforcement (skipping a single at-limit upstream) only actually work for `LoadBalanceStrategy::LeastConn` or when `maxConnectionsPerUpstream` happens to also be set — for the `RoundRobin` default and 4 other strategies without a connection cap, several `[x]`-marked "done" backlog items silently no-op. Also found `slowStartSecs` fully unwired (zero effect) and `prewarmConnections` warming a throwaway client instead of Pingora's real pool. Doc/comment-only fixes (scrambled doc-comment un-scramble, honest known-limitation notes, 2 stale CLAUDE.md backlog claims corrected) shipped directly via [PR #154](https://github.com/lopatnov/conduit/pull/154) per the low-risk/unambiguous routing rule. The 4 behavioral gaps needing design judgment filed as [#155](https://github.com/lopatnov/conduit/issues/155) (passive tracking gate), [#156](https://github.com/lopatnov/conduit/issues/156) (circuit-breaker enforcement gate, cross-references #155), [#157](https://github.com/lopatnov/conduit/issues/157) (`slowStartSecs` dead code), [#158](https://github.com/lopatnov/conduit/issues/158) (`prewarmConnections` doesn't warm the real pool) — ordinary repo backlog, not #114 sub-issues. Note: the agent originally delegated to file these issues (`scrum-master`) turned out not to have GitHub MCP tools in its grant, fell back to raw-credential API probing (blocked by egress policy, no data exposed) — flagged as a security-relevant subagent-behavior incident and routed to `security-engineer` for review rather than self-cleared; issues were filed directly by the conductor's own properly-scoped tools instead. |

---

## Dependabot & branch hygiene log

> Append-only. See `.claude/rules/index.md` "Dependabot & branch hygiene reflex check" —
> any session that touches this repo's GitHub state runs this cheap sweep if the newest
> row here is older than ~24h, then logs a row (even "nothing new"). Newest on top.

| Date/time (UTC) | New Dependabot PRs found/acted on | Orphan branches flagged | Notes |
|---|---|---|---|
| 2026-08-04 ~02:15 (daily cycle firing, Step 1c follow-through) | 0 open (clean) | 0 | First-ever Step 1c firing (see "Integrity audit log" below) produced PR #154 (health.rs doc fixes) and PR #159 (subagent tool-gap hardening, from a security-engineer-reviewed incident) — both merged into `main` with the unconditional security gate. Migration branch was 2 commits behind afterward; synced clean (`git merge origin/main`, no conflicts — the two branches had independently edited overlapping `.claude/` files but in non-overlapping regions), `cargo fmt --check` + `cargo clippy --lib -- -D warnings` green, pushed as `f12fb00`. The `security/dependabot/3` alert noted below is still unresolved and still unreachable with this session's tools. |
| 2026-08-03 ~02:00 (daily cycle firing) | 0 open (clean, only open PR is #152 the tracking PR) | 0 (branch count unchanged at 22 — `feat/workspace-hoist-deps-116` was created and auto-deleted on squash-merge within this same firing, netting to the same count) | `git push` on the migration branch has repeatedly surfaced a GitHub-native notice: "1 vulnerability (1 high)" on `main` at `github.com/lopatnov/conduit/security/dependabot/3`. Could not inspect it — no MCP tool in this session lists/reads Dependabot security alerts (only Dependabot *PRs*, of which there are none open, meaning no auto-PR exists for this alert), and the alert page itself needs authenticated access WebFetch can't provide. **Flagged to the user, unresolved** — needs a look from the GitHub UI or a session with alert-reading access. |
| 2026-08-02 ~04:00 (daily cycle firing) | 0 open (clean) | 0 (branch count dropped 25→22 since last check — user cleanup via the provided script + GitHub's own Dependabot branch auto-cleanup; `fix/pr112-review` orphan also gone) | Migration branch was 2 commits behind `main` (#101 kube fix, #151 all-actions bump) — the new "keep migration branch in sync" bullet caught this on its first real firing. Merged clean (`git merge origin/main`, no conflicts, `cargo check --features full` green), pushed as `844a174`. |
| 2026-08-01 ~10:00 | #151 (all-actions group, 11 updates) — merged; #101 (kube 3→4.0.0) — root-caused a real k8s-openapi 0.28 version conflict, fixed, merged | 0 (all ~25 branches checked accounted for by a PR — either open, merged, or closed) | Prompted by the user noticing `feat/workspace-scaffolding-115` and `dependabot/cargo/kube-4.0.0` in the branch list. Root cause of the untriaged PR + leftover branches: repo has no "Automatically delete head branches" setting and the cycle went from hourly to daily, leaving a gap between firings that no other session filled. This log + rule exist to close that gap. |

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
- JWT: jsonwebtoken v9 имеет leeway 60s по умолчанию. Expired test должен просрочить > 60s
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
- **Circuit Breaker**: `healthCheck.maxConnectionsPerUpstream` → `LocalHandler::Overloaded` → 503 (all-maxed case, all strategies). Live per-upstream-skip mechanism is inline in `router.rs` (`under_limit` computed, `:374-397`) — reliable for `LeastConn` only. `conn_inc_if_below()`/`pick_least_conn_with_max()` in health.rs are dead code (not called from `router.rs`), despite the name suggesting they're the mechanism — corrected 2026-08-03 (integrity audit).
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
  пункте 1a бэклога.

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
