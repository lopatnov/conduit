# Conduit — Claude's reference

> Высокопроизводительный реверс-прокси на Rust · Cloudflare Pingora · `main` = v1.5.0 · ветка миграции Conduit 2.0 (#114) = `2.0.0`
> Проект: `<projects-root>\conduit`

---

## Источники для сверки (`.reference/<name>`)

Реальные исходники зависимостей и референсных прокси клонируются **по мере необходимости** в `.reference/<name>` (в `.gitignore`), по возможности на тот же tag, что в `Cargo.lock` (`git clone --depth 1 --branch <tag>`); `/cleanup` их **не** трогает — это накопительный кэш. Таблица (что где даёт, какие уже клонированы) — `.claude/archive/reference-sources.md`. Перед доверием к changelog/памяти — читать источник; для новых фич — шаг «Research before building» (`rules/workflow.md`), включая Pingap и River.

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

Не пересматривать без явного обсуждения. **Если решение всё же пересмотрено и его статус
поменялся** (не просто уточнён факт под ним, а сам вывод — например "не входит в Conduit"
→ "вопрос открыт") — **менять саму headline-строку решения**, не только дописывать заметку
ниже неё. Дописанная-снизу заметка при неизменённой headline создаёт видимое противоречие
(поймано CodeRabbit на decision #28, 2026-09-12: заметка ниже уже говорила "статус открыт",
а сама строка #28 всё ещё звучала как окончательное "не входит, отдельный проект").

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
14. **Rate limiter** — `DashMap` v6 (`AppState.rate_limiter`, общий для site/route/consumer). Ключи строятся **только** через `rate_limit::{site_key,route_key,consumer_key}` (`\0`-разделители): `site\0{site_label}\0{client}`, `route\0{site_label}\0{route_key}\0{client}`, `consumer\0{username}` (квота consumer'а **намеренно** глобальна по сайтам). `GET /rate-limits` разбирает все три формы. Redis-бэкенд — `crates/conduit-ratelimit/src/redis.rs` (за фичей `redis`; ключи тоже `\0`-разделены, не `:` — #350); на процесс одно Redis-соединение (первый найденный URL, #357). Admission — через `conduit_ratelimit::check_key_for` (#305). История находок и форматов — `.claude/archive/decisions-history.md`.
15. **Graceful shutdown** — `Arc<AtomicUsize>` inflight. SIGTERM → перестать принимать → ждать нуля → exit. Факт (исправлено #489, 2026-10-10): `global.shutdownTimeoutSecs` (по умолчанию 30) идёт в `ServerConf.grace_period_seconds`; При SIGTERM Pingora спит весь период безусловно, не завершается раньше при нулевом inflight и не смотрит на `AppState.inflight`; `POST /shutdown` (и `conduit shutdown`) сам опрашивает inflight и выходит при нуле или по дедлайну. `global.backlog` — no-op (Pingora фиксирует 65535), validate предупреждает (#490).
16. **`FallbackConfig`:** нет поля `redirect`.
17. **LoadBalanceStrategy** — 8 вариантов (включая P2c). Веса статические. Для IpHash/CH — `hash_key: "ip" | "header:X-Key" | "url"`. P2C: splitmix64 RNG, O(1).
18. **Динамические upstream'ы** — только в памяти. `UpstreamRegistry` отдельно от конфига. `conduit reload` сбрасывает overrides.
19. **Upstream groups** — `groups` + `groupStrategy`. Phase 3.7b.
20. **Filter Chain (CoR)** — `crates/conduit-runtime/src/filter/chain.rs` (с #145; `src/filter/chain.rs` в корне — фасад). Новый guard = `impl RequestFilter` + push в chain. `service.rs` не трогать. `phase: "response"` scripts пропускаются в request-фазе (`MiddlewareGuard::apply`, теперь в `crates/conduit-middleware/src/guard.rs` — issue #114/#141; сборка chain'а — там же, в `filter/chain.rs` крейта `conduit-runtime`).
20a. **Feature warnings** — `config::validate::feature_warnings()`. WASM (без `--features wasm`) + OTLP (без `--features otlp`) → `tracing::warn!` при старте и hot-reload. `/reload` response включает поле `warnings: [...]`.
21. **Handler Registry** — трейт `LocalHandlerImpl` в `src/handler/mod.rs`. 7 handler structs реализованы. `dispatch_local` → `build_handler()` + `handle()`.
22. **Routing Strategy** — трейт `LoadBalancingStrategy` в `src/proxy/strategy.rs`. Новая стратегия = новый struct + `from_config()` arm. `router.rs` не трогать.
23. **CLI Commands** — трейт `CliCommand` в `crates/conduit-cli/src/lib.rs` (с #147; `src/cli/mod.rs` в корне — фасад). Новая команда = struct + arm в `dispatch_command()` (`crates/conduit-cli/src/dispatch.rs`, вынесен из `main.rs` тем же #147). `main()` не трогать.
24. **YAML конфиг** — `serde_yaml`, `from_yaml()` в `parse.rs`, автопоиск `conduit.yaml/yml`.
25. **Provider pattern** — `Provider` trait в `crates/conduit-server/src/config/provider.rs` (с #147; `src/config/provider.rs` в корне — фасад). `FileProvider` (one-shot + auto-reload). `KubernetesProvider` (feature = "kubernetes") в `crates/conduit-server/src/config/kubernetes.rs` (тот же #147; `src/config/kubernetes.rs` — фасад).
26. **WASM middleware** — `type: "wasm"`, фича `wasm`, wasmtime, fail-open; `crates/conduit-plugin-wasm/src/wasm.rs`. Плагин экспортирует `on_request() -> i32` (опционально `on_response(status) -> i32`); 17 host-функций в request-фазе, +7 в response-фазе (20 различных имён); память — экспорт `"memory"`, если плагин вызывает любую host-функцию, читающую/пишущую в неё (иначе молчаливая деградация, #381). Подробности — `.claude/archive/decisions-history.md`.
27. **MiddlewareGuard** — объединяет Rhai ("script") и WASM ("wasm") в `crates/conduit-middleware/src/guard.rs`
    (issue #114/#141, было `src/filter/chain.rs` до извлечения крейта; сборка chain'а сама по себе
    остаётся в `src/filter/chain.rs` — правило "Filter Chain" ниже про это). Порядок entries
    соблюдается. `ScriptGuard` = type alias для совместимости.
28. **CGI** — вопрос «внутри Conduit или отдельный проект» **открыт** (пересмотрено 2026-09-12; было «не входит»). Реализацию не начинать: #290/#291 заблокированы до выбора транспорта, политики RAM-admission и streaming-дизайна. Разбор трёх вариантов — в телах issues и `.claude/archive/decisions-history.md`.
29. **Тесты** — port 0, rcgen, serial_test для Admin API, mock = `TcpListener` без Axum.
30. **`RequestCtx` per-request state (#114)** — поля живут в крейте, которому принадлежит структура (`conduit-runtime` с #145); **без** type-erased слота и **без** trait'а в `conduit-core`; feature-поля — `#[cfg(feature = "x")]` на самой структуре (zero-cost на hot path). Пересматривать только если экстракция #133/#134/#135 окажется реально болезненной (не наступило). Решение владельца 2026-08-21, подтверждено 2026-08-23 и 2026-09-26. Обоснование — `.claude/archive/decisions-history.md`.
31. **Feature-гейты (ipFilter/cors/securityHeaders/compression/static/fallback/hotReload/metrics/redirects)** — гибрид: отдельные крейты ради организации кода, но реально опциональными (`default` расширен) делаются только тяжёлые — `static`/`hotReload`, возможно `compression`; `ipFilter`/`cors`/`securityHeaders`/`redirects`/`metrics` остаются always-on (гейтинг лёгкой логики почти не даёт выигрыша, а цена «забыл флаг — тихо не работает» реальна). `metrics`: посылка про `prometheus` устарела с Pingora 0.9, вывод нет (выигрыш 2–3 крейта, <1%, `#[cfg]` на hot path); пересматривать только ради бюджета размера (#451, #516). Решение владельца 2026-08-23.

32. **Публикация member-крейтов (#114)** — все `lopatnov-conduit-<name>` публикуются на crates.io в lockstep (иначе не опубликовать бинарник), но **как internal plumbing**: без semver-гарантий, `pub`-поверхность чистится по мере находок (как `path_matches`, PR #230). Полноценный публичный API отложен, не отклонён — трекается #258 (предложение по уровням, 2026-10-03). Решение владельца 2026-08-23.

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
  ResponseFilterChain (crates/conduit-runtime/src/filter/response_chain.rs; в корне — фасад):
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

## Беклог

Живой беклог — **GitHub Project #5** (`@lopatnov/conduit`) и issues. Всё выполненное и заметки исследований (h2o, Angie, freenginx, Tokio, ранее проверенные «блокировки») — в `.claude/archive/backlog.md`. Ниже только то, что открыто или заблокировано (на 2026-10-03); причины блокировок и перепроверка на Pingora 0.9 — #451.

### Открыто
- Middleware Stack
- Полностью feature-driven архитектура с CoR-сборкой по компиляции и именованными бандлами (V2; фактически идёт как #114)
- Graceful upstream drain
- External processing filter (ext_proc)
- Lua скрипты
- `responseThrottle.bytesPerSec`
- TLS 0-RTT Early-Data replay protection (RFC 8470)
- `gracefulShutdownTimeoutMs`
- PROXY Protocol v1/v2 поддержка
- Docker/container service discovery
- Dynamic DNS re-resolution
- Upstream connection drop on removal
- Persistent cache index
- MQTT preread для TCP proxy
- Configurable custom metrics zones
- Ignore unexpected 1xx responses from upstream
- `limits.minUploadRateBytesPerSec`
- Leaky bucket алгоритм для rate limiting ответа (#34)
- Multipath TCP (MPTCP)

### Заблокировано (Pingora и др.)
- Request queue + backpressure (#58, #451)
- OCSP stapling config (#59, #451)
- `tls.versions`/`tls.ciphers` enforcement (#189, #450)
- X-Reproxy-URL internal redirect (#60, #451)
- Upstream H1/H2 protocol ratio selector (#63, #451)
- Happy Eyeballs RFC 8305
- Encrypted Client Hello (ECH)
- Upstream HTTP/3 (QUIC) (#95)
- H2/QUIC flood detection

---

## Журналы аудитов и гигиены

Integrity-аудит (Step 1c цикла) и Dependabot/branch hygiene ведутся в `.claude/logs/integrity-audit.md` и `.claude/logs/dependabot-hygiene.md` (новая строка — сначала туда). Прежние inline-таблицы — в `.claude/archive/backlog.md`.

---

## Правила

- `pingora-cache = "0.9"` — кастомный cache key обязателен (CVE-2026-2836). С 0.9 у `CacheKey::new` нет `namespace`: хост вшивается в primary через `\0` (`build_cache_key` в `crates/conduit-cache/src/cache.rs`), хэши отличаются от 0.8 — персистентный кэш (disk/redis) после апгрейда холодный
- Pingora `"0.9"` — только 0.8+ (3 CVE исправлено в 0.8; в 0.9 ушли `protobuf 2.28.0` и `daemonize`)
- Pingora 0.9: `RequestHeader`/`ResponseHeader` без `DerefMut` — заголовки менять только через `insert_header`/`append_header`/`remove_header`, не через `.headers.*`
- `schema/conduit.schema.json` — вручную синхронизировать с реальными `Config`-структурами (после миграции #114 они разбросаны по крейтам, не в одном `schema.rs`). Валидировать: `node -e "JSON.parse(fs.readFileSync('schema/conduit.schema.json','utf8'))"`. С issue #496 (PR закрывающий #496) — `scripts/check_schema_superset.py` (CI job `schema-superset-check`) best-effort ловит дрейф для structs, у которых есть именованный `$defs`-эквивалент; untagged-энумы, `#[serde(flatten)]` и инлайновые (без своего `$defs`) блоки вне его охвата — см. докстринг скрипта.
- HTTP/3 (Phase 5) — ждём Pingora Issue #95, ~август 2026
- `src/main.rs` тонкий: CLI → `dispatch_command()` → command struct → `execute()`
- `tls.versions`/`tls.ciphers` — **не работают, отклоняются на validate()** (issue #189,
  2026-08-29). Pingora (0.8 и 0.9 — проверено по исходнику) rustls `TlsSettings` не даёт API для
  ограничения версий/шифров (подробности и перепроверка на 0.9 — `.claude/archive/backlog.md`).
- Admin API bind — по умолчанию loopback (`127.0.0.1:2019`); не-loopback bind (например `0.0.0.0:2019`) допустим,
  но только с `global.admin.token` и за VPN/SSH-туннелем (`docs/admin.md`, «Security»). Решение владельца 2026-09-27:
  правило переписано, а не превращено в отказ на валидации — старая формулировка «только loopback» не соответствовала коду
  (`validate()` любой bind пропускает, схема сама показывает `0.0.0.0:2019` с токеном). Валидация, что не-loopback bind
  без токена — предупреждение, **не реализована** (при желании — отдельный issue). Связанное: пустой токен — ошибка
  валидации (#480), lint forwardAuth→Admin API учитывает bind-хост (#470).
- `hotReload` при `static` как IndexMap — следить за ВСЕМИ директориями
- `routes` backward-compatible с top-level `proxy`/`static`
- tracing spans в hot path — только `Level::TRACE`
- Бинарник ≤15 МБ — это `--no-default-features`, stripped (замер 2026-10-03: 12,84 MiB; пол — минимальный Pingora-прокси
  с тем же профилем, 7,64 MiB; разбор и план — #516). `default`/`standard`/`full` — по Footprint-отчёту PR, без роста без причины
- `WeightedRoundRobin` валидация: targets — `WeightedTarget`, не строки
- Docs: `docs/configuration.md`, `docs/deployment.md`, `docs/benchmarks.md`
- YAML: `.yaml`/`.yml` через `from_yaml()`, env interpolation + version check работают так же
- Filter Chain: `crates/conduit-runtime/src/filter/chain.rs` (фасад `src/filter/chain.rs`) — добавлять новые guard-фильтры ТОЛЬКО сюда
- Response Chain: `crates/conduit-runtime/src/filter/response_chain.rs` (фасад `src/filter/response_chain.rs`) — добавлять новые response-фазы ТОЛЬКО сюда
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

---

## Журнал сессий

> Полная запись пишется **сразу** в конец `.claude/logs/session-log.md`; здесь остаются выжимки (≤ 3 КБ каждая) двух
> последних записей — старую выжимку просто удаляй (полный текст уже в логе). Всё ещё открытое — комментарием на issue
> (Step 8 цикла), а не прозой здесь. **Каждая запись кончается строкой «Здоровье»** (владелец, 2026-10-03):
> `python scripts/health.py --line`, размер `--no-default-features` из Footprint-отчёта PR и одно честное предложение
> на вопрос «выбрал бы я этот проект для другого начала?» (подробности — Step 8 цикла).

### Реализовано в сессии 2026-09-27 (часть 6 — #147/#492: `conduit-server` + `conduit-cli`; builder.rs разбит)

- **PR #493** (squash `8c67287`; #147, #492 закрыты вручную). Извлечены `crates/conduit-server` (`run_server()`, supervisor Admin API, `POST /reload`, валидация, file/Kubernetes providers) и `crates/conduit-cli` (dispatch и все команды) по плану `architect`, поправившему текст issue (`clap` всё равно тянет `pingora-core`; `builder.rs` не переносится в одиночку). Порядок admin → server → cli → root; 10 фич зеркалятся на `conduit-server` (10 compile-time assert'ов в корне), `kubernetes` — на `conduit-cli`; 16 assert'ов `validate` остались в корне (`cfg!()` крейт-относителен). Багфиксы отдельными коммитами: фоновые задачи Admin API останавливаются на shutdown; конфиг из Kubernetes CRD валидируется при старте и на каждом live-обновлении (#492). 272 теста переехали под теми же именами.
- **PR #495** (squash `970fe1d`): `server/builder.rs` (431 строка кода) разбит на `listeners.rs`, `config_watch.rs`, `acme_certs.rs`, `redis_bootstrap.rs` — чистое перемещение, `run_server` остаётся единственным внешним символом; `cargo hack --each-feature` на `conduit-server` чисто.
- Находки: #494 (K8s live-update не вызывает `detect_cold_changes`), Redis-логи печатают сырой `redis://` URL. `cargo hack` на время работы мутирует все `Cargo.toml` — ждать завершения, не коммитить; GitHub не закрывает issue при merge в не-default ветку.
- Полный текст — `.claude/logs/session-log.md`.

### Реализовано в сессии 2026-10-03 (часть 7 — /retro: диета инструкций, RAG, пять приоритетов, #516 admin-фича; PR не открыты)

- **Измерено:** production Rust +22% (16,4 → 20,0 тыс. строк), ×2 выросли файлы, документы и `.claude/` (298 → 820 КБ); в сессию грузилось 207 КБ инструкций. 12,84 MiB `--no-default-features`: пол — минимальный Pingora 7,64 MiB; наши крейты 1,1 MiB `.text`, конфиг всех фич ~0,7, CLI ~0,5, Admin API ~0,3; сжатие Pingora 0.9 (~0,7) безусловно (#516).
- **Диета (#512):** авто-загружаемое 207 → **49 КБ**; `CLAUDE.md` 125,7 → 28,6 КБ; история — в `.claude/archive/` (не `docs/`: она английская), ничего не удалено (проверено построчно). Находка: `AGENTS.md`/`.agents/`/`.codex/` — устаревшие копии старых инструкций (v1.1.0), решение за владельцем.
- **Правила:** 5 приоритетов (+ код-практики, RFC); «Found while here» в PR; «Research before building»; строка «Здоровье» в журнале (`scripts/health.py`); `crate-extractor` → `crate-steward` + skill `new-feature-crate` (#259).
- **RAG (#513):** qdrant + LM Studio запущены владельцем; `scripts/rag/rag.py` (docs 1 414 точек за 333 с, issues, code, `.reference`).
- **#516, срез 1:** Admin API — Cargo-фича `admin` (default-on); из `--no-default-features` уходят 15 крейтов (axum, hyper, tower…); `default`/`full` без изменений; warning + golden `[!admin]` + 11-й parity-assert.
- **Ждёт владельца:** push/PR (две ветки), пилот для #258 (рекомендую `conduit-ratelimit`), остальные срезы #516, `AGENTS.md`.
- **Здоровье:** 20 042 строки, 32 крейта, инструкции 49 КБ (ok), `--no-default-features` 12,84 MiB. Выбрал бы Conduit для другого проекта? **Нет, пока** (ACME не продлевается #491, no-op поля #489/#490, 84k не воспроизводится, RFC не измерено #514; зато ниша «платишь за скомпилированное» настоящая) — что сделать: #514, #516, #487.
- Полный текст — `.claude/logs/session-log.md`.
