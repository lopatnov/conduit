# Беклог и прочие заметки — архив (перенесено из CLAUDE.md, #512)

Живой беклог — GitHub Project #5 и issues. Здесь: исходные два раздела беклога (выполненное `[x]`, открытое, заблокированное с причинами),
результаты исследований h2o/Angie/freenginx, заметки про Tokio, прежние inline-журналы аудита и гигиены, устаревшая таблица «блокировок».

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
  **#216 closed 2026-09-06** — turned out to be a real leak, not just an undercount; see the
  2026-09-06 entries in `.claude/logs/session-log.md` for the full 4-PR fix (#367, #368, #216 parts 1 and 2).
  **#217 and #218 were both already closed separately** (2026-08-22 and prior, respectively)
  before this session picked up #216 — verified via `gh issue view`, not assumed.
- [x] **Forward Auth** — `forwardAuth: { url, requestHeaders?, responseHeaders?, timeoutMs?, skipPaths? }`. `ForwardAuthGuard` (6d в chain). Subrequest через `reqwest::Client` singleton. 2xx=allow+inject headers, 4xx/5xx=deny, unreachable=fail closed. 5 integration tests.
- [x] **Service Failover** — `ProxyRouteConfig.backup`. Когда все primary unhealthy → route to backup. Логика в `resolve_proxy()`.
- [x] **Inflight request limit** — `LimitsConfig.maxInflightRequests`. `LimitsGuard` проверяет `inflight` перед прочими лимитами. 503 при превышении.
- [x] **Traffic Mirroring** — `proxy.*.mirror: String`. Fire-and-forget через `tokio::spawn` + reqwest. V1: headers only (тело не буферируется). Заголовок `X-Mirrored-From` добавляется. `UpstreamTarget::Proxy.mirror_url`. `fire_mirror_request()` в service.rs.

#### Средний приоритет

- [🚫 BLOCKED] **Request queue + backpressure** — когда upstream на maxconn: ставить в очередь (не сразу 503). Priority queue по классу + timestamp. HAProxy: `queue.c`.
  **Причина:** `ProxyHttp` trait не имеет хука "upstream перегружен, подожди" — Pingora не предоставляет механизма задержки принятия соединения до освобождения upstream slot. Circuit Breaker (`maxConnectionsPerUpstream`) покрывает основной кейс. Pingora 0.9 вышел (2026-09-09), но пункт по его исходнику **не перепроверялся** — #58, #451.
- [x] **Upstream slow start** — `UpstreamEntry.recovery_time_secs` + `slow_start_fraction()` в health.rs. `UpstreamHealthCheck.slowStartSecs` config field.
  **Уточнение 2026-09-06 (issue #157)**: чекбокс был отмечен преждевременно — сам механизм
  (`slow_start_fraction`) существовал, но нигде не вызывался за пределами собственных тестов;
  конфиг `slowStartSecs` был полным no-op. Реально подключено фиксом #157 — см. запись 2026-09-06 в
  `.claude/logs/session-log.md`. `src/proxy/slow_start.rs::Ramp` — probabilistic Bernoulli admission gate внутри
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
  **Причина:** Pingora rustls backend не имеет публичного API для управления OCSP stapling. Rustls обрабатывает его внутренне без конфигурации. Pingora 0.9 вышел, по его исходнику **не перепроверялось** — #59, #451.
- [🚫 BLOCKED] **`tls.versions`/`tls.ciphers` enforcement** (issue #189, найдено
  `integrity-auditor` при Step 1c аудите `src/server/tls.rs`) — поля парсятся с самого первого
  коммита (`58ec267`), но никогда не были подключены: `make_tls_settings()` не принимает
  versions/ciphers параметр, `TlsPortMap` не имеет для них места, `detect_cold_changes()` их
  тоже не проверял — полный silent no-op с 2026-мая. **Причина:** подтверждено через vendored
  `pingora-core-0.8.1/src/listeners/tls/rustls/mod.rs:62-63` — `TlsSettings::build()` жёстко
  зашивает `ServerConfig::builder_with_protocol_versions(&[TLS12, TLS13])` без cipher-suite
  API, все поля `TlsSettings` приватные, единственный конструктор `intermediate()` берёт
  только cert/key path, `add_tls_with_settings()` принимает исключительно `TlsSettings` —
  никакого хука для кастомного `ServerConfig`/`Acceptor`. **Перепроверено на 0.9.0
  (2026-09-25, security-review PR #450) — блокировка остаётся:** `TlsSettings::build()` по-прежнему
  зашивает `[TLS12, TLS13]`; `Acceptor::from_server_config` в 0.9 есть, но listener принимает только
  `TlsSettings` (`listeners/mod.rs:411`, поле `tls` — `pub(crate)`), так что кастомный acceptor
  подключить нельзя. (Запись 2026-09-12 в журнале называла `from_server_config` рабочим обходом —
  это было неверно.) `set_cert_resolver` (0.9) даёт только выбор сертификата, не версий/шифров.
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

- [x] **WASM plugin system** — `type: "wasm"` вместе с Rhai (не вместо). Wasmtime, `--features wasm`. 17 host-функций (read/modify headers, set response, get_uri, get_header_names, abort_with_redirect, get_request_id). Module cache, fail-open. `src/filter/wasm.rs`, 38 unit-тестов inline WAT (выросло с исходных 15 по мере добавления response-фазы и отдельных host-функций, включая 3 новых из этого же аудита — trap в `on_response`, отказ `memory.grow` за пределами 16 MiB кэпа, и отказ инстанцирования при превышении кэпа изначально заявленной памятью; число поправлено 2026-09-07, Step 1c аудит).

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
  **Все три закрыты 2026-08-17** — см. запись в `.claude/logs/session-log.md`
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

- [ ] **3. CLI: минимальный набор Cargo-фич для конфигурации** ([#473](https://github.com/lopatnov/conduit/issues/473),
  задача владельца 2026-09-26) — команда читает конфиг и печатает только те фичи, без которых он не запустится
  (без лишних и без подразумеваемых другими), готовую строку `cargo build --features …` и `--json`; код возврата ≠ 0,
  если текущий бинарник собран без нужной фичи. Отвечать должна из тех же предикатов, что `feature_warnings()`
  (`COMPILED`/`feature_warning` в `warnings.rs` каждого крейта) — не третья таблица вручную. Приёмка: на каждом файле
  `examples/` набор достаточен (предупреждений нет) и минимален (без любой одной фичи предупреждение появляется).

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

- [x] **Certificate rotation** — `POST /certs/reload`. Принимает `{ cert, key }` PEM, валидирует пару через rustls (cert/key match), записывает атомарно в `tls.cert`/`tls.key` файлы. После — `conduit reload` или рестарт процесса активирует новый серт. `validate_cert_key_pem()` в `server/tls.rs`. 5 unit-тестов + 4 integration-теста. **Zero-downtime hot-swap: в Pingora 0.9.0 хук есть** (`TlsSettings::set_cert_resolver`, `listeners/tls/rustls/mod.rs:127`), Conduit его пока **не подключает** — трекается в #451 вместе с мультисертификатным SNI (тот же хук).

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
  вместо более мягкого "doesn't yet". **Перепроверено на 0.9.0 (2026-09-25):** `client_upstream`
  по-прежнему приватное поле без аксессора — блокировка остаётся.

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
  **Причина:** требует mid-request смены upstream в Pingora — нет публичного API. Pingora 0.9 вышел, по его исходнику **не перепроверялось** — #60, #451.
  Источник: `h2o/lib/handler/reproxy.c`.

- [🚫 BLOCKED] **Upstream H1/H2 protocol ratio selector** — дефицитный RR алгоритм для
  выбора протокола (H1/H2) по конфигурируемым процентам.
  **Причина:** требует управления ALPN на уровне Pingora — не экспонировано. Pingora 0.9 вышел, по его исходнику **не перепроверялось** — #63, #451.
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
| 2026-09-07 | `src/filter/wasm.rs` (WASM plugin middleware, unchanged since it shipped 2026-06-05, never audited before) | 10 findings: 7 low-risk/unambiguous + 3 needing design judgment | Filed [#379](https://github.com/lopatnov/conduit/issues/379) (high severity — `on_response` body override is completely non-functional and leaks two internal headers to the client), [#380](https://github.com/lopatnov/conduit/issues/380) (`conduit_get_header_names`'s "insertion order" doc claim is unreachable, sourced from a `HashMap`), [#381](https://github.com/lopatnov/conduit/issues/381) (missing `"memory"` export degrades silently with zero warning log). 7 low-risk docs/schema fixes + 3 new resource-limit tests shipped via [PR #382](https://github.com/lopatnov/conduit/pull/382) (off `main`) — see `.claude/logs/integrity-audit.md` for the full findings list, the 6 real gitar/CodeRabbit findings on the PR's own docs prose (all fixed), and a genuine HOLD on the 3rd review round: one of those 6 "fixes" was itself factually wrong (a wasmtime memory-limit claim taken from CodeRabbit's own unverified web-doc summary instead of the vendored source), caught and corrected before merge. |

## Dependabot & branch hygiene log

> Append-only — **full history moved to `.claude/logs/dependabot-hygiene.md`** (split out
> 2026-08-28, same rationale as above). See `.claude/commands/dependabot-hygiene.md` (was
> `.claude/rules/index.md` "Dependabot & branch hygiene reflex check") — any session that
> touches this repo's GitHub state runs this cheap sweep if the newest row in the full log
> file is older than ~24h, then logs a row there (even "nothing new"). Only the newest
> row(s) stay inline below.

| Date/time (UTC) | New Dependabot PRs found/acted on | Orphan branches flagged | Notes |
|---|---|---|---|
| 2026-09-20 ~17:10 (ad hoc, run by `/retro`; `main` still frozen) | 1 open: #422 (pingora major) — HELD, not merged | 0 new (24 remote heads; the ~21 old remote-only branches flagged 2026-09-18 untouched) | The full log was stale by its own newest row (2026-09-13) because the 09-18 row had been written only here — backfilled; `dependabot-hygiene.md` now says to write the full log first. Full detail in `.claude/logs/dependabot-hygiene.md`. |
| 2026-09-26 (ad hoc, while working #450/#314; `main` still frozen) | 5 new against `main` (#452 clap, #453 rustls, #454 clap_complete, #455 jsonwebtoken, #456 async-compression — all patch/minor), all HELD under the freeze; #422 (pingora) closed as superseded by #450 | 0 new; 29 remote heads (24 + 5 Dependabot branches), the ~21 old remote-only branches untouched | Written from the full log row first. Full detail in `.claude/logs/dependabot-hygiene.md`. |

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

### ⚠️ ИСПРАВЛЕНИЕ: предыдущие данные о блокировках были ОШИБОЧНЫ

Проверка исходников `<projects-root>\pingora` (v0.8.0) показала что 3 из 4 задач РЕАЛИЗУЕМЫ:

| Задача | Старый статус | Реальный статус (проверено в pingora src) |
|--------|---------------|------------------------------------------|
| **mTLS** | 🚫 BLOCKED | ✅ **РЕАЛИЗУЕМО** — `TlsSettings::set_client_cert_verifier(Arc<dyn ClientCertVerifier>)` в `pingora-core/src/listeners/tls/rustls/mod.rs:97`. `WebPkiClientVerifier` экспортируется. |
| **Stale-while-revalidate** | 🚫 BLOCKED | ✅ **РЕАЛИЗУЕМО** — `CachePhase::Stale` + `StaleUpdating` в `pingora-cache/src/lib.rs:85-87`. Хук `should_serve_stale()` в `proxy_trait.rs:621`. |
| **Request body buffering** | 🚫 BLOCKED | ✅ **РЕАЛИЗУЕМО** — `request_body_filter(session, body: &mut Option<Bytes>, end_of_stream, ctx)` в `proxy_trait.rs:132`. |
| **OCSP stapling** | 🚫 BLOCKED | ❌ Действительно заблокировано — `// TODO` в pingora source, нет публичного API. |

---
