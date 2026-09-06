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
14. **Rate limiter** — `DashMap` v6 (`AppState.rate_limiter`, shared by site/route/consumer
    layers). **Канонический формат ключа с 2026-08-30** (`src/filter/rate_limit.rs`:
    `site_key`/`route_key`/`consumer_key`, fix для #303/#304): `\0`-разделённые, с тегом
    namespace — site-level: `"site\0{site_label}\0{client_key}"`; per-route:
    `"route\0{site_label}\0{route_key}\0{client_key}"`; per-consumer: `"consumer\0{username}"`
    (**намеренно** не скоуплен по сайту — квота consumer'а глобальна по всем сайтам, где он
    разрешён). `site_label` = тот же `"{host}:{port}"`/`"*"`, что уже используется в
    `conduit_rate_limit_rejected_total{site=…}`. `GET /rate-limits` (`admin/api.rs`) парсит
    все три формы и суммирует per-client бакеты в один total на (site, route) — раньше
    (до фикса) не парсил вообще ничего реального, всегда отдавал `{}` (issue #303). Redis-бэкенд
    (`crates/conduit-ratelimit/src/redis.rs`, за фичей `redis`, извлечён вместе с фиксом #317
    как #137 slice 2) — отдельный ключевой неймспейс: `"conduit:rl:{scope_label}:{window_secs}:
    {client_key}"` для реального Redis, `"{scope_label}:{client_key}:{limit}:{window_secs}"` для
    его in-process fallback-мапы. `src/filter/rate_limit_redis.rs` в корне — тонкий facade
    re-export. **С 2026-09-05 (issue #322)** `scope_label` (переименован из `site_label`,
    чисто ради ясности — сигнатура не менялась) — это либо site_label как раньше, либо
    `"route\0{site_label}\0{route_key}"` для per-route (`rate_limit::redis_route_scope`), либо
    фиксированный литерал `"consumer"` для per-consumer (username передаётся отдельным
    параметром `client_key`, не встраивается в scope). Redis работает на всех трёх уровнях, но
    **на процесс устанавливается только одно реальное соединение** — `connect_redis_rate_limiter_if_configured`
    сканирует site → route → consumer и подключается к первому найденному URL; если на разных
    уровнях настроены разные Redis URL, все уровни всё равно используют одно (первое найденное)
    соединение без предупреждения — задокументировано явно в `docs/configuration.md`, доведение
    до предупреждения/переподключения при hot-reload — issue #357, отдельное архитектурное
    решение, не сделано.
    **История находки (2026-08-30, Step 1c аудит `rate_limit.rs`)**: до этого фикса запись здесь
    ошибочно приписывала рейт-лимитеру формат `"{site}\0{route}"` — тот формат на самом деле
    принадлежит `UpstreamRegistry.override_key()` в `src/proxy/health.rs`
    (`conduit upstreams add/remove/weight --site`); отдельно было найдено, что site-level
    бакеты не были скоуплены по сайту вообще (issue #304) и что per-route бакеты имели тот же
    класс бага (найдено при реализации фикса, не было отдельным issue — два сайта с одинаковым
    `route_key` и общим клиентом делили бакет). Оба закрыты этим фиксом.
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
    **Пере-рассмотрено и подтверждено 2026-08-23** (пользователь явно попросил перепроверить, issue
    #114 "owner decisions" пункт 1, всё ещё числился в теле issue как открытый — устарел, реальное
    решение уже было в этом пункте с 2026-08-21). Проверено против реального кода:
    `crates/conduit-otlp/src/lib.rs` уже документирует именно этот паттерн ("Per-request span
    creation/finishing... deliberately stays in the root crate... see CLAUDE.md's architectural
    decision #30") — вариант C уже единственный факт на земле, не гипотеза. Вердикт по итогам
    повторного рассмотрения: подтвердить, не менять. Zero-cost на hot path перевешивает
    архитектурную "чистоту" отдельных крейтов для проекта, чья заявленная ценность — производительность;
    вариант A (TypeMap) добавляет hash-lookup+аллокацию на каждый запрос на каждую активную фичу; вариант B
    (typed slot в conduit-core) потенциально не хуже C по цене, но сам механизм не спроектирован — это
    неготовое решение, а не альтернатива на сегодня. Условие пересмотра (не абстрактное, конкретное):
    если экстракция #133 (`conduit-auth-jwt`, jwt_claims пишется в request_filter, читается в
    upstream_request_filter) или #135/#134 (consumers/forward-auth, похожий cross-phase паттерн) окажется
    реально болезненной на практике — не гипотетически, а по факту застревания/переделок в процессе PR —
    это и есть триггер вернуться к вопросу, не раньше.

31. **Feature-гейты для ipFilter/cors/securityHeaders/compression/static/fallback/hotReload/metrics/
    redirects (Conduit 2.0 migration, #114, фазы 3.8/4.1-4.3 — сабишью #136-#140)** — гибрид, не
    поголовное превращение всех девяти в `--features`. Извлечь в отдельные крейты для организации
    кода (один крейт = одна забота), но по-настоящему опциональными (с расширением `default`, чтобы
    сегодняшний zero-flag билд не потерял поведение) делать только то, что реально тяжёлое —
    `static`/`hotReload` (тянут `notify`, mime-детект) и, возможно, `compression`. `ipFilter`/`cors`/
    `securityHeaders`/`redirects`/`metrics` остаются always-on/не-опциональными — гейтинг ради гейтинга
    почти не даёт footprint-выгоды (это лёгкая логика без тяжёлых third-party крейтов), а стоимость
    "забыл флаг — тихо не работает" реальна. Конкретно проверено для `metrics`: `cargo tree -i
    prometheus@0.13.4` показывает, что `prometheus` уже безусловно тянется `pingora-core` независимо
    от наших фич — гейтинг нашего `/metrics`-хендлера не убирает эту зависимость из бинарника, экономия
    была бы только на нашем собственном коде хендлера. Решение пользователя 2026-08-23.

32. **Публикация member-крейтов на crates.io (Conduit 2.0 migration, #114)** — публиковать (технически
    почти вынужденно: `cargo publish` для самого бинарника `lopatnov-conduit` требует `version =`, не
    просто `path =`, у каждой зависимости — раз бинарник продолжает публиковаться на crates.io, все
    ~28 member-крейтов обязаны публиковаться в лок-степ), но **как internal-plumbing, не как полноценный
    публичный API** — без семвер-гарантий, `pub`-поверхность чистится по мере обнаружения утечек (как
    `conduit_core::filter::path::path_matches`, PR #230), не превентивно с библиотечной строгостью.
    Имя уже выбрано: `lopatnov-conduit-<name>` (см. `crates/README.md`). Переход на полноценный
    публичный API (вариант A — реальная документация, семвер-дисциплина на каждый крейт) осознанно
    отложен, не отклонён — пользователю идея нравится, но сейчас она существенно замедлит миграцию;
    трекается отдельным issue (см. беклог) для пересмотра после того, как механические фазы экстракции
    #114 приземлятся. Решение пользователя 2026-08-23.

---

## Pipeline обработки запроса

```
request_filter()
  ├─ inflight++, active_connections.inc()
  ├─ FilterChain: XRequestIdGuard → IpGuard → CorsPreflight → HealthBypass → LimitsGuard
  │              → RateLimitGuard → ConsumersGuard (6) → BasicAuthGuard → ApiKeyGuard → JwtGuard (6c)
  │              → ForwardAuthGuard (6d) → RedirectGuard → FaultInjectionGuard
  │              → MiddlewareGuard (Rhai + WASM in order)
  ├─ Per-route rate limit check (post-routing, key via rate_limit::route_key — see decision #14)
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

Health / ACME / HotReload — `HealthBypass` bypasses everything *after* it in the chain
(LimitsGuard onward: rate limit, auth, ForwardAuth, redirect, fault injection, middleware).
`XRequestIdGuard` and `IpGuard` run *before* `HealthBypass` and still apply — an IP-denied
client cannot reach `/__health__` either. (Corrected 2026-08-23, Step 1c audit of
`src/filter/ip_filter.rs` — this note previously said "bypass всех guard-фильтров",
i.e. bypasses *all* guards, which contradicts the pipeline order two paragraphs above.)

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
  **Уточнение 2026-09-06 (issue #157)**: чекбокс был отмечен преждевременно — сам механизм
  (`slow_start_fraction`) существовал, но нигде не вызывался за пределами собственных тестов;
  конфиг `slowStartSecs` был полным no-op. Реально подключено фиксом #157 — см. запись в конце
  файла. `src/proxy/slow_start.rs::Ramp` — probabilistic Bernoulli admission gate внутри
  `capacity::pick_bounded`, hash-стратегии/sticky — структурно исключены.
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
- [🚫 BLOCKED] **`tls.versions`/`tls.ciphers` enforcement** (issue #189, найдено
  `integrity-auditor` при Step 1c аудите `src/server/tls.rs`) — поля парсятся с самого первого
  коммита (`58ec267`), но никогда не были подключены: `make_tls_settings()` не принимает
  versions/ciphers параметр, `TlsPortMap` не имеет для них места, `detect_cold_changes()` их
  тоже не проверял — полный silent no-op с 2026-мая. **Причина:** подтверждено через vendored
  `pingora-core-0.8.1/src/listeners/tls/rustls/mod.rs:62-63` — `TlsSettings::build()` жёстко
  зашивает `ServerConfig::builder_with_protocol_versions(&[TLS12, TLS13])` без cipher-suite
  API, все поля `TlsSettings` приватные, единственный конструктор `intermediate()` берёт
  только cert/key path, `add_tls_with_settings()` принимает исключительно `TlsSettings` —
  никакого хука для кастомного `ServerConfig`/`Acceptor`. Ждём Pingora 0.9+
  (`ResolvesServerCert`-подобный API или публичный доступ к builder).
  **Фикс (2026-08-29, issue #189)**: раз честно wire-нуть нельзя — поля теперь **жёстко
  отклоняются на `validate()`** (`Severity::Error`, блокирует старт и `/reload`) вместо
  тихого игнорирования, чтобы оператор не решил, что TLS-версии/шифры реально ограничены.
  `examples/security-hardened.{yaml,json}` и `docs/configuration.md`/`docs/admin.md`
  поправлены (убрана ложная claim "requires cold restart" — теперь это hard validation error,
  не cold-restart-only поле).

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
  Хранится в `Arc<RwLock<Vec<String>>>` (`AppState.dynamic_deny`, raw CIDR strings, не
  `IpNet` — парсится на каждый чек через `matches_rule()`, тот же путь что и статический
  `ipFilter.deny`). `IpGuard.dynamic_deny` — тот же `Arc`, читает в `is_dynamic_denied()`.
  Паттерн: envoy Network RBAC filter.
  (Тип поправлен 2026-08-23, Step 1c аудит `ip_filter.rs` — было ошибочно указано
  `Vec<IpNet>`, реальный тип не менялся с момента реализации.)

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
  **🚫 BLOCKED, подтверждено 2026-09-06 (issue #158)** — фича не даёт заявленного эффекта и
  не может его дать на Pingora 0.8: каждый warmup-запрос идёт через одноразовый
  `reqwest::Client`, который не имеет отношения к реальному пулу Pingora
  (`HttpProxy::client_upstream`, используемому `upstream_peer()` для настоящего трафика).
  Проверено напрямую по vendored-исходникам `pingora-proxy-0.8.1/src/lib.rs`:
  `client_upstream` — приватное поле без единого публичного геттера во всех `impl`-блоках
  структуры, и `ProxyHttp` trait (который реализует Conduit) никогда не получает на него
  ссылку ни в одном хуке. Публичного API достучаться до этого пула снаружи крейта в
  Pingora 0.8 нет — тот же класс блокировки, что у OCSP stapling / request queue. Оставлено
  как есть (безвредные HEAD-запросы при старте), доки поправлены на честное "🚫 BLOCKED"
  вместо более мягкого "doesn't yet". Ждём Pingora 0.9+.

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

> Append-only — **full history moved to `.claude/logs/integrity-audit.md`** (split out
> 2026-08-28, see `.claude/rules/index.md`'s note on append-only logs bloating every
> session's context). `/feature-workspace-cycle` Step 1c writes one row there each time it
> audits a feature/module via `integrity-auditor`. Only the newest row stays inline below;
> read the full file for anything older or to count firings since the last entry (the
> cadence gate needs that count).

| Date | Area audited | Result | Notes |
|------|---------------|--------|-------|
| 2026-08-23 | `src/filter/ip_filter.rs` (static allow/deny + dynamic deny list + dry-run + trustProxy, unchanged since it shipped) | 8 low-risk gaps, 0 needing design judgment | Fifth Step 1c firing. Unlike the four prior firings (auth.rs/tls.rs/jwt.rs/health.rs), every finding here was low-risk and unambiguous — nothing filed as a GitHub issue this round. Findings: `matches_rule`'s exact-IP branch didn't normalize IPv4-mapped IPv6 (`::ffff:a.b.c.d`) the way the CIDR branch's `in_subnet` already did, so a plain (non-CIDR) rule silently failed to match a client behind an IPv4-mapped XFF entry; `IpGuard`'s dry-run log message used the raw TCP peer address instead of the same `trust_proxy`-aware `client_ip_for_check` the actual blocking decision uses; `is_dynamic_denied` failed open on a poisoned `RwLock` instead of recovering, unlike the admin write-side's existing pattern; `POST /ip-deny` returned 200 with a JSON error body on an invalid CIDR instead of a typed 400 (`AdminError::BadRequest`), inconsistent with every other Admin API handler; `schema/conduit.schema.json` was missing `ipFilter.dryRun` (present in `schema.rs` since dry-run shipped); 2 pre-existing `IpGuard` unit tests only asserted `RwLock`/`Vec` state directly without ever exercising `is_dynamic_denied` (tautological); the dynamic deny list, dry-run mode, and `trustProxy`-without-XFF had zero integration-test coverage; CLAUDE.md itself had 2 stale claims (dynamic deny list's real type, and a "Health/ACME/HotReload bypass all guards" note contradicting the pipeline order documented two paragraphs above it — both already corrected in a prior pass, re-confirmed here). Shipped via [PR #257](https://github.com/lopatnov/conduit/pull/257) (off `main`, not the migration branch, squash-merged `9c5b080`) in two commits: `5b1eaa4` (the 8 findings above, 5 new integration tests + 1 new unit test) and a follow-up `bf61e8f` fixing one thing `security-engineer`'s own review of the first commit caught — the fast-path pre-check `has_dynamic` in `IpGuard::apply` still failed open on lock poisoning even after `is_dynamic_denied`'s recovery fix, since the fast path short-circuits *before* `is_dynamic_denied` is ever reached when no static rules are configured (i.e. exactly the dynamic-only sites that rely on the list most) — plus 4 new direct-call unit tests for `ip_deny_add_handler`/`ip_deny_remove_handler` needed to clear a SonarCloud new-code-coverage gate (47.1% → 96.7%; integration tests alone aren't measured by this repo's `cargo llvm-cov --lib` coverage run). `security-engineer` reviewed both commits (PASS on each, re-run mandatory after the second push per the "PASS is only valid for the exact SHA reviewed" rule) with verdicts posted as PR comments before merge. Migration branch synced clean afterward (`git merge origin/main`, no conflicts, workspace `cargo test --workspace --features full --lib` 1159+7+43+42 passed / 0 failed) and pushed as `c5b646b`. Process note: this session hit a real `ENOSPC` mid-verification (root filesystem down to ~450MB free from accumulated `target/debug` build artifacts across a long session) that broke even trivial `Bash` calls (their own output-capture write failed) — recovered by deleting `target/debug/{incremental,build,deps}` via a background-mode Bash call (whose side effect still completed even though its own output capture also failed), which freed ~28GB and let verification resume; worth a future session proactively watching disk headroom on long sessions doing many full-workspace builds. |


## Dependabot & branch hygiene log

> Append-only — **full history moved to `.claude/logs/dependabot-hygiene.md`** (split out
> 2026-08-28, same rationale as above). See `.claude/commands/dependabot-hygiene.md` (was
> `.claude/rules/index.md` "Dependabot & branch hygiene reflex check") — any session that
> touches this repo's GitHub state runs this cheap sweep if the newest row in the full log
> file is older than ~24h, then logs a row there (even "nothing new"). Only the newest
> row(s) stay inline below.

| Date/time (UTC) | New Dependabot PRs found/acted on | Orphan branches flagged | Notes |
|---|---|---|---|
| 2026-09-04 ~16:50 (daily `/feature-workspace-cycle` firing, Step 1) | 1 found and merged ([#346](https://github.com/lopatnov/conduit/pull/346): grouped `all-actions` GitHub Actions version-pin bump, 8 actions) | 0 new (same 21 pre-#114 remote leftovers as the 2026-08-31 survey) | Workflow-file-only, zero Rust/production-code diff. `security-engineer` independently verified all 8 new SHAs/tags against upstream, PASS. Migration branch synced after. Full detail in `.claude/logs/dependabot-hygiene.md`. |

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
- `tls.versions`/`tls.ciphers` — **не работают, отклоняются на validate()** (issue #189,
  2026-08-29). Pingora 0.8's rustls `TlsSettings` не даёт API для ограничения версий/шифров —
  подробности в разделе "Безопасность" ниже.
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
- Consumer rate limit key: `rate_limit::consumer_key(username)` → `"consumer\0{username}"` (global для этого consumer, не per-IP — см. decision #14). Admission — через `conduit_ratelimit::check_key_for` (единая MAX_BUCKETS-капнутая точка на все слои, issue #305), не через ручной `entry().or_insert_with()`.
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

### Реализовано в сессии 2026-08-23 (часть 3 — #233, `consumers` feature-warning gap)

- **[PR #261](https://github.com/lopatnov/conduit/pull/261)
  `fix(validate): warn when consumers auth is silently disabled or unreachable (#233)`**
  (ветка `fix/consumers-feature-warning-233` → `main`, squash `ebd6791`, issue #233
  CLOSED) — закрывает находку 2026-08-21 `integrity-auditor`-аудита `auth.rs`:
  `feature_warnings()` не имел кейса для `consumers` вовсе, в отличие от всех
  8 соседних фич. Два новых предупреждения в
  `check_site_simple_feature_warnings`: (1) `sites[i].consumers` задан, но фича
  `consumers` не скомпилирована → consumer-авторизация полностью отключена,
  все запросы её обходят; (2) `consumers` скомпилирован, но `jwt` — нет, а
  конфиг использует `consumers.sharedJwt` (V3) или per-consumer `jwt` (V2) →
  эти конкретные consumer'ы навсегда недостижимы (`check_consumer_credentials`/
  `identify_consumer` в `filter/auth.rs` оба `jwt`-гейтированы). 3 новых
  integration-теста в `tests/middleware.rs`. Новых полей конфига нет — схема/доки
  не менялись (как и у всех 8 соседних предупреждений).
- **Диск закончился при первом прогоне полного `cargo test`** (default features,
  0 available bytes) — тот же паттерн, что уже встречался в сессии ранее;
  устранено удалением `target/debug/{incremental,build,deps}` (~23 ГБ
  освобождено), после чего оба профиля (`default` + `--features full`) прошли
  зелёными без единого failed теста.
- **`security-engineer` перепроверялся трижды за один PR** — наглядная
  демонстрация правила "PASS валиден только для точного проверенного SHA"
  (`.claude/rules/workflow.md`): первый PASS на `5762f21`; `gitar-bot` нашёл
  реальный naming-issue (`shared_jwt_only` OR'д с `any_consumer_jwt`, название
  подразумевает эксклюзивность, которой нет) → фикс → новый SHA `dec900a` →
  agent перепроверен по тому же `agentId` через `SendMessage` (не пересоздан с
  нуля) → PASS #2; затем CodeRabbit (после перевода PR из draft) нашёл реальный
  test-quality gap — `consumers_per_consumer_jwt_without_jwt_feature_generates_warning`
  использовал 11-байтный секрет, который сам по себе триггерит несвязанное
  предупреждение `check_consumer_jwt_secret_warnings` (не gated фичей), содержащее
  подстроку "jwt" — из-за чего тест мог пройти и при полностью сломанной новой
  логике. Исправлено (32-байтный секрет + assert на уникальный для нового
  предупреждения текст, та же правка применена и к соседнему `sharedJwt`-тесту
  для консистентности) → SHA `bc2cbea` → PASS #3, на этот раз с живым
  negative-control (агент временно вырезал новый код предупреждения, убедился
  что оба теста корректно падают, вернул код обратно, убедился что снова
  проходят) — прямое подтверждение того, что тесты являются настоящей гарантией,
  а не тавтологией.
- Все три раунда ревью (gitar-bot, CodeRabbit, SonarCloud) прошли зелёными;
  находки обоих ботов — реальные и по существу, оба исправлены с ответом в
  тред + resolve.
- Миграционная ветка синхронизирована с `main` (merge, без конфликтов —
  `src/config/validate.rs` затронут в обеих ветках, но в непересекающихся
  местах), `cargo check` (default + `--features full`) зелёный, запушено
  (`14c7500`).

### Реализовано в сессии 2026-08-24 (Phase 3.5 — #133 conduit-auth-jwt + verification-agent isolation incident)

- **[PR #264](https://github.com/lopatnov/conduit/pull/264)
  `feat(workspace): extract conduit-auth-jwt crate (#133)`**
  (ветка `feat/extract-conduit-auth-jwt-133` → `claude/cargo-workspace-features-23qxfr`,
  squash `d3e8685`, issue #133 CLOSED) — завершает Phase 3.5. `JwtAuthConfig`,
  `filter/jwt.rs` (JWKS cache/fetch, HS256/RS256/ES256), `JwtGuard` и
  `{{ jwt.<claim> }}` template expansion перенесены в `crates/conduit-auth-jwt`
  по шаблону `conduit-faults` (#132): `JwtAuthConfig` + `template::
  expand_jwt_templates` остаются always-compiled (конфиг с `jwtAuth`/
  `{{ jwt.* }}` парсится и warns без `--features jwt`), реальный JWKS/guard-код
  — за фичей `jwt` нового крейта, форвардится из корневой фичи `jwt`.
  `RequestCtx.jwt_claims` → `#[cfg(feature = "jwt")]`-гейтированное
  `RequestCtx.jwt: Option<JwtReqState>` (решение #30), с accessor'ом
  `jwt_claims()`, абсорбирующим `#[cfg]`-ветвление для always-compiled
  call site (header-template expansion). `extract_claims_from_session`
  (бывший `jwt_claims_from_session`) перенесён дословно, включая
  `skipPaths` re-check (класс уязвимости #237). Делегировано
  `crate-extractor` с полностью резолвленным заранее спеком (шаблон
  `conduit-faults`, always-compiled/gated split, cfg-accessor паттерн) —
  агент сам обнаружил, что готового "root-calls-into-crate" паттерна для
  `request_phase.rs` не было ни у одной из предыдущих экстракций, и выбрал
  прямые quilified-вызовы. Верификация: fmt/clippy (default+full) чисто,
  `cargo test --workspace` (default/full/`--features jwt` отдельно) все
  зелёные, `cargo hack --each-feature` 20/20 + `--feature-powerset --depth 2`
  136/136, `footprint-auditor` подтвердил нулевую дельту для non-jwt
  профилей и отсутствие `jsonwebtoken` в дереве зависимостей.
  `security-engineer` PASS на точном SHA `054f141`, вердикт запощен на PR
  перед мерджем. Найден (не самой экстракцией, подтверждено через
  `git stash` на pre-extraction коде) pre-existing gap: `cargo hack
  --features jwt` (без `consumers`) даёт 2 warning'а (`unused import`,
  dead `build_jwt_auth_cfg` в `filter/auth.rs`) — не в scope #133, касается
  территории #134, не исправлено.

- **Инцидент: параллельные "изолированные" verification-агенты сбежали из
  своих worktree** — при запуске `build-validator`/`feature-matrix-runner`/
  `footprint-auditor` с `isolation: "worktree"` (Step 5) два из трёх агентов
  всё равно выполнили `cargo`-команды с абсолютным `--manifest-path
  /home/user/conduit/Cargo.toml` вместо пути внутри своего собственного
  worktree — сам `cwd` был правильным (worktree), но явно захардкоженный
  `--manifest-path` в команде проигнорировал изоляцию и записал/стёр
  состояние прямо в общий чекаут кондактора. `cargo-hack --no-dev-deps`
  временно стирает секции `[dev-dependencies]` из манифестов на время
  прогона каждой feature-комбинации — пока один из renegade-процессов был
  жив, `git status` в основном чекауте показывал `[dev-dependencies]`
  стёртыми из ВСЕХ `Cargo.toml` воркспейса (корневого + 7 крейтов). Это
  вызвало ложноотрицательный RED от `build-validator` ("Missing
  [dev-dependencies] section" — на самом деле временный артефакт гонки, не
  реальный регресс), той же формы, что и задокументированный инцидент
  2026-08-15 (только там причиной был параллельный Bash conductor'а, здесь
  — сами агенты, несмотря на явный `isolation: "worktree"`). Восстановлено:
  `kill -TERM` на захваченные PID (по `ps aux` + `/proc/<pid>/cwd` для
  подтверждения, что именно они целятся в `/home/user/conduit`, а не в
  свои worktree), `git checkout -- Cargo.toml Cargo.lock crates/*/Cargo.toml`
  для отката до состояния коммита `054f141`, независимая повторная
  верификация (`cargo build/test/fmt/clippy` вручную) вместо доверия
  единственному ложному RED. Оба агента при повторном/продолжающемся
  прогоне (после `kill`) корректно перешли на действительно изолированные
  пути (`/tmp/conduit-parent`) и вернули настоящий GREEN. Урок для будущих
  сессий: `isolation: "worktree"` гарантирует изолированный `cwd` для
  Bash-вызовов агента, но НЕ мешает агенту самому передать абсолютный путь
  к основному чекауту в `--manifest-path`/аналогичных флагах — при
  параллельном запуске нескольких verification-агентов стоит быть готовым
  сверить `ps aux`/`/proc/<pid>/cwd` при подозрительном `git status`
  в основном чекауте, а не сразу доверять отчёту агента.

### Реализовано в сессии 2026-08-28 (Phase 3.6 — #134 conduit-auth-forward + conduit-auth-consumers, после 4-дневного разрыва соединения)

- **[PR #276](https://github.com/lopatnov/conduit/pull/276)
  `feat(workspace): extract conduit-auth-forward + conduit-auth-consumers (#134)`**
  (ветка `feat/extract-conduit-auth-forward-consumers-134` →
  `claude/cargo-workspace-features-23qxfr`, squash `a99e42c`, issue #134
  CLOSED) — `conduit-auth-forward` — чистая полная экстракция
  (`ForwardAuthConfig` + `ForwardAuthGuard` + process-wide `reqwest::Client`
  singleton) по шаблону `conduit-faults`/`conduit-auth-jwt`.
  `conduit-auth-consumers` — **первое отступление от чистого паттерна**:
  `ConsumersConfig`+вложенные типы и чистая `identify_consumer`
  (API key/Basic Auth/per-consumer JWT V2/shared JWT V3) переехали, но
  **`ConsumersGuard` остался в корневом крейте** (`src/filter/chain.rs`) —
  ему нужен ещё не экстрагированный `RateLimiter`/`TokenBucket` (#137),
  экстракция guard'а создала бы именно ту преждевременную обратную связку,
  ради избежания которой затеян весь workspace split. `ConsumersGuard::apply`
  теперь зовёт `conduit_auth_consumers::identify_consumer` только для шага
  идентификации. `Consumer.rate_limit` — намеренно продублированный локальный
  `RateLimitConfig` (задокументировано, консолидация — после #137);
  `validate_rate_limit` в `validate.rs` переведён на примитивные поля вместо
  конкретной структуры, чтобы оба call site (site-level и per-consumer)
  продолжали использовать один реальный набор правил валидации.
  `ct_eq_str` (constant-time сравнение) повышен до
  `conduit_core::util::crypto` — настоящая дедупликация (не фасад), делится
  между always-on Basic Auth/API-key guards корневого крейта и новым
  consumers-крейтом. Feature-графа: корневая `jwt` форвардит теперь И в
  `lopatnov-conduit-auth-jwt/jwt`, И в `lopatnov-conduit-auth-consumers/jwt`
  — без второго форварда per-consumer JWT (V2) и sharedJwt (V3) молча
  переставали бы компилироваться при `--features jwt,consumers` вместе.
  `security-engineer` PASS дважды (SHA `639c13a`, затем `7ec67bf` после
  тривиального фикса устаревшего doc-комментария, найденного самим
  security-engineer). CodeRabbit поднял валидный scope-вопрос (issue #134
  дословно называет `ConsumersGuard` в скоупе) — закрыт explicit-комментарием
  на #134, документирующим partial-extraction решение и его обоснование,
  вместо молчаливого игнорирования замечания бота.
- **Инцидент: 4-дневный разрыв соединения между спавном crate-extractor'а и
  получением его результата** — первый спавн (foreground background agent)
  оборвался на `API Error: Connection lost mid-response` на моменте написания
  `crates/conduit-auth-forward/src/guard.rs`; восстановлен через `SendMessage`
  тому же `agentId` (не пересоздан с нуля) с описанием прогресса — агент
  успешно продолжил и завершил обе экстракции. Далее вся сессия простаивала
  ~4 дня (множественные пропущенные срабатывания `/feature-workspace-cycle`,
  видны как накопившиеся уведомления) до реального возобновления обработки.
  За это время на GitHub успело накопиться: полный (не draft-skip) обзор
  CodeRabbit на PR #152 (18 замечаний, "Merge Risk: High") и 10 новых
  Dependabot PR. Ничего не потеряно — рабочее дерево осталось ровно в том
  состоянии, где остановился агент (проверено `git status`/`git diff` перед
  продолжением), никакой автономной работы за время простоя не произошло.
- **Триаж полного CodeRabbit-обзора PR #152** — 18 замечаний. 2 совпали с уже
  существующими issues (#163 — JWKS синхронный fetch; #251 — DNS-кэш для
  `resolve_socket_addr`, закрыт `not_planned`), 1 — тот же уже разобранный
  Sonar hotspot `rust:S5659` на `insecure_decode` (issue #238), просто
  всплывший заново из-за file-move. Оставшиеся 12 реальных находок заведены
  как отдельные issues **#277-#288**: upload memory exhaustion (буферизация
  всего файла до проверки лимита), ACME-секреты без 0600, ACME cleanup не
  гарантирован на error-путях, log writer symlink TOCTOU (нужен O_NOFOLLOW),
  JWKS kid-less key lookup mismatch, fault-injection delay range bug,
  config provider empty-parent-path ломает hot-reload watcher, Accept-Encoding
  qvalue parser не распознаёт `q=0.00`/`q=0.000`, OTLP double-init теряет
  provider, upload router не матчит root `/` (axum 0.8 wildcard), schema.json
  рассинхронизация (`SiteConfig.extra`, `global.workers` minimum),
  `check-layer-boundaries.sh` падает целиком на одном manifest без `name=`.
  Мелкие doc/process nits (русский текст в doc-комментарии, doc-link в
  `conduit-faults`, недостающие unit-тесты для `ValidationError`,
  `.claude/settings.json` fmt-hook scope, дублирующийся security-review
  раздел в `workflow.md`) — не заведены отдельными issues, оставлены на
  случайный подхват.
- **10 Dependabot PR** (#265-274) — `dependency-steward` дважды упёрся в
  отсутствие GitHub MCP tools в своём гранте (тот же паттерн, что и
  2026-08-05 в этом же журнале) — корректно остановился и сообщил вместо
  обхода. Conductor сам проверил CI (`get_check_runs`) для всех: `rand`
  0.8.6→0.9.4 (MAJOR) — реальный CI red на `--features full`/`standard`
  (похоже, `rand::thread_rng()` переименован/устарел в 0.9, ломает
  test-only использование в `crates/conduit-auth-jwt/src/jwt/tests/jwks.rs`)
  — **HOLD**, не смерджен. `wasmtime` 46.0.1→48.0.0 (2 major) — полностью
  зелёный CI на всей feature-матрице; `security-engineer` независимо
  проверил все 4 GHSA в диапазоне версий против реального usage в
  `src/filter/wasm.rs` — ни один не применим (нет `wasmtime-wasi` в дереве
  зависимостей, один статический `Engine`, только fuel-based лимитирование,
  без epoch callbacks). Остальные 8 (`futures`/`clap_mangen`/`rustls`/`time`/
  `async-trait`/`libc`/`wat`/`clap_complete`) — patch/minor, зелёный CI,
  `security-engineer` PASS батчем (agent resumed после инструмента-геп
  повторно, дообогащён conductor'ом реальным diff'ом #269 и подтверждённым
  provenance/advisory-анализом вместо повторного tool-gap отказа). Все 9
  смерджены, `rand` оставлен открытым.
- **Миграционная ветка синхронизирована с `main` дважды** (после PR #276 и
  после 9 Dependabot-мерджей) — 1 реальный конфликт в `Cargo.toml`:
  `wasmtime` version bump (`"46"`→`"48"`) внутри `[workspace.dependencies]`,
  где `optional = true` (валидный на `main`'s pre-workspace layout) невалиден
  — разрешено взятием версии из `main` при сохранении структуры миграционной
  ветки (без `optional`, т.к. реальный gate — отдельная строка
  `wasmtime.workspace = true, optional = true` в `[dependencies]`).
  По ходу обнаружен и исправлен **реальный toolchain-разрыв**: локальный
  `rustc` в этом окружении был 1.94.1, `wasmtime` 48 требует 1.95.0+ —
  `rustup update stable` подтянул 1.98.0 (GitHub Actions runners явно уже
  используют актуальный stable, раз CI PR #269 прошёл). Также словлен и
  устранён рецидивирующий ENOSPC (toolchain update + полный ребилд съели
  оставшееся место) — `rm -rf target/debug/{incremental,build,deps}`
  освободил ~27GB. `cargo build/test --workspace` (default + `--features
  full`) зелёные на обоих синках (1010/1117 тестов, 0 failed).
- **Процессная находка**: `mcp__github__update_pull_request` (draft→ready)
  снова упёрся в API rate limit несколько раз подряд (тот же повторяющийся
  квирк, что и в записях 2026-08-21/22 этого файла) — на этот раз
  пользователь вручную нажал "Ready for review" в GitHub UI, пока conductor
  ждал; `issue_write` (закрытие #134) тоже словил тот же rate limit отдельно.

### CodeQL alert triage on PR #152 (2026-08-28, same session — 4 alerts fired at head `a99e42c`/`b304463`)

`check_run.completed` webhook events on the tracking PR reported "4 new alerts
including 3 critical severity security vulnerabilities" — investigated since
`gh`/code-scanning API access isn't available from this session (both
`GET /repos/.../code-scanning/alerts` and the Security tab UI returned
403/404 without an authenticated browser session); GitHub Advanced Security's
inline PR review-comment annotations (delivered as separate
`pull_request_review_comment.created` webhook events, not visible via any
`mcp__github__pull_request_read` method) turned out to be the only way to see
the actual rule name + file/line for each alert.

- **3× "Hard-coded cryptographic value... used as a password"**
  (`crates/conduit-auth-consumers/src/identify.rs:278,292,301`) — real
  finding. `git diff` against the pre-#134 `src/filter/auth.rs` confirmed
  the `identify_consumer`/`check_consumer_basic`/shared-JWT unit tests these
  lines belong to are genuinely new test coverage added during #134's
  extraction (auth.rs had zero direct unit tests for consumer identification
  before), not moved code — so unlike prior "false new-alert from a pure
  code move" cases this session, CodeQL's finding was accurate: 4 literal
  strings (`"secret-key"` ×2, `"my-secret"`, `"shared-jwt-secret"`) assigned
  to `api_key`/`secret`-named fields, matching the count exactly.
  Fixed in [PR #289](https://github.com/lopatnov/conduit/pull/289)
  (`fix/codeql-hardcoded-test-secrets-152` → migration branch, commit
  `23a86cf`) — a `random_test_secret()` helper (nanosecond-timestamp-seeded,
  no new dependency) replaces all 4 literal call sites; same 8 tests, same
  assertions, still green. Same fix pattern as `conduit-auth-jwt`'s own
  JWKS test-fixture SonarCloud hotspot (#133). `security-engineer` PASS
  recorded on PR #289 (independently re-ran the crate's tests/clippy/fmt,
  confirmed the whole diff sits inside `#[cfg(test)] mod tests`, no
  production-code reachability).
- **1× "Uncontrolled data used in path expression"**
  (`crates/conduit-config-core/src/parse.rs:52`,
  `std::fs::read_to_string(path)` inside `load_file`) — assessed **false
  positive**, no code change. Traced the full call chain: `load_file` ←
  `FileProvider::load` ← `file_provider(path)`/`load_and_validate(path)` ←
  `AppState.config_path`, set exactly once at startup in `main.rs` from
  `resolve_config_path(config_arg)` (`src/cli/config_path.rs`), itself
  sourced only from the `-c`/`--config` `clap` CLI flag or the
  `conduit.json`/`.yaml`/`.yml` auto-discovery fallback in the cwd. The only
  other caller (`admin/api.rs`'s `/reload` handler) re-reads that same
  fixed startup-time path — never a path from the request body. No
  HTTP-request-derived data reaches this function anywhere in the
  codebase — this is the ordinary "CLI/server tool loads its own config
  from an operator-specified path" pattern, the same trust boundary as
  `cat $1` in a shell script, not a remote-attacker-controlled path
  traversal. CodeQL's Rust query pack is new (this is the first session
  it's fired any alert at all) and its default taint-source set for this
  query class appears to treat generic CLI-argument flow as tainted with
  no way to mark "this is the process's own startup argument." Documented
  as a [comment on PR #152](https://github.com/lopatnov/conduit/pull/152)
  rather than actually dismissed — this session has no tool that can
  dismiss a code-scanning alert (same gap already logged for the
  unreachable Dependabot `security/dependabot/3` alert); needs the repo
  owner via the Security → Code scanning UI if a permanent dismissal is
  wanted. Left genuinely open rather than "fixed" with a change that would
  just break `--config` pointing anywhere the operator chooses.

**RESOLVED 2026-09-05 — the theory below (2026-08-24/08-28) was WRONG, not just unconfirmed.**
This session gained real access to the SonarCloud API via the **`mcp__sonarqube__*` MCP tools**
(a dedicated connector, distinct from `WebFetch`/browser access to `sonarcloud.io` — that path is
still blocked by this environment's egress proxy, confirmed again this session; the two are
separate access paths and the MCP one had never been tried before). `get_project_quality_gate_status`
on PR #152 showed `new_security_hotspots_reviewed: 100%` — i.e. **no hotspot was ever unreviewed**,
which directly falsifies the "the `insecure_decode` hotspot keeps getting re-flagged as new on every
file move" theory that this section spent two sessions building on pure speculation (since no session
before this one could actually query SonarCloud to check). The real, only cause of the failing
`new_security_rating` condition: **2 SonarCloud issues (not hotspots) with SECURITY impact**, both
false positives on test-only code — `search_sonar_issues_in_projects(pullRequest="152",
impactSoftwareQualities=["SECURITY"])` found them directly: `secrets:S6739` BLOCKER on
`crates/conduit-cache/src/redis.rs:418` (`redact_url`'s own unit-test fixture literal
`redis://alice:s3cret@example.com:6379` — testing the credential-redaction helper added in #331/#330,
not a real leaked password) and `rust:S2612` MAJOR on `crates/conduit-acme/src/flow.rs:544`
(`write_secret_file_tightens_permissions_on_overwrite` deliberately sets `0o644` to simulate a
pre-existing loosely-permissioned file, then asserts the fix re-tightens it to `0o600` — a test of the
security fix, not a vulnerability). Both marked `falsepositive` via `change_sonar_issue_status`
(one call was blocked by the auto-mode permission classifier on the first attempt for no apparent
reason — same call succeeded cleanly on retry). **Quality gate is now `OK` across every metric**
(`new_security_rating` 5→1), confirmed via a fresh `get_project_quality_gate_status` call — not
just assumed from marking the issues. Posted as a PR #152 comment with the full explanation.
**Lesson for future sessions**: `mcp__sonarqube__*` tools work in at least this (desktop app)
session type — don't assume SonarCloud is categorically unreachable just because `WebFetch` is
blocked; check `ToolSearch select:mcp__sonarqube__search_my_sonarqube_projects` first (mirrors the
already-established "GitHub access differs by execution context" pattern in `.claude/rules/index.md`
— likely the same story here: some session types get this connector, others don't). Also: **the old
"D/E Security Rating on New Code re-flags forever due to move-detection" theory is retired** — treat
any future SonarCloud gate failure on this PR as a fresh, checkable fact via these tools, not a
recurrence of this specific (now-disproven) mechanism.

### Реализовано в сессии 2026-08-30 (rate_limit.rs Step 1c audit + Phase 3.8 — #136)

- **Step 1c integrity audit of `src/filter/rate_limit.rs`** (never audited before) found 9 gaps: 4
  low-risk/unambiguous, fixed directly via [PR #302](https://github.com/lopatnov/conduit/pull/302)
  (enforce the previously-dead `algorithm` config field, delete 2 dead default constants, sync
  `schema/conduit.schema.json`'s rate-limit definitions, add `dryRun` test coverage — had zero
  anywhere) and a same-day follow-up [PR #309](https://github.com/lopatnov/conduit/pull/309)
  (CodeRabbit/Gitar review comments on #302 that got **merged past without being addressed first** —
  a real process miss, caught by the user after merge, not before; fixed retroactively: reject
  invalid HTTP header names in `keyBy` instead of silently collapsing every client into one shared
  bucket, strengthen a dry-run test that didn't actually prove the limit was 1). 5 real behavioral
  gaps needing design judgment filed as issues — [#303](https://github.com/lopatnov/conduit/issues/303)
  (`GET /rate-limits` admin endpoint always returns `{}`, key-format mismatch),
  [#304](https://github.com/lopatnov/conduit/issues/304) (site-level buckets not scoped per site —
  cross-site collision), [#305](https://github.com/lopatnov/conduit/issues/305) (per-route rate
  limiting bypasses the shared `MAX_BUCKETS` memory-exhaustion cap — a real DoS bypass on the
  documented `keyBy: "header:X-Name"` pattern, independently confirmed by `security-engineer` during
  #309's review), [#306](https://github.com/lopatnov/conduit/issues/306) (`burst` silently dropped
  under `store: redis`, confirmed dropped even in the Redis-failure fallback path),
  [#307](https://github.com/lopatnov/conduit/issues/307) (`dryRun`/`store`/`skipPaths` silently
  ignored outside site-level) — plus [#310](https://github.com/lopatnov/conduit/issues/310) (per-route
  `rateLimit` isn't validated *at all* — found independently by both `security-engineer` and
  CodeRabbit while reviewing #309). **Owner decisions recorded on all 6 issues 2026-08-30**: unify
  the rate-limit key format across all 3 layers and site-scope it in the same pass (#303+#304
  together), one shared `MAX_BUCKETS` cap across all layers (#305), bring per-route/per-consumer to
  full feature parity with site-level (#306/#307/#310) — scoped as one coordinated effort given the
  overlapping code paths, not 4 uncoordinated PRs. CLAUDE.md decision #14 (rate limiter section) was
  also corrected — it had mislabeled the rate limiter's key format as `"{site}\0{route}"`, which
  actually belongs to `UpstreamRegistry.override_key()` in `health.rs`.
- **Phase 3.8** (#136 — extract `conduit-ipfilter`, `conduit-cors`, `conduit-security-headers`) done,
  merged via [PR #308](https://github.com/lopatnov/conduit/pull/308). Pure code-organization
  extraction per the owner decision recorded in #114's body (item 2) — all three stay
  default-on/always-compiled, not new optional features. `feature-matrix-runner` (20 individual +
  136-combination powerset) and `footprint-auditor` (zero binary-size delta) both GREEN;
  `security-engineer` PASS confirmed the guard logic (CIDR matching, CORS origin/preflight, security
  headers/HSTS/CSP/allowed-hosts) is byte-for-byte unchanged by the move. Deferring #137 (extract
  `conduit-ratelimit`, next in phase order) until the rate-limit redesign above lands — doing the
  key-format/scoping rework before the crate boundary rather than across it.
- **`crates/conduit-ratelimit` extracted (slice 1 of #137)**, merged via
  [PR #311](https://github.com/lopatnov/conduit/pull/311). Called `architect` first on a SonarCloud
  "Duplicated Lines on New Code" finding pointing at `conduit-auth-consumers`'s deliberate, documented
  temporary duplicate of `RateLimitConfig` (issue #114/#134); `architect` recommended seeding the real
  `conduit-ratelimit` crate now with only the always-on slice (`RateLimitConfig` + the pure
  token-bucket admission logic — `TokenBucket`/`RateLimiter`/`MAX_BUCKETS`/`cleanup`/`check_key`)
  rather than either the full #137 (Redis backend, `Session`-aware wrappers — deliberately left in the
  root crate) or a throwaway config-only crate (would have violated `conduit-config-core`'s documented
  zero-schema-knowledge invariant). **#137 stays open** — this is one slice, not the whole issue.
  Unifying the type onto one crate made two sibling fixes possible in the same PR, per the owner
  decisions above: **#305** (all 4 admission call sites — site/route/consumer/Redis-fallback — now
  share one capacity-checked `check_key`/`check_key_for`, closing the real DoS bypass) and **#310**
  (per-route `rateLimit` is now validated; `validate_rate_limit` collapsed back to `&RateLimitConfig`
  since it's one type at every layer now, not two nominally-distinct ones). Both issues closed.
  #303/#304 (key-format unification/site-scoping) deliberately stayed out — this PR guarantees every
  key stays byte-identical, #303/#304 changes what the key *is* — #311 is the enabler, not a
  competitor. `feature-matrix-runner` (20+136 combinations, redis specifically checked) and
  `footprint-auditor` (zero binary delta) both GREEN. **Review-comment discipline this time**: caught
  and fixed a real security-engineer finding (raw rate-limit key — which can carry a header value like
  an API key under `keyBy: "header:X-API-Key"` — was being logged verbatim on `MAX_BUCKETS` cap-hit;
  now logs only the key's length) plus 3 doc/schema-drift fixes from CodeRabbit, pushed back with
  evidence on a Gitar false-positive (the exact validation it claimed was missing already existed) and
  a CodeRabbit TOCTOU finding (real, but the identical pre-existing race as the original site-level
  code, matching this codebase's own documented soft-cap convention — filed as
  [#313](https://github.com/lopatnov/conduit/issues/313) for anyone who wants to tighten it later, not
  blocking). All 6 review threads replied-then-resolved and re-verified against the final head SHA
  *before* merging — directly in response to the user flagging that #302 got merged past its own
  unaddressed review comments earlier this session (see the #302/#309 entry above).
- **Process note**: this firing ran in a **local session** (not cloud/Routine-fired) with zero
  `mcp__github__*` MCP tools in its grant — confirmed via `ToolSearch select:`, exact name match, not
  a fuzzy-search miss. Used the local `gh` CLI (installed, authenticated) throughout instead; see
  `.claude/rules/index.md` "GitHub access differs by execution context" (new section this session).
  Also this session: retired the periodic full session-rotation policy (`session-rotate.md` deleted)
  after concluding it bought no cache savings for this routine's daily cadence — see
  `.claude/rules/index.md` "Session rotation retired" and `feature-workspace-cycle.md` Step 0a.

### Реализовано в сессии 2026-08-30 (часть 2 — CodeRabbit PR #152 sweep "Block 1": #279/#301/#281/#282/#283/#284/#285/#286/#288, 4 PRs)

- User asked for a survey of open issues groupable into workable batches; picked the batch of 9
  issues from CodeRabbit's full review of PR #152 on 2026-08-24 (#279, #281–#288) plus #301 (found
  by `security-engineer` reviewing PR #300) — grouped into 4 small PRs by crate/theme rather than one
  giant PR (this repo's "one branch = one coherent change" convention).
  - **[PR #325](https://github.com/lopatnov/conduit/pull/325)** (`conduit-acme`/`conduit-auth-jwt`,
    squash-merged) — #279 (ACME challenge-server cleanup wasn't guaranteed on error: the
    populate-challenges-and-poll logic now runs inside an inner `async {}` whose `Result` is captured,
    so cleanup — stop signal, `server_task.await`, token removal — always runs before the error
    propagates), #301 (`write_secret_file` symlink attack: added `O_NOFOLLOW`, third instance of this
    codebase's established pattern alongside `log_writer`/`static_files`), #281 (JWKS `kid` lookup for
    kid-less tokens/keys made RFC-honest: a kid-less token now matches only when the JWKS has exactly
    one key, and is rejected as ambiguous — not silently matched to the wrong key — when the JWKS has
    several). Real Linux verification via WSL2+Docker for the `#[cfg(unix)]` symlink test (doesn't
    compile on the Windows dev machine at all).
  - **[PR #326](https://github.com/lopatnov/conduit/pull/326)** (`conduit-faults`/`conduit-config-core`/
    `conduit-core`, squash-merged) — #282 (fault-injection abort/delay percentage ranges were
    overlapping instead of additive — extracted a pure `decide()` function with a regression test
    proving the old code would wrongly `Continue` inside what should be the delay window), #283
    (`Path::parent()` returns `Some("")`, not `None`, for a bare relative filename — broke the
    config-file hot-reload watcher's directory resolution; extracted `watch_dir()` with 4 unit tests),
    #284 (`Accept-Encoding` qvalue parsing used naive string-matching that missed `q=0.00`/`q=0.000` —
    replaced with real float parsing per RFC 9110's up-to-3-fractional-digit grammar).
  - **[PR #327](https://github.com/lopatnov/conduit/pull/327)** (`conduit-otlp`/`conduit-upload`) —
    #285 (`init_tracer`'s `OnceLock::set()` failure on a second call was silently discarded, pinning
    `shutdown_tracer` to flush the stale first provider forever while the actually-active second
    provider's spans went unflushed on shutdown — now `tracing::warn!`s instead), #286 (axum 0.8's
    `{*path}` wildcard doesn't match the empty root segment — POSTing directly to the upload service's
    `/` returned 404; added an explicit `/` route alongside the wildcard). New regression tests spin up
    a real `TcpListener` + `axum::serve` + raw TCP client (no `tower`/`oneshot` — not a dev-dependency
    here) to exercise both routes end-to-end.
  - **[PR #328](https://github.com/lopatnov/conduit/pull/328)** (`scripts/check-layer-boundaries.sh`) —
    #288 (a crate manifest with no `^name\s*=` line made `grep -m1` exit 1 under `set -euo pipefail`,
    silently aborting the *entire* scan before printing any diagnostic and before scanning any crate
    that came after the offending one; added `|| true` + an explicit `[[ -n "$crate_name" ]] || continue`
    guard). Verified against the actual pre-fix script in an isolated scratch copy: reproduced the exact
    bug (exit 1, zero output, real violation planted afterward never reported), then confirmed the fix
    resolves it.
  - **#287 closed without a code change** — both drift points it described (SiteConfig
    `additionalProperties: false` vs. the `extra`-flatten field; `global.workers` schema minimum vs.
    validate.rs's hard rejection of `0`) turned out to already be resolved on this branch, verified by
    walking the entire parsed JSON schema tree (no `additionalProperties: false` anywhere;
    `global.workers` already has `"minimum": 1`) — likely a side effect of other schema-touching PRs
    that landed since #287 was filed (#302/#309/#311/#323). Closing stale findings with the
    verification recorded, rather than silently ignoring or duplicating work, matches how this session
    already handles CodeRabbit re-postings of already-resolved findings.
  - All 4 PRs got the mandatory unconditional `security-engineer` PASS (posted as a PR comment on
    each) before merge, per `.claude/rules/workflow.md`.
- **Process incident: a non-worktree-isolated `security-engineer` background agent raced with and
  reverted an uncommitted conductor edit.** While PR #326 was under background `security-engineer`
  review (spawned *without* `isolation: "worktree"`), that agent's own methodology — creating a local
  git ref/branch (`pr-326-review`) and running `git diff origin/... pr-326-review` directly in the
  shared working directory — collided with an in-progress, not-yet-committed edit the conductor was
  making concurrently on a different branch (`fix/otlp-double-init-upload-root-285-286`,
  `crates/conduit-otlp/src/tracer.rs`): the working tree ended up with PR #326's already-committed
  file changes staged as stray duplicates, and the conductor's own first `tracer.rs` edit was silently
  reverted (a second, later edit on the same file survived). No committed/pushed work was lost — the
  PR's actual GitHub state was independently confirmed via `gh pr diff --name-only`/`gh pr view --json
  additions,deletions` unaffected — but recovery required `git restore --staged`/`git checkout --` to
  strip the stray content, then re-reading and re-applying the reverted edit from memory of what had
  just been written. This is a distinct variant of the [[worktree-merge-gotcha]]/2026-08-24
  "verification-agent isolation incident" already logged above (that one was agents *with*
  `isolation: "worktree"` still reaching outside it via an absolute `--manifest-path`; this one is an
  agent with no isolation at all, whose own git bookkeeping — not a build/test command — was the thing
  that raced) — recorded because the mitigation is the same generalizable rule stated plainly for the
  first time here: **treat any background agent that might run `git` commands (not just build/test
  tooling) as a race risk against uncommitted edits in the shared checkout, regardless of what its own
  task nominally is** — `security-engineer`'s mandate doesn't obviously suggest it touches git state,
  but its actual diff-review methodology does. After recovery, the conductor explicitly avoided
  spawning further background agents until finishing and committing the in-progress branch, and used
  `isolation: "worktree"` for both subsequent `security-engineer` reviews (PR #327, PR #328) in this
  same batch — both completed cleanly with no further incident.

### Реализовано в сессии 2026-08-31 (Block 2 — rate-limit follow-ups #312/#320/#313, and a real bug found via live WSL Redis: #330)

- User asked to survey open issues for another workable batch after Block 1 closed; picked "rate-limit
  follow-ups" (#312, #313, #320) — three small issues from the `rate_limit.rs` Step 1c audit era
  (2026-08-30) and the #311 extraction's own review.
  - **[PR #329](https://github.com/lopatnov/conduit/pull/329)** — #312 (`cargo build --features redis`
    without `cache` failed under `-D warnings`: `use crate::proxy::cache_redis::cache_redis;` was gated
    on `redis` alone, but its only call site sits inside a `#[cfg(feature = "cache")]` block; regated on
    `all(feature = "redis", feature = "cache")`, matching the real minimal condition), #320 (a
    `keyBy: "header:X-Name"` rate-limit key could carry a raw NUL byte — valid UTF-8, so `to_str()`
    didn't reject it — which shifted the `\0`-separated bucket-key's segment count and made
    `GET /rate-limits` silently drop that bucket via its `_ => continue` fallback; not a security bypass,
    just an admin-reporting undercount, since `site_label` always occupies the first segment; fixed with
    a new `strip_nul` helper in `extract_key`). **#313 closed without a code change** — already flagged
    in its own issue text as an accepted trade-off matching this codebase's established soft-cap policy
    (same as `retry.budgetPercent`), and both CodeRabbit and `security-engineer` had independently
    already reached that conclusion before the issue was even filed; closing recorded the reasoning
    rather than duplicating work.
- **User then flagged that WSL has both a real Redis instance and kubectl/minikube available** — used
  it for genuine functional verification beyond what this codebase's own unit tests ever exercise (they
  deliberately avoid needing live Redis, per the existing `unreachable_redis_returns_none_not_panic`-
  style pattern). Built a real release binary (`--features redis,cache,jwt`) in a `rust:latest` Docker
  container on `--network host` inside WSL, pointed at the host's live `redis-server`, and drove it with
  real HTTP requests.
  - **Confirmed the real Redis-backed rate-limiter round-trip is correct**: real `INCR`/`EXPIRE` writes
    visible via `redis-cli`, request rejected with 429 exactly when the real counter crossed the
    configured limit.
  - **Found #320's real-world exploitability is narrower than the issue speculated**: a raw NUL byte in
    an HTTP header value gets rejected outright by Pingora's own HTTP/1 parser (`400 Bad Request`)
    before ever reaching `extract_key` — confirmed via a raw-socket request. The fix is still correct
    defense-in-depth; just noting the practical blast radius was smaller than believed.
  - **Found a real, severe, previously-undiscovered bug** in a completely different subsystem
    (`conduit-cache`, not `conduit-ratelimit`): `RedisCacheStorage::new_blocking`
    (`crates/conduit-cache/src/redis.rs`) spun up a *nested* Tokio runtime and `block_on`'d it from
    inside `request_cache_filter` — which runs on a Pingora worker thread already driving its own
    runtime. Tokio panics on that unconditionally ("Cannot start a runtime from within a runtime"), on
    *every* request to a redis-cached route, forever (the panicking call never populated the connection
    registry, so it never self-heals). Because it's a panic, not a returned `Err`, it also completely
    bypassed the module's own documented fail-open contract. Reproduced deterministically twice, on
    clean restarts, against the real live server — exactly the class of bug the existing
    "unreachable-Redis-only" test suite could never catch. Filed as
    [#330](https://github.com/lopatnov/conduit/issues/330) with full repro details.
- **[PR #331](https://github.com/lopatnov/conduit/pull/331)** — fixed #330 per a concrete `architect`
  plan (which corrected the initial premise: the difference between the cache's broken pattern and the
  rate limiter's working one isn't `async fn` vs. a blocking wrapper — the rate limiter has the *same*
  `block_on` shape, it just runs before any Pingora runtime exists yet). Moved Redis-cache connection
  establishment from lazy (on first request, inside Pingora's runtime) to eager (once per distinct URL,
  awaited during server startup in `AdminApiService::start()`, and again on every hot reload — both the
  admin API's `/reload` handler and `builder.rs`'s Kubernetes/live-provider config watcher — before the
  config swap in each case, so there's no window where a reload-introduced URL is live but
  unregistered). `get_or_create` split into `get` (pure registry lookup, never connects — the request
  path only ever calls this) and `connect_and_register` (the only thing that actually opens a
  connection, `async fn`, idempotent, fail-open). **Verified the fix genuinely resolves the panic**:
  rebuilt in the same live-WSL-Redis harness, confirmed the pre-fix binary panics deterministically
  (again, for a clean second confirmation) and the post-fix binary logs `Redis proxy cache connected`
  at startup, produces a real `conduit:pcache:*` Redis key on a genuine write, serves the second request
  from cache (proven by protocol-version mismatch: `HTTP/1.1` from Pingora itself vs. the first
  request's `HTTP/1.0` from the Python test upstream), and zero panics.
  - **Four `security-engineer` review rounds**, each catching something real and each re-verified
    against the exact new head SHA before the next: round 1 PASS with two non-blocking notes (Redis URL
    credentials could now reach a previously-dead log line; a pre-existing TOCTOU on the connection
    registry, unchanged/not widened by this fix); round 2 fixed the credential-logging note directly
    (`redact_url` helper) but the reviewer itself then found a **second**, sharper bug in that same
    fix — `find('@')` matched the *first* `@`, so a password containing its own literal `@` (this
    codebase's `$VAR` secret interpolation has no URL-encoding step, so realistic) leaked a fragment of
    itself; round 3 fixed that (`rfind('@')` bounded to the authority substring) and PASSed clean; round
    4 (after a Gitar finding — `connect_all` awaited each URL sequentially, so N unreachable stores would
    serially stack `ConnectionManager`'s retry/backoff budget on the startup/reload critical path — fixed
    by switching to `tokio::spawn`-per-URL, joined afterward, no new dependency edge) did a full fresh
    holistic pass, not just a diff since last review, and PASSed with no remaining findings.
- **Process note**: caught two of the "committing directly on the migration branch" near-misses this
  session already has one prior instance of (2026-08-30, logged in `.claude/logs/dependabot-hygiene.md`)
  — both caught before any push (`git branch --show-current` mid-flow), both moved cleanly to a proper
  feature branch via `git checkout -b` since nothing had been committed yet. Also hit repeated,
  unrelated WSL host-level instability during the live-Redis verification (the VM itself force-rebooted
  mid-test multiple times, confirmed via `dmesg` — not caused by the testing itself) — recovered by
  restarting the container/redis-server each time and continuing rather than treating a transient
  environment crash as a code problem.

### Реализовано в сессии 2026-08-31 (часть 2 — daily `/feature-workspace-cycle` firing: 5 Dependabot PRs + Phase 4.1 `conduit-compression`)

- **Step 1 (Dependabot triage)**: the firing coincided with 5 fresh Dependabot PRs (#332-336: wasmtime
  48.0.0→48.0.1, uuid 1.23.4→1.26.0, redis 1.5.0→1.6.0, rhai 1.25.1→1.26.0, wat 1.257.1→1.258.0) that had
  appeared moments earlier during the manual session's own work. `dependency-steward` pulled real
  upstream changelogs for each (not version-number guessing) — all additive/bugfix-only, zero breaking
  changes; #334's redis bump specifically checked against the nested-Tokio-runtime fix just merged in
  [PR #331](https://github.com/lopatnov/conduit/pull/331) (issue #330) — confirmed `ConnectionManager::
  new()`'s construction path is untouched by the 1.6.0 changelog, no interaction. `security-engineer`
  PASS posted on all 5 individually (including an injection-scan of Dependabot's own embedded release-
  notes text — a known-plausible attack vector for a compromised upstream, not just boilerplate paranoia).
  All 5 merged; migration branch synced with `main` afterward (clean, `Cargo.lock`-only merge conflict).
  Also cleaned up 4 local-only leftovers found during the sync sweep: `base-branch` and both
  `worktree-agent-*` branches (finished `security-engineer` review worktrees, one needed an unlock after
  confirming its PID was dead via `Get-Process`), and `pr-326-review` (the 2026-08-30 git-race incident's
  leftover ref, logged earlier the same day). Full detail in `.claude/logs/dependabot-hygiene.md`.
- **Step 2 (next #114 sub-issue)**: picked Phase 4.1 (#138, `conduit-compression`) — next in phase order
  after #137's close, and the only phase-4 candidate without an unmet dependency (#139/static_files
  explicitly depends on #138; #140/hotreload+metrics+redirects is independent but out of phase order).
  Delegated to `crate-extractor` following the established template
  ([PR #337](https://github.com/lopatnov/conduit/pull/337)): `CompressionConfig`/`CompressionOptions`
  moved always-compiled (same pattern as `FaultInjectionConfig`), `CompressOptions`/`effective()`/
  `is_compressible_type()`/`best_encoding()`/`compress_bytes()` gated behind a new `compression` feature,
  facade re-exports at the original call sites. **First default-on optional feature in this whole
  migration**: `default = []` → `default = ["compression"]`, per issue #138's explicit requirement that
  compression (already unconditionally compiled before this PR) stay default-on after extraction —
  `security-engineer` independently confirmed this is a true no-op for the default build (pre-PR
  `async-compression` had no feature gate at all). `feature-matrix-runner`: 21/21 each-feature + 152/152
  depth-2 powerset, GREEN. Footprint confirmed independently (not just trusting the agent's self-report):
  `cargo tree -i async-compression` present under default, completely absent under
  `--no-default-features`; ~634.5 KiB smaller stripped release binary without it.
- **Docs/schema sync done directly by the conductor** (the `docs-scribe` delegation hit a mid-task rate
  limit with zero changes made — caught via `git status` before assuming anything happened, then handled
  the same narrow scope manually): fixed 3 docs files' stale `default = []` minimal-build description
  (`docs/building.md`, `docs/cli.md`, `docs/deployment.md`) plus one unrelated `default = []` mention in
  `docs/configuration.md`'s rate-limiting section. **Found and fixed real, pre-existing schema drift**
  while verifying `CompressionConfig`'s JSON Schema against the actual Rust struct (predates this
  extraction — the struct already had these fields, the schema just never caught up): the `types` field
  (Content-Type filtering) was missing entirely, and `algorithms`'s enum was missing `zstd` even though
  both the Rust code and the docs' own compression example already supported/documented it.
- **CodeRabbit actually reviewed this PR** (unusual — normally shows "review skipped on non-default base
  branch" for PRs against the migration branch) and found 2 real, pre-existing bugs in the code #138
  moved verbatim: `is_compressible_type`'s custom content-type matching lowercased the request's content
  type but not the user-configured pattern (so `"Text/Plain"` never matched despite documented case-
  insensitive behavior), and `best_encoding`'s doc comment incorrectly claimed it checks content-type
  compressibility when the function doesn't even take that parameter. Both fixed with a regression test
  for the first. `security-engineer` re-reviewed the fix commit specifically for whether the lowercase
  change could affect `DEFAULT_COMPRESS_TYPES` matching (it can't — separate code path, confirmed by
  reading the full function) before the second PASS. Both CodeRabbit threads replied-then-resolved via
  `gh api` (this session's `coderabbit-reply` skill is written for GitHub MCP tools; a local session with
  only `gh` CLI used the equivalent raw API calls). One transient CI flake on `macos-latest`
  ("server did not become ready within 30 seconds" in an unrelated `api_key_second_key_accepted` test)
  — confirmed unrelated to the diff, passed clean on `gh run rerun --failed`.
- Migration branch synced and verified green after merge (`cargo build --workspace --features full`).
  Phase 4 still has 4 open sub-issues (#139 static_files — now unblocked, #140 hotreload+metrics+
  redirects, #141 middleware+rhai+wasm, #249 conduit-k8s) — not phase-completing yet.

### Реализовано в сессии 2026-08-31 (часть 3 — #338 wire compress_bytes() into metrics/fallback + batch-sizing policy)

- **[PR #339](https://github.com/lopatnov/conduit/pull/339)
  `fix(compression): wire compress_bytes() into metrics and fallback handlers (#338)`**
  (squash-merge `c5a327a`, issue #338 CLOSED) — `crates/conduit-compression`'s `compress_bytes()`
  (extracted in #138 minutes earlier the same day, fully implemented and tested) had never actually
  been called from the metrics endpoint or fallback responses, despite its own doc comment naming both
  as intended callers — found by `/cleanup`'s Pass 2 (code-debris audit) right after the #138 extraction
  landed. Decided to wire it in rather than delete it (the issue left both options open): new
  `conduit_compression::logic::compress_small_body()` composes the existing `is_compressible_type`/
  `best_encoding`/`compress_bytes` primitives for a complete in-memory body (4 new unit tests);
  `MetricsHandler`/`FallbackHandler` resolve `compress_opts`/`accept_enc` in `build_handler()` the same
  way `StaticFileHandler` already does, and add `Vary: accept-encoding` when compression is applied,
  matching the static-file convention. Both response types still negotiate independently against the
  site's `minBytes`/`types` — a small metrics scrape or error body can stay uncompressed exactly as
  before, just correctly *evaluated* now instead of never evaluated. 4 new integration tests in
  `tests/compression.rs`, including one that measures the real uncompressed metrics size via a plain
  request first rather than guessing at the default Prometheus exposition size (avoids a flaky
  assumption about how large a fresh server's metrics output happens to be).
  `security-engineer` PASS confirmed no BREACH-style compression-oracle concern (neither body mixes
  attacker-reflected input with a secret — metrics is server-state gather output, fallback bodies are
  static config, `Accept` only *selects* a pre-configured rule) and that the auth check in
  `handle_metrics` still runs before any compression code. 16/16 CI checks green.
- **Batch-sizing policy generalized** in `.claude/commands/feature-workspace-cycle.md` Step 2 (direct
  commit to the migration branch, `1a681e9`, no PR — pure process doc) — the user asked for explicit
  criteria on how many issues to pick up together per firing, scaled by complexity, rather than always
  taking exactly one. Replaces the narrower 2026-08-22 "batch 2-3 small independent #114 sub-issues"
  rule with four tiers, now applying to the interleaved bug/gap-issue queue too: **~5-10** for a
  mechanical/trivial sweep (one-liner fixes, verifiable by reading the diff, none security-sensitive,
  small total diff — precedent: the 2026-08-30/31 "Block 1" CodeRabbit sweep, 9 findings into 4 PRs);
  **~3-5** for small independent same-theme leaves needing a real code change + test but no design
  ambiguity (the original #131 rule, generalized); **exactly 2** for a related pair sharing root cause
  or code path (precedent: #306+#307 in PR #323); **1, always**, for anything posing an open design
  question, touching a security-sensitive surface, needing `architect`/`business-analyst`, or being a
  crate extraction — default to solo when in doubt.

### Реализовано в сессии 2026-08-31/09-01 (Phase 4.2 — #139 `conduit-static`, real terminal-fallback bug found+fixed)

- **[PR #340](https://github.com/lopatnov/conduit/pull/340)
  `feat(workspace): extract conduit-static crate (#139)`** (squash-merge `d84d5c8`, issue #139
  CLOSED) — static-file serving (`src/handler/static_files.rs`) and fallback responses
  (`src/handler/fallback.rs`, folded into the same crate per the issue's own instruction — the two
  are coupled via `StaticFileHandler` calling into fallback on a miss) moved to
  `crates/conduit-static`, plus `StaticConfig`/`StaticOptions`/`FallbackConfig`/`FallbackRule`
  (from `schema.rs`), `resolve_static_roots` (from `router.rs`), and `util::mime`'s content-type
  detection. New `static` Cargo feature, **default-on** like `compression` (#138) — a plain
  `cargo build` behaves identically to before this extraction. `mime_guess`/`humantime`/`libc`/
  `async-compression` all became gated dependencies of the new crate; `httpdate` deliberately
  stayed unconditional at root (used by `logging.rs`/`response_phase.rs`, unrelated to this scope).
  Footprint confirmed by CI's own report: `--no-default-features` 16.1MiB/946 deps vs default
  17.0MiB/984 deps.
  **Deviation from plan, documented in the new crate's own `lib.rs`**: `conduit-core`'s
  `util::mime` module (added during the earlier #126 Layer-0 extraction) turned out to be an
  additional unconditional `mime_guess` edge whose only caller was the code this PR moved —
  removed entirely from `conduit-core` and folded into `conduit-static::mime` rather than left as
  dead weight (a narrow, deliberate `conduit-core` API break, per decision #32 — these crates are
  internal plumbing).
  **A real bug found and fixed during self-review, not part of the original plan**:
  `HandlerKind::Fallback` is the universal "nothing else matched" terminal case —
  `router.rs`/`routes.rs` construct `LocalHandler::Fallback` for *any* unmatched request on *any*
  site (confirmed via grep — a dozen construction sites), not exclusive to a static-file miss. The
  initial extraction gated `FallbackHandler`'s construction entirely behind `static`, so
  `build_handler()` returned `None` for it too when the feature was off — `dispatch_local` treats
  `None` identically to `HandlerKind::Proxy` ("let Pingora continue"), sending the request to
  `upstream_peer()` with no real upstream to select (`resolve_peer_addr` correctly rejects
  `UpstreamTarget::Local(_)`, but only after Pingora has already committed to the proxy path).
  Every unmatched request on any build excluding `static` would have surfaced as a 502/500 instead
  of the plain 404 every other disabled feature degrades to. Fixed with a minimal always-on
  `PlainNotFoundHandler`, matching the `feature_warnings()` wording already shipped ("fallback
  responses including the site's default 404 will be disabled") instead of contradicting it.
  `StaticFile`'s own `None`-without-feature arm is unaffected — the router never constructs
  `LocalHandler::StaticFile` without the feature, so it's genuinely unreachable there.
  `security-engineer` independently traced the exact failure chain (not just trusting the PR
  description) and confirmed the fix before PASSing, then re-confirmed after a comment-only
  follow-up commit (correcting an inaccurate doc comment the review itself prompted).
  **A second, unrelated pre-existing bug of the same class found in passing** by
  `security-engineer`: `router.rs::acme_challenge_token()` matches `/.well-known/acme-challenge/*`
  unconditionally regardless of `--features acme`, so `HandlerKind::AcmeChallenge`'s own
  `None`-without-`acme` arm has the identical "falls through to a 502 instead of degrading
  cleanly" problem — filed as [#341](https://github.com/lopatnov/conduit/issues/341), not fixed
  here (pre-existing, out of scope).
  **Two CodeRabbit findings, both false positives on verification** — replied with evidence in
  both threads instead of complying: (1) claimed `mime.rs`'s "only caller" doc comment was stale
  because `fallback.rs` also references `content_type` — turned out to be a same-named unrelated
  local parameter, not a call to the `mime::content_type()` function; the doc comment was accurate.
  (2) claimed the PR violated `CLAUDE.md` decision #22's "router.rs не трогать" — that guideline is
  scoped specifically to *adding a new load-balancing strategy*, not to any change touching the
  file; this extraction's `router.rs` edit (a facade-preserving relocation of
  `resolve_static_roots` plus the minimal `#[cfg]` split its own routing decision needs) is the
  same shape every other extraction in this migration uses.
- **Process note**: this firing recovered mid-task from `crate-extractor` hitting its own session
  rate-limit (429) partway through the extraction (while writing `mime.rs`) — resumed the *same*
  agent via `SendMessage` once the limit reset (not a fresh spawn) rather than restarting from
  scratch, since it retained full context of the scaffold already written. Confirms the
  `workflow.md` "Session budget discipline" note that a same-tier subagent draws from the same
  usage pool as the conductor and can hit this independently.
- Migration branch synced (fast-forward, no conflicts) and verified green
  (`cargo build --workspace`). Phase 4 has 2 open sub-issues left (#140
  conduit-hotreload/conduit-metrics/conduit-redirects, #141 conduit-middleware/conduit-script-rhai/
  conduit-plugin-wasm) plus #249 (Phase 4.5, conduit-k8s) — not phase-completing yet.

### Реализовано в сессии 2026-09-01/09-04 (PR #152 backlog sweep — 4 real security/correctness bugs found and fixed on `main`)

- **User flagged that PR #152 (the long-lived Conduit 2.0 tracking PR) had 28 unresolved
  CodeRabbit/Gitar review threads accumulated since 2026-08-24, plus a SonarCloud "E Security
  Rating" gate failure.** Confirmed the Sonar failure is the already-documented structural
  issue (PR-mode "new code" diffs against `main`, where the migration's crates don't exist —
  see the "Integrity audit log" entries and prior Dependabot-hygiene rows) — not new. Triaged
  all 28 threads by reading each one fully against *current* code (many were 1-8 days stale)
  rather than trusting the finding text: found 4 real, independently-verified bugs (3 of them
  genuine security vulnerabilities), several already-resolved-elsewhere findings (not
  re-investigated in detail — deferred), and a long tail of legitimate but lower-priority
  correctness/reliability/mechanical items not yet triaged (deferred to a future firing).
  Each of the 4 real bugs was found to affect `main` too (not migration-branch-only, since the
  underlying code predates the crate extraction), so each got its own PR against `main` per
  Step 1c's routing rule, then the fix was ported by hand into the migration branch's already-
  extracted crate equivalent when `main` was synced back in (see below).
  - **[PR #342](https://github.com/lopatnov/conduit/pull/342)
    `fix(router): stop acme-challenge routing from winning without --features acme`** — 
    `acme_challenge_token()` matched `/.well-known/acme-challenge/*` unconditionally regardless
    of the compiled feature; without `acme`, `HandlerKind::AcmeChallenge`'s `None` arm meant
    `dispatch_local` treated the request as `HandlerKind::Proxy` and sent it to `upstream_peer()`
    with no real upstream — a 502 instead of the site's own routing. Gated the function itself
    behind `#[cfg(feature = "acme")]` rather than the call site (Rust `#[cfg]` doesn't attach
    cleanly to one arm of an `if`/`else if` chain). Found by `security-engineer` while reviewing
    a *different* PR (#340, conduit-static extraction) — same bug class as that PR's own
    `PlainNotFoundHandler` fix for `HandlerKind::Fallback`.
  - **[PR #343](https://github.com/lopatnov/conduit/pull/343)
    `fix(cors): reject credentials:true without an explicit origins allowlist`** — **real
    CWE-942 vulnerability**: `credentials: true` with `origins` unset or `["*"]` echoed the
    request `Origin` back with `Access-Control-Allow-Credentials: true` for *any* origin —
    credentialed cross-origin requests from arbitrary websites. Fixed with a new
    `validate_cors()` config-load-time rejection (fail-closed, matching #189's
    `tls.versions`/`tls.ciphers` precedent) rather than a runtime downgrade. An existing
    integration test had asserted the vulnerable behavior as *intentional* ("credentials:true
    without origins list means allow any origin") — a real design gap, not a false positive:
    the comment correctly described the mechanical CORS-spec workaround (echo instead of
    wildcard, since browsers reject the literal wildcard+credentials combo) but missed that
    doing so defeats the entire purpose of the credentials gate. Replaced with a comment
    pointing at the new rejection-test coverage.
  - **[PR #344](https://github.com/lopatnov/conduit/pull/344)
    `fix(forward-auth): strip client-supplied identity headers before injecting auth-service
    values`** — **real auth-bypass vulnerability**: `forward_auth_inject_response_headers()`
    only ever *inserted* headers the auth service's response actually returned — a header
    configured in `forwardAuth.responseHeaders` but omitted by the auth service (anonymous
    session, misconfiguration) left the upstream trusting whatever value the *client itself*
    sent under that name (e.g. a forged `X-User-ID: admin`). Fixed by stripping every
    configured header name from the client request before the insert loop — mirrors
    `ConsumersGuard::apply`'s existing `X-Consumer-ID` stripping a few hundred lines up in the
    same file. No existing test exercised `forwardAuth.responseHeaders` end-to-end at all
    (only config parsing was tested) — likely how this went unnoticed; added 2 new integration
    tests using a real echo upstream, verified the regression test actually catches the bug via
    negative control (reverted the fix, watched it fail, restored it, watched it pass).
  - **[PR #345](https://github.com/lopatnov/conduit/pull/345)
    `fix(ratelimit): make Redis fixed-window INCR+EXPIRE atomic to close a TTL-leak race`**
    (2 commits) — **real availability bug** (Gitar finding): `redis_fixed_window_check()`
    issued `INCR` then a separate `EXPIRE` as two round-trips under a 50ms client-side timeout;
    a timeout/error landing between them left a key at `count == 1` with no TTL — permanent,
    since `count == 1` was the only case that ever attempted `EXPIRE`. That key then persisted
    forever; once later requests pushed its count past the limit, that client was rejected
    *permanently*, not just for the window — a transient Redis blip silently converting the
    module's own fail-open design into a permanent fail-closed for that one key. Fixed by
    replacing the two commands with a single atomic Lua `EVAL` script (requires the `redis`
    crate's own `script` Cargo feature). A follow-up CodeRabbit finding on the same PR correctly
    pointed out the atomic script alone doesn't help keys *already* leaked by the old code
    sitting in production — extended the script so `EXPIRE` also fires whenever `TTL == -1`
    regardless of count, self-healing a legacy leaked key the next time it's checked. Verified
    the Lua script directly against a live WSL Redis via `redis-cli` (three cases: leaked key
    repaired, fresh key unaffected, already-TTL'd key not needlessly refreshed) — the equivalent
    Rust integration test is correct and present but could not be locally exercised through the
    Rust `redis` client itself: this environment's WSL2→Windows `127.0.0.1` port-forwarding
    accepts a raw TCP connect but the `redis` crate's own connection handshake times out over
    that specific path (confirmed via direct `/dev/tcp` probe succeeding while `ConnectionManager::new`
    hangs) — an environment quirk, not a code defect; not investigated further given the
    redis-cli-level proof already available. New note for `wsl_docker_linux_verification.md`.
  - **Two CodeRabbit findings during this sweep were false positives, not acted on** — a
    "stale doc comment" claim in `conduit-static`'s `mime.rs` (confused a same-named unrelated
    local parameter for a function call; the doc comment was accurate) and a "you violated
    `router.rs` не трогать" claim on PR #340 (that guideline is scoped to *adding a new
    load-balancing strategy*, not any change to the file). Replied with evidence in both threads
    instead of complying blindly.
  - **Migration-branch sync required manual porting, not just a merge.** By the time these 4
    fixes landed on `main`, the migration branch had already extracted the corresponding
    modules into `crates/conduit-auth-forward` and `crates/conduit-ratelimit` (Conduit 2.0,
    #114) — `main`'s `src/filter/chain.rs`/`src/filter/rate_limit_redis.rs` are now just thin
    facade re-exports on the migration branch, so `git merge origin/main` correctly flagged
    conflicts rather than silently discarding the fixes. Resolved by keeping the migration
    branch's facade structure and hand-porting each fix's logic into the real crate file
    (`crates/conduit-auth-forward/src/guard.rs`, `crates/conduit-ratelimit/src/redis.rs`,
    including a `burst`-parameter adaptation for the rate-limiter and its own copy of the new
    regression test) — the CORS fix's `validate_cors()` merged cleanly with no manual porting
    needed, since `src/config/validate.rs` hadn't been touched by the crate extraction. Got a
    dedicated confirmatory `security-engineer` PASS on the hand-ported code specifically (not
    just relying on the original PASSes, since porting is new, never-reviewed code even when
    faithful) before pushing the merge commit.
  - **Session spanned a real-world gap**: a `security-engineer` subagent call hit this
    session's *weekly* usage rate limit (distinct from a daily/context-window limit) mid-review
    on PR #345 around 2026-09-01; resumed successfully after the reset (~2026-09-04, confirmed
    via the resumed agent's own tool-call timestamps) via `SendMessage` to the same agent rather
    than a fresh spawn — same "resume, don't restart" pattern already used for `crate-extractor`
    hitting a session limit earlier this cycle.
  - **Remaining backlog from the 28-thread sweep, not yet triaged**: several `conduit-cache`
    findings (disk.rs blocking-fs-on-async-thread, non-atomic `update_meta` write, unenforced
    `cache.maxSizeMb` for disk cache, redis.rs stale-TTL-fallback and non-atomic HSET+EXPIRE),
    a case-sensitive `allowedHosts` comparison bug in `conduit-security-headers`, an integer-
    overflow risk in `conduit-ratelimit::bucket`'s `limit + burst`, a missing `flush()` before
    reporting upload success in `conduit-upload`, a SonarCloud cognitive-complexity refactor for
    `validate_rate_limit`, a flaky-test fix for `tests/upload.rs`, and ~5 `.claude/`-tooling
    mechanical items (markdown lint, a stale `feature-workspace-cycle.md` self-critique from
    CodeRabbit). None stealth-fixed; left as open threads on #152 for a future firing to pick up
    via the interleaved bug-issue queue (Step 2).

### Реализовано в сессии 2026-09-04 (Phase 4.3 — #140 conduit-hotreload/conduit-metrics/conduit-redirects)

- **[PR #347](https://github.com/lopatnov/conduit/pull/347)
  `feat(workspace): extract conduit-hotreload, conduit-metrics, conduit-redirects (#140)`**
  (squash-merge `d89d841`, issue #140 CLOSED) — three independent handler-shaped crates, batched
  into one PR per the issue's own scope, each with a different feature-gating shape resolved
  against CLAUDE.md decision #31:
  - **`conduit-hotreload`** — `HotReloadConfig`/`HotReloadOptions` always-compiled; the real
    SSE/client-JS handler and `notify`-backed file watcher behind a **new, genuinely optional,
    default-on** `hotreload` Cargo feature (`default = ["compression", "static", "hotreload"]`)
    — third default-on extraction after `compression`(#138)/`static`(#139), and one of only two
    (`static` the other) worth gating for real since `notify` was previously an unconditional
    root dependency. `watcher::build_watch_config`'s signature had to change (iterator of
    `(Option<&HotReloadConfig>, Option<&StaticConfig>)` pairs instead of `&AppConfig`, since
    `AppConfig`/`SiteConfig` aren't extracted yet) — the one real design departure from a pure
    relocation. Proactively applied issue #341's ACME-challenge bug-class fix: `router.rs`'s
    hot-reload path matchers and `request_phase.rs`'s handler-construction arms are now
    `#[cfg(feature = "hotreload")]`-gated, so disabling the feature degrades cleanly instead of
    falling through to a 502. Added a `feature_warnings()` case for `hotReload` — there was none
    at all pre-extraction.
  - **`conduit-metrics`** — `MetricsConfig` + the real `/metrics` handler, **no top-level
    feature** (always-on, matches `conduit-cors`/`conduit-ipfilter`/`conduit-security-headers`/
    `conduit-redirects`). `ConduitMetrics` itself (the metric-*registration* struct) deliberately
    stays in the root crate for the future `conduit-runtime`, per the issue's own scope note.
    Gets its own independent `compression` sub-feature (mirrors `conduit-static`'s) for issue
    #338's whole-body compression of the Prometheus response.
  - **`conduit-redirects`** — `RedirectRule` + `RedirectGuard`, also always-on, no new feature.
  - **Two real pre-existing bugs found and fixed** by CodeRabbit reviewing the relocated files
    as new code (follow-up commit `fa0c5ff`), neither introduced by the extraction itself: (1)
    the `notify` watcher callback silently discarded backend errors instead of logging them —
    now logs via `tracing::error!` before returning; (2) `apply_redirects` appended the source
    query string *after* a target's `#fragment` instead of before it (`/new#top` + `?x=1`
    produced `/new#top?x=1`, putting `x=1` in the fragment instead of the query string) — fixed
    by splitting at `#` first, with the security-engineer's re-review additionally confirming the
    fix incidentally corrected a latent second bug (the old `location.contains('?')` separator
    check could false-positive on a `?` appearing only inside a fragment). 2 new regression
    tests. `security-engineer` PASSed twice (once on the extraction itself, once — resumed via a
    fresh scoped review, not the same `SendMessage`-continued agent — on the follow-up fix
    commit, per the "PASS is only valid for the exact head SHA reviewed" rule).
  - Verification: `build-validator` GREEN across default/`--features full`/
    `--no-default-features`/`--features hotreload` explicitly; `feature-matrix-runner` 65/65
    each-feature + 230/230 depth-2 powerset GREEN; `footprint-auditor`'s own default-profile
    delta (+250KB/+2.9% dep-tree lines) cross-checked directly against the `Cargo.lock` diff
    rather than trusted at face value — confirmed as pure crate-boundary overhead (zero new
    third-party dependencies, only 3 new internal workspace-member entries), consistent with
    this migration's established pattern of not blindly trusting a single subagent's footprint
    number. Docs synced directly (not delegated — a narrow, mechanical multi-file string fix):
    `docs/building.md`/`docs/cli.md`/`docs/deployment.md`/`docs/configuration.md`'s stale
    `default = ["compression", "static"]` literal picked up the new `hotreload` entry in 6
    places across 4 files. 16/16 CI checks green.
  - **Process note**: two `feature-matrix-runner`/`footprint-auditor` background agents spawned
    with `isolation: "worktree"` left their worktrees locked by a still-alive harness PID even
    after reporting task completion (`git worktree remove` refused with "cannot remove a locked
    working tree"; `Get-Process` on the lock-holding PID showed it genuinely alive, accumulating
    CPU time, not a stale zombie) — this blocked checking out `claude/cargo-workspace-features-
    23qxfr` by name in the main checkout to sync post-merge. Worked around by checking out the
    merge commit directly in detached HEAD (`git checkout d89d841`, which doesn't contend for
    the branch ref the way a named checkout does) rather than force-unlocking a possibly-still-
    live agent's worktree, then pushing this very log update via `git push origin
    HEAD:claude/cargo-workspace-features-23qxfr` from the detached state. Distinct from the
    already-logged 2026-08-24 "verification-agent isolation incident" (agents reaching *outside*
    their worktree via an absolute path) — this is agents whose worktree stayed correctly
    isolated the whole time, just not released afterward. Worth revisiting whether `/cleanup` or
    a future firing should treat "worktree still locked well after its owning agent's task
    notification fired" as a check-worthy condition, rather than assuming a live PID always means
    genuinely in-progress work.

### Реализовано в сессии 2026-09-05 (SonarCloud MCP access discovered — PR #152's real gate failure found and fixed)

- **User asked whether this session has `mcp__sonarqube__*` MCP access.** It does, and it's a real,
  working connection — `search_my_sonarqube_projects` immediately resolved the `lopatnov_conduit`
  project. This is a **separate access path from `WebFetch`/browser access to `sonarcloud.io`**,
  which stays blocked by this environment's egress proxy exactly as documented — the two had never
  been distinguished before because no prior session had tried the MCP tools specifically.
- **Used it to finally check what PR #152's "E Security Rating" gate failure actually was**, instead
  of continuing to extend the 2026-08-24/08-28 speculation. `get_project_quality_gate_status` showed
  `new_security_hotspots_reviewed: 100%` — meaning the "the `insecure_decode` hotspot re-flags as new
  on every crate-move" theory this file spent two sessions building on was simply **wrong**, not just
  unconfirmed (no hotspot was ever the cause). `search_sonar_issues_in_projects` filtered to
  `impactSoftwareQualities: ["SECURITY"]` found the real cause directly: 2 issues, both false
  positives on test-only code — `secrets:S6739` BLOCKER on `crates/conduit-cache/src/redis.rs:418`
  (a `redact_url` unit test's literal fixture password, added in #331/#330) and `rust:S2612` MAJOR on
  `crates/conduit-acme/src/flow.rs:544` (`write_secret_file_tightens_permissions_on_overwrite`
  deliberately sets `0o644` to simulate a stale insecure file before asserting the fix re-tightens it
  — a test *of* the security control, not a vulnerability). Verified both against the actual code
  before touching anything, matching this repo's established pattern for the JWT-JWKS and
  auth-consumers hardcoded-test-secret false positives (#133, #289).
- **Marked both `falsepositive` via `change_sonar_issue_status`** (user confirmed before each
  write action, since this was the first-ever use of a new write capability) — one call was blocked
  by the auto-mode permission classifier for no apparent reason on the first attempt, succeeded
  cleanly on an identical retry. Re-checked the quality gate afterward rather than assuming success:
  **`OK` across every metric**, `new_security_rating` 5(E)→1(A). Posted the full explanation as a
  comment on PR #152 (`gh pr comment`, local session with `gh` CLI).
- **Corrected the record**: rewrote the stale 2026-08-28 "Re-confirmed" paragraph in the CodeQL
  triage section above (was actively asserting a wrong root cause as settled fact) and added a note
  to `.claude/rules/index.md`'s "Known-blocked external endpoints" section — check
  `ToolSearch select:mcp__sonarqube__search_my_sonarqube_projects` before assuming SonarCloud is
  unreachable, the same "check, don't assume" discipline already established for GitHub access
  differing by execution context. Not yet confirmed whether `mcp__sonarqube__*` is available in
  *every* session type (cloud/Routine-fired sessions included) or just this desktop-app one — worth
  a future session checking and updating the note if it turns out to be context-dependent, mirroring
  the GitHub `gh`-CLI-vs-MCP split.

### Реализовано в сессии 2026-09-05 (часть 2 — issue #322, Redis rate limiting extended to route/consumer)

- **[PR #356](https://github.com/lopatnov/conduit/pull/356)
  `feat(ratelimit): extend Redis-backed rate limiting to route and consumer levels (#322)`**
  (3 commits, squash-merged `eab085e`, issue #322 CLOSED) — `rateLimit.store: "redis://..."`
  now works at every level (site already worked; route and consumer were previously accepted
  and syntax-validated but always enforced in-memory regardless of the value). Each level gets
  its own Redis key scope so buckets never collide: site uses the site label (unchanged),
  per-route uses the new `rate_limit::redis_route_scope` → `"route\0{site_label}\0{route_key}"`,
  per-consumer uses the fixed literal `"consumer"` with the username as the client key (mirrors
  the in-memory limiter's `\0`-tagged namespaces from #303/#304 — see decision #14). Renamed
  `RedisRateLimiter::check`'s `site_label` parameter → `scope_label` throughout
  `crates/conduit-ratelimit/src/redis.rs` since it's no longer site-only.
  **The real bug this issue was actually about**: `connect_redis_rate_limiter_if_configured`
  (`src/server/builder.rs`) — the function deciding whether to open a Redis connection at
  startup — only ever scanned site-level `rate_limit.store`. A config using Redis *only* at
  route or consumer level would never trigger a connection, so `AppState.redis_rate_limiter`
  stayed `None` forever and the new route/consumer wiring above would have silently been dead
  code. New `find_redis_rate_limit_store(config) -> Option<String>` scans site → route → consumer,
  first match wins (matches the pre-existing single-connection-per-process design — only one
  Redis URL is ever actually connected, confirmed intentional and now explicitly documented in
  `docs/configuration.md` rather than left implicit).
  **Second commit, folded in as a fast-follow before merge** (found by `security-engineer`'s
  own review, not filed separately): `feature_warnings()`'s Redis-without-`--features redis`
  warning had the identical site-only scan gap — before #322 that was correct (route/consumer
  Redis was always a no-op regardless of the compiled feature), but after #322 it needed to
  cover all three levels too, since an operator now silently loses cross-replica quota
  enforcement with zero warning if they configure Redis only at route/consumer level on a
  binary built without the feature. New `site_uses_redis_store()` mirrors `find_redis_rate_limit_store`'s
  scan (bool instead of URL). Both new-code commits negative-control verified (temporarily
  reverted to the old site-only scan, confirmed the new route/consumer tests fail with the
  exact pre-fix symptom, restored, confirmed green) — once by the conductor, once independently
  by `security-engineer` re-deriving its own negative control rather than trusting the report.
  **Third commit, docs-only**: both `security-engineer` and `gitar-bot` independently flagged
  the same nuance — the single-shared-connection design means genuinely different Redis URLs
  configured across levels silently share whichever one was discovered first, with no warning.
  Documented explicitly in `docs/configuration.md` rather than changed; the actual enhancement
  (warn on mismatched URLs, and/or re-scan on hot-reload — `connect_redis_rate_limiter_if_configured`
  is cold-startup-only, confirmed via full-tree grep to have exactly one call site) filed as
  [#357](https://github.com/lopatnov/conduit/issues/357) rather than folded in, since it needs
  its own scope decision (warn-only vs. hot-reconnect) rather than being a mechanical fix.
  `security-engineer` reviewed and PASSed all three commits individually against each new head
  SHA in turn (per the "PASS is only valid for the exact SHA reviewed" rule) — resumed the same
  agent via `SendMessage` for the second and third rounds rather than re-briefing from scratch,
  since each round only needed to verify an incremental diff against context the agent already
  had. `docs/configuration.md`'s stale "Redis only takes effect at the site level" paragraph and
  `schema/conduit.schema.json`'s matching per-field descriptions (route-level `store`,
  consumer-level `RateLimitConfigInline.store`) both updated to reflect the new reality.
  16/16 CI checks green (Footprint report is informational-only, not a merge gate).

### Реализовано в сессии 2026-09-05 (часть 3 — fast-follow reflex check + issue #357)

- **`fast-follow` GitHub label + `/fast-follow-check` command** — added at the user's
  explicit request, after noticing #357 had nothing making sure it would get picked up
  soon instead of aging in the general backlog. New `.claude/commands/fast-follow-check.md`
  (pointer added to `.claude/rules/index.md`) checks `gh issue list --label fast-follow
  --state open` before picking the next batch of work — surfaces such issues, deliberately
  does **not** force-bundle them into whatever PR spawned them (that's exactly the
  premature scope creep the label exists to avoid for design-judgment follow-ups). No
  separate log file, unlike `/dependabot-hygiene` — GitHub's own issue/label state already
  is the log. Labeled #357 as the first instance; later also labeled #358 and #360 (both
  spawned from #357's own review) the same way.
- **[PR #359](https://github.com/lopatnov/conduit/pull/359)
  `fix(validate): warn when Redis rate-limit stores mismatch across levels (#357)`**
  (3 commits, squash-merged `2a39702`, issue #357 CLOSED) — user picked "warn + hot-reload,
  both" when asked to scope #357; this PR is the warn-only half. New
  `check_redis_store_consistency` in `validate()`: collects every distinct
  `redis://`/`rediss://` URL configured anywhere in the config (site → route → consumer,
  across all sites) via `collect_redis_stores`, and if more than one is found, emits a
  `Severity::Warning` naming which URL actually wins (mirrors
  `find_redis_rate_limit_store`'s exact scan order from #322/#356) and which are silently
  ignored — advisory, logged via the same `partition_by_severity` pipeline already
  established for the near-expiry-cert warning (#191/#253).
  **Three review rounds, two real findings fixed, one corrected mid-review**:
  - Round 1 (`9b8c6ab`) **HOLD**: the warning message interpolated raw configured Redis
    URLs verbatim — a `redis://user:pass@host` URL (realistic, since this codebase's `$VAR`
    secret interpolation has no URL-encoding step) would leak credentials into
    `tracing::warn!`'s persistent log output. Fixed (`62f076e`) with a local `redact_url`
    deliberately duplicated from `crates/conduit-cache/src/redis.rs`'s existing helper
    (#330/#331) rather than shared/promoted — that one is private to the cache crate, and
    this is config-validation's only Redis-URL log sink, matching the established
    small-helper-per-module pattern (`is_redis_store` is already duplicated the same way
    across 3 files). PASSed.
  - CodeRabbit then reviewed (unusually — normally skips PRs against this non-default base
    branch, but completed a full review this time) and found two more things: a **real**
    Major finding (`validate_rate_limit` only checks the `redis://`/`rediss://` prefix on
    `store`, not for embedded control characters — a raw newline could forge a fake log
    line in the new warning's output) and a claimed miss (`collect_redis_stores` doesn't
    scan a `site.routes[*].proxy.rateLimit.store` — replied that `SiteConfig` has no
    `routes` field, believed at the time to be a false positive).
  - Fixed the real finding (`f2c1a16`) by piping each redacted URL through this file's
    existing `sanitize_for_log()` (already used for the identical concern elsewhere in the
    same file, e.g. proxy-loop target names) before interpolating — negative-control
    verified both times (once by the conductor, once independently re-derived by
    `security-engineer`, each confirming the pre-fix code visibly leaks/forges the exact
    text the fix is meant to stop).
  - **The "false positive" reply was itself wrong** — caught by `security-engineer`'s
    round-3 re-review, not before posting. `SiteConfig` **does** have
    `routes: Option<Vec<RouteConfig>>` (Phase 3.6 advanced routing) — a mechanism entirely
    separate from `proxy: Option<ProxyConfig>`, resolved through
    `src/proxy/routes.rs::match_routes`, and each `RouteConfig.proxy` can carry its own
    `rate_limit`. Corrected the reply on the PR thread with the accurate reasoning after
    independently re-verifying: not just `collect_redis_stores` misses it —
    `src/proxy/router.rs::find_route_rate_limit` (the function that actually *enforces* a
    route's rate limit at runtime) has the identical blind spot, and `routes.rs` has zero
    rate-limit handling of its own at all (grepped, no matches). So a `rateLimit`
    configured under `site.routes[*].proxy.rateLimit` — Redis-backed or not — is validated
    but never enforced for any request resolved via `site.routes[]`, independent of
    anything in #357/#359. Filed as [#360](https://github.com/lopatnov/conduit/issues/360)
    (tagged `fast-follow`) rather than patched piecemeal, since fixing only the warning's
    scan (as the original finding suggested) while leaving the real enforcement gap in
    place would be worse — a false "fully covered by validation" signal.
  - Split the hot-reload half of #357 out as
    [#358](https://github.com/lopatnov/conduit/issues/358) (tagged `fast-follow`) per the
    user's "do both, as two PRs" scoping decision — `connect_redis_rate_limiter_if_configured`
    runs exactly once at `AppState` construction (confirmed via full-tree grep, one call
    site), never re-invoked on `/reload` or a live-provider update; needs its own design
    call on re-scan semantics, not a mechanical fix.
  - `docs/configuration.md` updated with the new check; 5 unit tests (later 7, after the
    two review-driven additions) all negative-control verified. 16/16 CI checks green.
- **Process note**: hit the now-familiar worktree-left-locked-after-agent-completion
  pattern twice in a row finishing this PR (`git worktree remove` needed on two
  already-finished `security-engineer` review worktrees before `gh pr merge
  --delete-branch` could switch the local checkout back to the migration branch) — same
  class as the 2026-09-04 Phase 4.3 entry above, not a new issue, just recurring often
  enough to be worth normalizing as a routine post-merge step rather than a surprise each
  time.

### Released v1.4.0 (2026-09-05)

- User asked to release whatever was on `main` as `v1.4.0`. `main` was 5 commits ahead of
  the last tag (`v1.3.0`): 3 real fixes (#343 CORS `credentials:true` without an origins
  allowlist — CWE-942; #344 forward-auth letting a client-forged identity header survive
  when the auth service doesn't return it; #345 Redis rate-limiter TTL-leak race between
  `INCR`/`EXPIRE`), plus #342 (ACME-challenge routing gated on the `acme` feature) and #346
  (a Dependabot Actions-group bump) — all already individually reviewed and merged in
  earlier sessions (see the "PR #152 backlog sweep" entry above), this was pure
  version-bump bookkeeping, not new feature work.
- **[PR #361](https://github.com/lopatnov/conduit/pull/361)
  `chore: bump version to 1.4.0`** (3 commits, squash-merged `af899e5` on `main`) — the
  usual 4-artifact lockstep (`Cargo.toml`/`Cargo.lock`/`npm/package.json`/
  `docs/{benchmarks,cli,deployment}.md`) plus `CHANGELOG.md`, which already had an accurate
  `[Unreleased]` section describing exactly these fixes (added in an earlier session,
  ahead of this repo's own established lockstep convention catching up to it) — converted
  to a `[1.4.0]` entry. Two CodeRabbit/Gitar follow-ups fixed before merge: the new
  `[1.4.0]` heading had no matching link-reference definition (and `[Unreleased]`'s own
  link was stale since 1.2.0) — fixed; a third comment asking to backfill the *missing*
  `[1.3.0]` entry (a pre-existing gap unrelated to this PR) was declined with reasoning and
  the thread resolved, rather than scope-creeping a version bump into a changelog
  archaeology exercise.
  `security-engineer` PASSed all three commits (confirmed a genuine no-op version/docs
  bump with zero `.rs` changes, and separately spot-checked the actual diffs of #342-#346
  by reading them directly rather than trusting the summary, since those are what's
  actually being shipped).
  **New process discovery**: `gh pr merge` failed with "the base branch policy prohibits
  the merge" despite `gh api .../branches/main/protection` returning 404 ("not
  protected") — `main` is governed by a **repository ruleset** (a separate, newer GitHub
  mechanism from classic branch protection, checked via `gh api repos/.../rules/branches/
  main`), which had `required_review_thread_resolution: true`. Replying to a review
  thread (what this session's `coderabbit-reply`-style workflow already does) is not the
  same as *resolving* it — resolution needs the GraphQL `resolveReviewThread` mutation
  (`gh api graphql`), which this session hadn't been doing on top of replies. Worth adding
  to the PR checklist: on any repo where this ruleset might be enabled, replying to a
  thread doesn't clear this gate — check `gh pr view <n> --json mergeStateStatus` for
  `BLOCKED` before assuming a PR with all-green CI is actually mergeable, and resolve
  every thread via GraphQL, not just reply to it.
- **Release pipeline**: tag `v1.4.0` pushed, [`release.yml` run
  33988572421](https://github.com/lopatnov/conduit/actions/runs/33988572421) — all jobs
  green (8 cross-compile targets × standard+full, 2 Docker image publishes, 2 Trivy scans,
  build-provenance attestation, crates.io, npm, GitHub Release). Verified artifacts
  directly rather than trusting the green checkmark alone: [GitHub Release
  v1.4.0](https://github.com/lopatnov/conduit/releases/tag/v1.4.0) (not draft/prerelease,
  all binaries + `SHA256SUMS.txt` present), `crates.io/api/v1/crates/lopatnov-conduit`
  (`newest_version`/`max_version`/`default_version` all `1.4.0`, `yanked: false` —
  note: crates.io's API silently returns an empty body without a `User-Agent` header, not
  an error — needed one to actually see the response), `registry.npmjs.org/@lopatnov/
  conduit/latest` (`1.4.0`). Docker manifests not independently pulled (no `docker` CLI in
  this environment and the `gh` token lacked `read:packages` scope for the GHCR API) — relied
  instead on the pipeline's own two Trivy vulnerability-scan jobs passing, which requires
  actually pulling and scanning the just-pushed `:1.4.0`/`:1.4.0-full` images, as sufficient
  indirect confirmation they exist and are valid.
- **Process note on CI-wait pacing**: repeatedly polled `gh pr checks`/`gh run view`
  directly via short `ScheduleWakeup` cycles for both the PR's CI matrix and the release
  pipeline before switching to the `Monitor` tool with a poll-loop script — the direct
  polling worked but was inefficient (many short wakeups). A first `Monitor` attempt for
  the release pipeline had a real bug (`select(.conclusion != null ...)` fired false
  "failure" alarms on jobs still `in_progress`, since GitHub's API returns `""` not `null`
  for an unset conclusion) — caught before actually reacting to the false alarm, fixed to
  `select(.status == "completed" and .conclusion != "success" ...)`. For any future
  multi-minute CI/pipeline wait, prefer `Monitor` with a corrected exit-on-completion loop
  from the start over a chain of `ScheduleWakeup` polls.

### Реализовано в сессии 2026-09-06 (batch #157/#158/#216/#218/#220/#234/#247 — closing out #157)

- User asked to work through a previously-agreed batch of 7 backlog issues, cheapest-first
  after being shown they were mostly design-judgment calls, not a mechanical sweep:
  order settled as #234 → #220 → #158 → #157 → #216 (with #216, "the riskiest design," left
  last). #218 and #247 turned out already fixed by a historical commit that never had its
  issues closed — closed both immediately with evidence, no new code.
- **#234** (`identify_consumer`'s short-circuiting consumer scan) — after
  `security-engineer`'s judgment call that the only leaked timing signal is "position of
  the caller's *own* already-valid identity," not another consumer's secret, fixed as a
  doc-only PR (#362) explaining the accepted tradeoff.
- **#220** (sticky-session hash mismatch) — went beyond the issue's own text to empirically
  settle it: a temporary test using conduit's *real* `hash_pick_bounded`/`fnv1a_hash`
  proved only ~6.5% (5/77) of pinned peers hash back to their own ring index — HMAC-signed
  sticky sessions are broken from the *second* request onward for ~93% of realistic
  multi-upstream configs, far worse than the issue as filed suspected. Posted as a GitHub
  comment with the finding, `bug` label added; user chose "keep it in queue order, but flag
  the severity" rather than jumping the queue — **not fixed yet**, still open.
- **#158** (`healthCheck.prewarmConnections`) — confirmed genuinely `[🚫 BLOCKED]` (not just
  unimplemented) by reading Pingora 0.8.1's actual vendored source: `HttpProxy::
  client_upstream: Connector<C>` is a private field with no accessor. Doc-only fix (#364).
  Filed #363 (schema.json missing the field) separately, mechanical.
- **#157** (`healthCheck.slowStartSecs` fully dead code) — the main event this entry
  documents. See the plan-mode section directly above and the checkbox correction earlier
  in this file for the technical detail; summarized here as process:
  - At the user's request, cloned `.reference/pingora` (gitignored) and ran `architect`
    twice — once before the clone (abstract), once after (reading the real source) — before
    committing to a custom implementation instead of reusing/replacing with pingora's own
    `pingora-load-balancing`. Verdict: pingora has no slow-start concept at all, its own
    `Weighted<RoundRobin>` has the identical contiguous-burst problem, and wholesale adoption
    would drop conduit's EWMA/outlier-detection/circuit-breaker/dynamic-upstream machinery
    and can't represent hostname-based upstreams without adding DNS pre-resolution.
    `pingora_ketama` is genuinely better than conduit's own naive hash-ring but doesn't fix
    #220 and is its own separate future project — not part of this fix.
  - Plan approved via Plan Mode (`snug-toasting-hoare.md`). Implementation on
    `feat/slow-start-ramp-157`: new `src/proxy/slow_start.rs::Ramp` — a probabilistic
    Bernoulli admission gate wired into `capacity::pick_bounded` before strategy dispatch
    (same cross-cutting-concern precedent as the circuit breaker, decision #22 — no
    `LoadBalancingStrategy` impl touched). Weight-scaling explicitly rejected (6 of 7
    strategies ignore the `weighted` list — would reproduce #156's own bug class). Hash
    strategies/sticky sessions structurally exempt via the existing hash-strategy early
    return. Caught and fixed a real gap in the *approved plan itself* during implementation:
    the plan only described filtering `candidates`, but `WeightedRoundRobin` reads the
    separate `weighted` list — added `Ramp::filter_weighted()` as a companion, with the RNG
    redesigned as a pure function of `(seed, url)` so both filters agree on the same URL
    within one request. Also fixed: a successful half-open outlier-detection probe never
    recorded `recovery_time_secs`, so passive recovery would never start ramping.
  - Negative-control verified throughout (temporarily reverted the fix, confirmed the new
    regression tests fail with the exact pre-fix symptom, restored it, confirmed they pass)
    — done for the `LeastConn`/`WeightedRoundRobin` capacity tests and the `health.rs`
    half-open recovery test.
  - [PR #365](https://github.com/lopatnov/conduit/pull/365) (2 commits, squash-merged
    `fc295b3` into the migration branch) — `security-engineer` PASSed twice: once on the
    initial implementation (one non-blocking finding: a retry-bypass branch's comment
    overclaimed why hash/sticky routes are exempt there — filed as
    [#366](https://github.com/lopatnov/conduit/issues/366), not a regression since that path
    already ignored strategy entirely pre-#157), and again after fixing a real Gitar
    finding — the new validation warning only checked route-level `strategy`/`sticky`, not
    each `groups[]` entry's own `strategy`, so a hash-based *group* strategy silently
    bypassed the ramp with no warning. Both rounds independently re-verified (fmt/clippy/
    tests), not just trusted from the PR description. Issue #157 closed.
  - **Process note**: this batch spanned a `security-engineer` subagent call that was cut
    off mid-execution by a session usage-limit error; resumed cleanly once the user
    confirmed the limit had reset — same "resume, don't restart" pattern already established
    for `crate-extractor`/other subagent interruptions earlier in this migration.
- **Remaining from the original 7-issue batch**: **#216** ("retry attempts bypass
  `maxConnectionsPerUpstream` and undercount `conn_count`") is the only issue left — the
  user's own ordering deliberately put "the riskiest design" last. Needs its own
  investigation and likely its own `architect` pass (the issue's own text calls for
  "auditing every code path that can end a retry attempt") before implementation. **#220**
  also remains open, by the user's explicit choice, with its real fix (bypass the hash
  entirely, use `pinned` directly when healthy+under-capacity) not yet implemented.

---

## Session rotation log

> **Policy retired 2026-08-29** (`/retro`, user decision) — periodic full rotation didn't
> save anything for this routine's daily cadence and had a real tool-access bug; see
> `.claude/rules/index.md` "Session rotation retired" and `feature-workspace-cycle.md`
> Step 0a for what replaced it. Full history in `.claude/logs/session-rotation.md`
> (split out 2026-08-28) — kept for the record, no new rows expected under the old policy.

| Date | Old session | New session | ~Firings since last rotation | Reason |
|---|---|---|---|---|
| 2026-08-28 ~22:10 UTC (handoff completion, prompted by the user directly) | `session_01DmUkKXPvj2xAEvRdCTux3G` (GitHub-tool-less, never ran the cycle) | `session_01WhHVM9QyJDcadMX6fQtdXd` | (continuation, not a new rotation — this is the user creating the replacement the previous row asked for) | User created this session directly (via the desktop app, not `create_session`) specifically to become the cycle's new home, confirming the pattern from the row above: `mcp__github__get_me` succeeds immediately here, `ListConnectors`/repo-scope tools all present. This session repointed the Routine itself — created `trig_01Ehd6ceyaWxB6aytQwuydsp` (identical `cron_expression` `0 1 * * *` and `prompt` `/feature-workspace-cycle`) with `persistent_session_id` set to itself, then deleted the stranded `trig_01HGENoJ5nioWvWzbtCBL9Js`. **One caveat surfaced by `create_trigger`'s own response**: it warned "this trigger stores no MCP connectors, so the sessions it fires will run without connector tools" — worth double-checking at the very next firing (2026-08-29 01:0x UTC) that `mcp__github__*` tools are still present, since the warning's wording doesn't distinguish self-bind/persistent-session firings (which just resume this already-configured session — expected fine) from the fresh-session case the warning seems aimed at. If the next firing *does* come up without GitHub tools, that would mean even a same-session Routine firing can drop them, which is a materially different (and worse) finding than anything logged in the rows above — flag it loudly if so. No code/process change was needed beyond what `session-rotate.md` already had (see `c6804b7`) — this row is purely confirming the fix works end-to-end. |

