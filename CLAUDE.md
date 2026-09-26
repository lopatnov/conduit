# Conduit — Claude's reference

> Высокопроизводительный реверс-прокси на Rust · Cloudflare Pingora · `main` = v1.5.0 · ветка миграции Conduit 2.0 (#114) = `2.0.0`
> Проект: `<projects-root>\conduit`

---

## Локальные репозитории источников — читать перед доверием к changelog/памяти

> **Изменено 2026-09-12**: старые копии на `<projects-root>\` (`C:\projects\...`) пропали
> при переустановке ОС и не будут восстановлены как отдельные top-level клоны. Новый дом —
> **`.reference/<name>` внутри самого репозитория conduit** (gitignored — см. `/.reference/`
> в `.gitignore`, тот же паттерн уже использовался разово для `.reference/pingora` в сессии
> 2026-09-06 при расследовании #157). Не все проекты из таблицы ниже присутствуют
> одновременно — клонируются **по мере необходимости** во время работы над конкретной
> задачей (`git clone --depth 1 --branch <tag>`, по возможности на тот же tag/версию, что
> реально запинена в `Cargo.lock` — так правки в исходнике будут соответствовать
> реальному поведению зависимости, а не произвольной более новой/старой версии).
> **`/cleanup` не должен трогать `.reference/`** — это не разовый scratch для одной
> проверки, а накопительный кэш источников, которым сессии пользуются повторно; см. также
> `.claude/rules/index.md`. По состоянию на 2026-09-13 уже склонированы: `pingora` (tag
> `0.9.0` — совпадает с `Cargo.lock` после апгрейда с 0.8.1 на ветке миграции 2026-09-20), `tokio`
> (tag `tokio-1.53.1`, совпадает с `Cargo.lock`), `dashmap` (tag `v6.2.1`), `axum` (tag
> `axum-v0.8.9`), `kube` (tag `4.2.0`), `k8s-openapi` (tag `v0.28.0`), `rhai` (tag `v1.26.0`),
> `wasmtime` (tag `v48.0.1`, без submodules — `--no-recurse-submodules`, ~118 MB даже так,
> самый крупный клон в `.reference/`), `arc-swap` (tag `v1.9.1` — Cargo.lock пинит `1.9.2`,
> но на GitHub нет такого тега, используем последний доступный `v1.9.1`) — последние семь добавлены
> по прямому запросу пользователя ("странно что не скачиваешь то, что мы используем") ровно на версии,
> реально запиненные в `Cargo.lock` на момент клонирования.

### Rust (прямо применимо к Conduit)
| `.reference/<name>` | Что даёт |
|------|---------|
| `pingora` | КРИТИЧНО. ProxyHttp, TlsSettings, CachePhase, все хуки. Conduit запинен на 0.9 (`Cargo.toml`, `Cargo.lock` = 0.9.0; апгрейд с 0.8.1 сделан на ветке миграции 2026-09-20) — см. `.claude/logs/session-log.md` (записи 2026-09-12 и 2026-09-20) за находки по факту чтения исходника (не changelog), включая реально unblocked backlog-пункты, которые ещё не подключены |
| `tokio` | Async runtime, spawn, channels |
| `dashmap` | Concurrent hashmap (`DashMap<String, TokenBucket>` в rate limiter, `UpstreamRegistry` в health.rs, connection tracking). Запинен на `"6"`, реально `6.2.1` |
| `axum` | Admin API (порт 2019), upload loopback-сервис, hot-reload SSE-эндпоинт. Запинен на `"0.8"` (`Cargo.toml`) — актуально для `{param}` vs `:param` route-синтаксиса (0.7→0.8 breaking change, см. issue #352's ACME-сервер баг) |
| `arc-swap` | Atomic read-copy-update контейнер для `AppState.config: Arc<ArcSwap<AppConfig>>` — безлокновая горячая переконфигурация через `POST /reload`, см. decision #12 ("Всё остальное — hot через ArcSwap"). Запинен на `"1"`, реально `1.9.2` в Cargo.lock (но на GitHub только tag `v1.9.1`) |
| `kube` | `KubernetesProvider` (`--features kubernetes`), CRD `ConduitSite`. Запинен на `"4.0"`, реально `4.2.0` |
| `k8s-openapi` | Типы K8s API объектов для `kube`. Запинен на `"0.28"`, feature `v1_32` |
| `rhai` | Rhai-скриптинг для `type: "script"` middleware (`ScriptGuard`, `on_response` фаза). Запинен на `"1"` с `features = ["sync"]`, реально `1.26.0` |
| `wasmtime` | WASM plugin middleware (`type: "wasm"`) engine source. Клонировать на tag, совпадающий с `Cargo.lock`'s `wasmtime` (менялся, см. Dependabot-лог) — сейчас `48.0.1` |
| `tower` | Service/middleware traits (наш FilterChain построен похоже) — не прямая зависимость conduit, только транзитивная через `axum`; референс для дизайна, не для версийной сверки |
| `http` | HeaderMap, Request/Response типы |
| `reqwest` | HTTP client (mirror, forwardauth, JWKS) |
| `linkerd2-proxy` | **Rust proxy** — `linkerd/http/retry/src/replay.rs` = ReplayBody (body buffering для retry) |
| `azure-sdk-for-rust` | Azure SDK — `azure_identity` (Managed Identity), `azure_security_keyvault` (Key Vault). Источник для `--features azure` |

### Proxy/gateway (паттерны и идеи)
| `.reference/<name>` | Язык | Что даёт |
|------|------|---------|
| `nginx` | C | mTLS, upstream TLS, buffering |
| `angie` | C | nginx fork (российский, активно развивается) — HTTP/3, ACME, статистика |
| `freenginx` | C | nginx fork от Igor Sysoev — community-driven, минималистичный |
| `h2o` | C | HTTP/2 server — mruby scripting, aggressive H2 optimizations, QUIC/H3 |
| `traefik` | Go | mTLS `ClientAuth`, middleware chain, OTLP |
| `envoy` | C++ | CircuitBreaker `resource_manager.h`, queue |
| `haproxy` | C | `src/queue.c` — request queue + backpressure |
| `apisix` | Lua/Go | Consumer model, 12-phase response pipeline |
| `oathkeeper` | Go | Authenticator→Authorizer→Mutator (наш ForwardAuth) |
| `caddy` | Go | Auto-TLS, Let's Encrypt patterns |
| `squid` | C | Cache patterns |
| `unit` | C | nginx Unit, модульная архитектура |

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
    как #137 slice 2) — отдельный ключевой неймспейс: `"conduit:rl:{scope_label}\0{window_secs}\0
    {client_key}"` для реального Redis, `"{scope_label}\0{client_key}\0{limit}\0{burst}\0
    {window_secs}"` для его in-process fallback-мапы (оба `\0`-разделены, не `:`-разделены — фикс
    2026-09-07, issue #350: `scope_label`/`client_key` могут легитимно содержать двоеточие —
    IPv6-хост без скобок в site_label, IPv6 client_key, произвольное значение `keyBy:
    "header:X-Name"` — что при `:`-разделителе давало реально воспроизводимую коллизию двух разных
    (scope, client) пар на один физический Redis-ключ; проверено напрямую конкретным примером,
    не абстрактно). `src/filter/rate_limit_redis.rs` в корне — тонкий facade
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
20. **Filter Chain (CoR)** — `src/filter/chain.rs`. Новый guard = `impl RequestFilter` + push в chain. `service.rs` не трогать. `phase: "response"` scripts пропускаются в request-фазе (`MiddlewareGuard::apply`, теперь в `crates/conduit-middleware/src/guard.rs` — issue #114/#141; сборка chain'а остаётся здесь, в `src/filter/chain.rs`).
20a. **Feature warnings** — `config::validate::feature_warnings()`. WASM (без `--features wasm`) + OTLP (без `--features otlp`) → `tracing::warn!` при старте и hot-reload. `/reload` response включает поле `warnings: [...]`.
21. **Handler Registry** — трейт `LocalHandlerImpl` в `src/handler/mod.rs`. 7 handler structs реализованы. `dispatch_local` → `build_handler()` + `handle()`.
22. **Routing Strategy** — трейт `LoadBalancingStrategy` в `src/proxy/strategy.rs`. Новая стратегия = новый struct + `from_config()` arm. `router.rs` не трогать.
23. **CLI Commands** — трейт `CliCommand` в `src/cli/mod.rs`. Новая команда = struct + arm в `dispatch_command()`. `main()` не трогать.
24. **YAML конфиг** — `serde_yaml`, `from_yaml()` в `parse.rs`, автопоиск `conduit.yaml/yml`.
25. **Provider pattern** — `Provider` trait в `src/config/provider.rs`. `FileProvider` (one-shot + auto-reload). `KubernetesProvider` (feature = "kubernetes") в `src/config/kubernetes.rs`.
26. **WASM middleware** — `type: "wasm"` в middleware array, feature = "wasm". Wasmtime, 17 host-функций
    в request-фазе (+7 в response-фазе, см. пункт бэклога "WASM `on_response()` hook" — 4 из них те же
    самые имена, переиспользованные в обоих линкерах (`conduit_set_response_header`,
    `conduit_set_response_body`, `conduit_get_plugin_config`, `conduit_log`), так что суммарно
    различных имён — 20), fail-open. `crates/conduit-plugin-wasm/src/wasm.rs` (issue #114/#141, было
    `src/filter/wasm.rs` до извлечения крейта). Плагины экспортируют `on_request() -> i32`.
    Память должна быть экспортирована как `"memory"`, если плагин вызывает хоть одну host-функцию,
    читающую или пишущую в неё — плагин без единой такой функции (например, всегда возвращающий
    `on_request() -> 0`) работает и без `memory` экспорта; см. issue #381 про то, что при пропущенном
    экспорте это вырождается в молчаливую деградацию без единого warning в лог, а не в чёткую ошибку.
    (Число реюзов поправлено 2026-09-07 по итогам ревью PR #382 — было ошибочно "2"; счёт "12"→"17"
    поправлен в этой же сессии, Step 1c аудит, не совпадал ни с одной реальной точкой в истории фичи.)
27. **MiddlewareGuard** — объединяет Rhai ("script") и WASM ("wasm") в `crates/conduit-middleware/src/guard.rs`
    (issue #114/#141, было `src/filter/chain.rs` до извлечения крейта; сборка chain'а сама по себе
    остаётся в `src/filter/chain.rs` — правило "Filter Chain" ниже про это). Порядок entries
    соблюдается. `ScriptGuard` = type alias для совместимости.
28. **CGI** — вопрос "входит в Conduit или отдельный проект" остаётся **открытым**
    (пере-рассмотрено 2026-09-12, было "не входит, отдельный проект" — см. ниже);
    реализацию не начинать до снятия блокеров.
    **Детали пересмотра 2026-09-12** (пользователь явно инициировал обсуждение — условие
    пересмотра решения выполнено). Итог обсуждения: решение **подтверждено как всё ещё
    открытый вопрос, не как "не пересматривать"** — issues
    [#290](https://github.com/lopatnov/conduit/issues/290)/[#291](https://github.com/lopatnov/conduit/issues/291)
    получили полный разбор архитектуры для настоящего invoke-on-demand (не путать с уже
    реализованным fixed worker-pool рецептом, PR #386): три конкурирующих варианта —
    (1) классический CGI (spawn на запрос, stdin/stdout/env — у пользователя уже есть
    рабочий прототип, `lopatnov/express-reverse-proxy`), (2) native-addon мост
    (napi-rs/PyO3 — тот же процесс-на-запрос lifecycle, что и CGI, только типизированный
    канал вместо текстового), (3) встроенный движок (rquickjs/аналог для Python — без
    OS-process-spawn, но требует спроектировать и вечно поддерживать capability-scoped
    SDK с нуля). "Вместе с Conduit или отдельным проектом" — по-прежнему открытый вопрос,
    завязан на выбор транспорта (вариант 3 естественно живёт внутри Conduit по
    существующей feature-gating дисциплине; варианты 1/2 архитектурно ближе к отдельному
    sidecar-бинарнику). **Решение по-прежнему: implementation НЕ начинать** — оба issue
    явно помечены blocked до выбора транспорта, политики RAM-admission (queue vs reject
    под нагрузкой) и streaming-дизайна для больших тел запроса/ответа. Полный разбор — в
    телах issues, не здесь (не дублировать).
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
    **Обновление 2026-09-25 (Pingora 0.9, PR #450) — посылка про `prometheus` устарела, вывод решения
    нет.** `pingora-core` 0.9 больше не зависит от `prometheus` (тот вынесен в отдельный
    `pingora-prometheus`, который Conduit не использует; серверный `/metrics` Pingora мы не подключали
    никогда), и `prometheus 0.14` теперь тянут только `lopatnov-conduit` и `lopatnov-conduit-metrics` —
    значит, `metrics` *можно* сделать опциональной фичей. Измерено (`cargo tree -i`): из дерева ушли бы
    только `protobuf` и `protobuf-support` (2–3 крейта, <1% бинаря; `fnv`, `lazy_static`, `memchr`,
    `parking_lot`, `thiserror` нужны другим), а запись метрик (`ConduitMetrics`, `src/proxy/service.rs`)
    проходит по горячему пути → `#[cfg]` на каждый вызов. Решение остаётся (always-on), но причина теперь
    другая: малый выигрыш при реальной работе, а не «всё равно тянет pingora-core». Пересматривать только
    если понадобится бюджет размера. Записано в #451.

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

## Правила

- `pingora-cache = "0.9"` — кастомный cache key обязателен (CVE-2026-2836). С 0.9 у `CacheKey::new` нет `namespace`: хост вшивается в primary через `\0` (`build_cache_key` в `crates/conduit-cache/src/cache.rs`), хэши отличаются от 0.8 — персистентный кэш (disk/redis) после апгрейда холодный
- Pingora `"0.9"` — только 0.8+ (3 CVE исправлено в 0.8; в 0.9 ушли `protobuf 2.28.0` и `daemonize`)
- Pingora 0.9: `RequestHeader`/`ResponseHeader` без `DerefMut` — заголовки менять только через `insert_header`/`append_header`/`remove_header`, не через `.headers.*`
- `schema/conduit.schema.json` — вручную синхронизировать со `schema.rs`. Обновлён 2026-05-31 со всеми Phase 4 полями. Валидировать: `node -e "JSON.parse(fs.readFileSync('schema/conduit.schema.json','utf8'))"`
- HTTP/3 (Phase 5) — ждём Pingora Issue #95, ~август 2026
- `src/main.rs` тонкий: CLI → `dispatch_command()` → command struct → `execute()`
- `tls.versions`/`tls.ciphers` — **не работают, отклоняются на validate()** (issue #189,
  2026-08-29). Pingora (0.8 и 0.9 — проверено по исходнику) rustls `TlsSettings` не даёт API для
  ограничения версий/шифров — подробности в разделе "Безопасность" ниже.
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

## Журнал сессий

> Полная история — в `.claude/logs/session-log.md` (вынесена 2026-09-20: занимала 76% этого
> файла, ~313 КБ, и подгружалась целиком в каждую сессию и при каждой компакции). Здесь
> остаются **две последние записи**. У записи один дом: новую добавляй **в конец этой секции**,
> а если записей стало больше двух — самую старую **вырежи** и допиши в конец
> `.claude/logs/session-log.md` (не копируй — иначе журналы разойдутся, как строка hygiene
> 2026-09-18). Всё ещё открытое — комментарием на issue (Step 8 цикла), а не прозой здесь:
> «открыто на конец сессии» устаревает за часы.

### Реализовано в сессии 2026-09-25/26 (Pingora 0.9.0 на ветке миграции — PR #450; #314 — PR #460 и #315 — PR #462 смерджены; main по-прежнему заморожен)

- **[PR #450](https://github.com/lopatnov/conduit/pull/450)** (squash `0456654`; решение владельца «обновим pingora в ветке миграции», Dependabot #422 закрыт как superseded). Bump-only, три поломки: (1) `.headers.remove/append` → `remove_header/append_header` (в 0.9 нет `DerefMut`); (2) `CacheKey::new` без `namespace` → `build_cache_key` строит `"{host}\0{scheme:path?query}"`: **персистентный кэш (disk/redis) один раз холодный, старые `disk:`-файлы не чистятся** (файлы удаляет только `purge`); (3) `Storage::purge` → `PurgeTarget`/`PurgeOutcome`, общий хелпер `purge_target_key` (`Exact` с id → `NotFound`). Мёртвые ignore удалены (`.cargo/audit.toml`, `osv-scanner.toml`): `protobuf 2.28.0` и `daemonize` ушли из lockfile (678→662 пакета). Проверено: 1730/1980 тестов, clippy на 7 профилях, `cargo hack` 79/79, негативные контроли обоих новых тестов.
- **Ревью безопасности — 4 раунда, оба HOLD были моими ошибками в `SECURITY.md`:** PASS `c0e949a` → HOLD `12d6fab` («None at present» — ложь: `rsa 0.9.10`, RUSTSEC-2023-0071, в lockfile через `jsonwebtoken` при `jwt`, фикса нет) → HOLD `c3d2569` («приватного RSA-ключа нет» — неверно: TLS-ключи есть, но идут через rustls, не через `rsa`; «ждём rsa 0.10» — advisory говорит, что RC тоже затронуты) → PASS `6aa67e1`. **Урок:** в публичном security-документе писать только то, что проверяется построчно; каждая правка документа меняет head и требует нового PASS.
- **Поймано в моих же утверждениях:** `Acceptor::from_server_config` — НЕ обход для #189 (listener принимает только `TlsSettings`); заметка `tls.rs` про SNI устарела (0.9 `set_cert_resolver` — «dynamic SNI-based selection»). Исправлено в доках/комментариях/CLAUDE.md, #451 поправлен.
- **Производительность:** passthrough на 0.9 медленнее на ~4–5% (RPS, p50): 4 замера CI (−2.6…−5.6%) + контролируемый WSL A/B (ABBA, прогрев, `taskset`, `oha` 1.16.0): −3.9% ±1.7%; трёхплечевой: **вся цена — `HttpUpstreamRequestPolicy::standard()`** (санитизация hop-by-hop заголовков): head −4.25% ±1.5%, head+`preserve()` −0.4% ±1.5% (= 0.8). `preserve()` не ставить (снимает защиту). CI-job меряет голову ПЕРВОЙ и без прогрева → ~−1% систематического сдвига на любом PR (на 7 PR без правок горячего пути: −2.8…+0.3%). Профиль `sanitize_h1_upstream_request` и патч для Cloudflare — **отложено владельцем → #459** (стенд сохранён комментарием там и в `~/perf` в WSL); #458 закрыт.
- **Issues:** #451 (что 0.9 открывает: горячая замена сертификата + мультисертификатный SNI — один хук `set_cert_resolver`; PROXY protocol через `PreTlsProcess`; `upstreamTls.ca`; `daemon_wait_for_ready`; purge через `expire`; пул для TLS-handshake; #58–#64 по исходнику 0.9 не перепроверены), #457 (**реальная гонка** в upload: `stream_field_to_file` не делает `flush()` → `200` уходит до записи файла; уронила `upload_single_file_returns_ok` в CI на Linux), #458 (закрыт), #459.
- **Вопрос владельца «отказаться от prometheus?»:** Pingora больше не заставляет (выгода получена: `protobuf 2.28` ушёл), но `prometheus 0.14` теперь наш собственный `/metrics`; опциональной фичей `metrics` сделать можно, выигрыш ~2–3 крейта (<1%) → решение #31 остаётся, причина обновлена (см. заметку под ним).
- **#314 — [PR #460](https://github.com/lopatnov/conduit/pull/460)** (**смерджен squash `99df275` 2026-09-25 22:41 UTC, #314 закрыт**; по указанию владельца: разбиение `schema.rs` → `schema/` и скрипт подсчёта строк + CI-job `code-length` в **одном** PR, скрипт — последним шагом как доказательство). Посылка issue («1424 строки, выше hard limit») устарела: после экстракций файл 935 строк и **ниже 400 code lines** по метрике владельца; разбиение сделано по решению владельца (ревьюабельность #222). Метод: «ножницы» по диапазонам строк (ничего не перепечатано) + верификатор (60 элементов побайтово, 69 публичных имён; негативные контроли: перестановка полей, перестановка untagged-вариантов, потеря re-export) + `-- --list` до/после **идентичны** (1726/1978) + clippy на 7 профилях. Скрипт: string-aware сканер, исключает `#[cfg(test)]`, `cfg(all(test, …))`, `#[test]`, тест-файлы; к мерджу 66 юнит-тестов и 30 мутаций пойманы (две первые не различали — переписаны так, чтобы неверный разбор менял счёт).
- **Найдено по ходу:** мой первый `loc_probe` завышал `retry.rs` (971 vs 368: тест-модуль `all(test, feature = "proxy")`) → на деле «между 400 и 1000» шесть файлов (`admin/api.rs` 917, `conduit-static/handler.rs` 619, `plugin-wasm/wasm.rs` 523, `proxy/request/filter.rs` 520, `upstream/health.rs` 484, `server/builder.rs` 405), выше 1000 — только `validate.rs` 1495 (#315). rustfmt переносит длинные атрибуты (12 таких в репозитории) → скрипт склеивает многострочные. Ревью безопасности #460 — три раунда: PASS `746d673`; замечания (пагинация/автор комментария в job, `--base=--output=`, устойчивость, экранирование, переименования) исправлены коммитом `2514031` → **HOLD** (B1: склейка многострочных атрибутов кубична на незакрытом `#[` — 169 с на файле 1 МБ; B2: моя ложная фраза «токен read-only на Dependabot-запусках» — принудительно read-only только на fork-`pull_request`; плюс N1–N9: пути `git diff` без `-z`, локаль, унаследованные `GIT_*`, `macro_rules!`, `HTTP 000000`, комментарии TOML) → `e07b4f3` **PASS** (merge — `--match-head-commit`). Нюанс ревьюера, оставленный без правки, чтобы не менять проверенный SHA: комментарий/docstring `MAX_ATTR_LINES = 50` неточен на единицу — **владелец разрешил поправить в #315**.
- **Процессное:** (1) `git add A B` падает целиком, если один путь не существует (удалённый файл) → первый коммит захватил только переименование; мой `echo "(clean)"` после `git status` врёт — проверять `git status --porcelain | wc -l`. (2) Возобновлённый через `SendMessage` агент снова остался без своего worktree (`git worktree list`) — писать ему явную команду `git worktree add` ВНЕ чекаута; свежий агент с `isolation: "worktree"` worktree получил. (3) Git Bash: `python3` — заглушка Store, есть только `python`; MSYS `/tmp` не виден нативному Python. (4) heredoc с кавычками/слэшами в Bash ломается → `Edit`/`Write`. (5) `kill` (SIGTERM) Conduit = graceful shutdown ~60 с; в бенчмарках `kill -9`. (6) Нельзя гонять Windows-`cargo` и замер производительности одновременно; шум ~5–6% CV на прогон → парные раунды. (7) Замечания рецензентов на CI-job (footprint/performance имеют тот же дефект пагинации/автора) — в этом PR не трогал; **токен в одном job с кодом PR** (находка CodeRabbit Major на `e07b4f3`, поймана уже после PASS) — **[#461](https://github.com/lopatnov/conduit/issues/461)** для всех report-jobs сразу: вердикт ревьюера — усиление защиты, граница доверия сегодня не пересекается (fork — токен read-only; same-repo/`push` — нужен push-доступ и workflow берётся из своего коммита; `pull_request_target`/`workflow_run` нет), реально важны `footprint`/`performance` (запускают `build.rs`/proc-macro bumped-зависимостей Dependabot в write-jobs; `oha` без checksum), `code-length` исполняет только stdlib-Python. Настройки репозитория проверены: `default_workflow_permissions: read`, `can_approve_pull_request_reviews: false`. (8) **Владелец спросил «ты все комментарии смотрел?» — нет, не полностью:** я прогнал три потока списком и первыми 100–200 символами, полностью открыл только Gitar; CodeRabbit пришёл с ревью на новый head уже ПОСЛЕ зелёного CI (его allowance — 1 ревью в час, стартует с задержкой) и нёс находку Major. **Правило:** перед мерджем — открыть тело каждого нового ревью/комментария на ТЕКУЩЕМ head (не только счётчики и заголовки), заново после того, как все проверки закончились, и убедиться, что среди них нет `pending` от ботов (`Review in progress`); находку «вне диффа» разбирать отдельным комментарием с диспозицией. Память `feedback_read_all_pr_comments` обновлена.
- **#315 — [PR #462](https://github.com/lopatnov/conduit/pull/462)** (**смерджен squash `f93889c` 2026-09-26 06:22 UTC, #315 закрыт**). `src/config/validate.rs` (1495 code lines, последний файл выше hard limit) → `src/config/validate/`: `mod.rs` (`validate()`, `feature_warnings()`, `pub use`) + 10 модулей (`warnings` 326, `auth` 236, `proxy` 221, `cross_site` 159, `site` 142, `handlers` 112, `tls` 102, `limits` 87, `proxy_loop` 87, `net` 64) + `tests.rs` (188 тестов). **План архитектора на #222 (2026-08) устарел по цифрам:** 36 `#[cfg]`-гейтов, а не 12 (каждый остался на своём элементе дословно); `pub(super)` — только у 30 элементов, которые реально зовут другие модули (не «каждая fn»); тестам нужны 4 явных импорта (`validate`, `feature_warnings`, `sanitize_for_log` по имени + типы), а не ~8. Пять коммитов для ревью: rename `validate.rs → validate/mod.rs` (100%), разрезание production по диапазонам строк, перенос тестов, пути в комментариях/доках, формулировка `MAX_ATTR_LINES` (+ граничный тест; разрешено владельцем). **Доказательства:** генератор + верификатор относительно `99df275` (69/69 fn/const байт-в-байт, нет выдуманных элементов, поток токенов тестов идентичен — 9447, строковые литералы сравниваются точно, поэтому raw-фикстуры не тронуты дедентом; 9 намеренно сломанных копий пойманы, правка одного импорта верификатор не трогает); `cargo test -- --list` до/после идентичны на default/standard/full (962/1020/1077, те же бинарии; `config::validate::tests` = 171/170/176); clippy `-D warnings` на 7 профилях; `cargo test` 962 / full 1075 / static-server 771; `cargo hack --each-feature` 79/79; footprint-отчёт байт-в-байт как у #460. Ревью безопасности сделано на локальных коммитах ДО пуша (SHA от пуша не меняется) — PASS `2cba9e2`, затем дельта-PASS `6771abe` (только комментарии). CodeRabbit: 3 устаревших комментария (два с номерами строк «381-385»/«474-476» — **прослежены по истории**: `git log -S` → коммит `a5b0f19` v0.3.0; они значили ветку `Url` в `validate_proxy_route_target` и проверку `Invalid upstream URL` в `validate_target_urls`, до которой доходит `validate_route_config`; цифры уже тогда были неточны) — исправлены `6771abe`; «Description check» — у репозитория есть `.github/pull_request_template.md`, секции `Type of Change` и `Checklist` обязательны (добавил в тело PR); ревью «Grok» под аккаунтом владельца — без блокирующих, дублирование JWT-проверок → заметка в #316. Верификатор получил явный список 4 разрешённых правок комментариев (без него падает ровно на них).
- **Процессное (часть 2):** (1) **Владелец: «А зачем ты ждёшь?» → «там написано: кроме случаев, когда я прошу — я прошу»** — правило «пуш не чаще раза в час» не ждать, готовую ветку пушить сразу (память `feedback_dont_wait_out_push_interval`). (2) **Владелец: устаревший комментарий/описание задачи — смотреть, что имелось в виду, по истории, а не гадать по сегодняшнему коду** (`git log -S`, blame, старый SHA) — память `feedback_read_history_dont_guess_stale`; тот же принцип для устаревших цифр плана #222. (3) Правки файлов Python-скриптом через heredoc в Bash **молча портят escape-последовательности** (`\n` превращается в реальный перевод строки, `\b` — в символ backspace внутри регулярки: скрипт «работает», но ищет не то) — править `Edit`/`Write`, а не heredoc-патчем. (4) CodeRabbit дописывает в тело PR свой блок release notes — при `gh pr edit --body-file` брать текущее тело и вставлять своё ПЕРЕД его блоком, не перезаписывать. (5) Автор Agent-ревьюера с `isolation: worktree` получает свой worktree в `.claude/worktrees/agent-*` (проверено `git worktree list`); после ответа его worktree и ветку `worktree-agent-*` удалять (`git worktree remove --force --force`). (6) Цепочка проверок (`clippy` 7 профилей → `cargo test` → `cargo hack`) удобна одним фоновым скриптом с ожиданием `DONE` предыдущего шага; `cargo hack` держит манифесты изменёнными пока идёт — коммиты до/после, не во время.
- **Routine отключена (2026-09-26, по явному OK владельца «можно архивировать или удалить, я запускаю цикл сам»):** `trig_01Ehd6ceyaWxB6aytQwuydsp` падала каждую ночь (`folders: []`, репозиторий не подключён); `RemoteTrigger update enabled:false` (delete в инструменте нет; обратимо). Цикл владелец запускает вручную — `feature-workspace-cycle.md` дополнен пометкой; Routine не чинить и не включать. **Dependabot #452–#456** (clap, rustls, clap_complete, jsonwebtoken, async-compression) — по решению владельца ждут, пока смерджим миграцию (#453/#455 на TLS/auth-поверхности — обязательное ревью безопасности при взятии).
- **Открыто на конец сессии:** #459 (отложено владельцем), #461 (токен в report-jobs), #451, #457, #444, #449, #447, #358, Sonar-находки, ~20 старых remote-веток; **следующий шаг — #316** (8 листовых валидаторов в крейты-владельцы; #315 закрыт; сначала сверить план с реальным `validate/`, а не с планом 2026-08; учесть заметку про дублирование JWT-проверок в `auth.rs`), затем #222 (`conduit-config`), #145–#148.

### Реализовано в сессии 2026-09-26 (#316 закрыт — PR #467; правило «одна issue = один PR»; жалоба владельца на стоимость процесса; main по-прежнему заморожен)

- **#316 — [PR #467](https://github.com/lopatnov/conduit/pull/467)** (squash `3a69220`; `Closes #316` не закрыл issue — база PR не default-ветка, закрыл вручную). 16 крейтов получили `validate.rs` и/или `warnings.rs`: 15 валидаторов (rateLimit, limits, ipFilter, cors, middleware, redirects, fallback, upload, metrics, cache, tcp, proxy, jwtAuth, consumers, forwardAuth) и 22 текста предупреждений переехали из `src/config/validate/`; в корне остались `validate_api_key`, `validate_tcp_site` (комбинации), `site_uses_redis_store`/`site_has_cache_config` (передаются bool'ом — второй не может уйти в cache: proxy-http зависит от cache). Гейты: `warnings.rs` 24 → 0, `auth.rs` 3 → 0, `validate/` 42 → 15 (proxy_loop 9 + redis 6). Механизм S4: крейт отдаёт `COMPILED = cfg!(feature=..)` + `feature_warning(i, cfg)`; корень зовёт плоско в старом порядке и держит **17 compile-time assert'ов** `COMPILED == cfg!(feature = "<root>")` (расхождение фич = ошибка сборки, а не молча пропавшее предупреждение). Слайсы S1–S5 — коммиты одного PR (S0/S0b — #465/#466, смерджены раньше правила: ошибка плана D9). Корневой `forward-auth` больше не тянет `dep:url` (S5).
- **Проверено:** golden без изменений в 61 комбинации фич; `--list` идентичен (968/1026/1083); зависимости корня идентичны; hack `--workspace --each-feature` 79/79 + depth-2 powerset для S5 14/14; clippy `-D warnings` 8 профилей + `--workspace --tests`; `cargo test --workspace` 1776. Контроли: для S1–S3 по 12–18 мутаций на верификатор; для S4 — плюс полярность (перевернуть `COMPILED ||` → golden падает в `--no-default-features`) и расхождение фич (`consumers` тянет `auth-jwt/jwt` → E0080 «must be enabled together»). **Security-ревью: один проход по 5 коммитам, PASS на `ea218ec`, без блокеров** (~15 мин, 264 тыс. токенов); его находки старые: #447 (`[::1]`) и ValidationError без `sanitize_for_log` (не заводил).
- **Технические находки:** (1) rustfmt переставляет `pub mod` (сортирует смежные объявления, в т.ч. рядом с `#[cfg]`) — после генератора всегда `cargo fmt`. (2) Heredoc с Python в Bash опять молча испортил `\\n` (два раза) — только `Write`/`Edit`. (3) Доказательство переноса текстов сообщений: сравнение строковых литералов по значению (без `\`-продолжений; в базе предварительно вычеркнуты `#[cfg]`-атрибуты, т.к. в них тоже строки с именами фич) — этого достаточно, отдельные генераторы на каждый слайс не нужны. (4) **Gitar с auto-apply закоммитил прямо в ветку миграции** (`8f5e37a`, «relax event loop lag assertion», флак `eventloop_lag_ms`) без PR-ревью; безвредно, но auto-apply включён на #152 (на #467 выключен). (5) `Cargo.lock`/`Cargo.toml` перезаписывает `cargo hack` пока идёт — в worktree ничего не коммитить (снова соблюдено).
- **Жалоба владельца (2026-09-26): «затратный процесс»** — отдельные PR на одну задачу, security-ревью по кругу, 4 генератора/верификатора и ~60 контролей на чисто механические переносы, баг #447 вынесен в issue вместо коммита в тот же PR. Разбор F1–F5 (пропорциональность, объём security-ревью, баги в тех же файлах в тот же PR, «разбить на N PR» — вопрос владельцу, бюджет на issue) подготовлен; **владелец подтвердил, правила записаны в `.claude/rules/workflow.md`** (раздел «Proportionate process»: одна issue = один PR = один проход security-ревью, доказательства по риску, баги затронутого кода — в тот же PR отдельным коммитом, бюджет «второй раунд ревью или ~2 часа — спросить», вопросы владельцу коротко и без внутренних меток; в «Security review» гейт остался безусловным, но один проход на финальном head с выводом верификатора) и в шаге 2 цикла. Память: `feedback_proportionate_process`.
- **PR #152 (на 8f5e37a, «глянь»):** 39 из 41 проверки зелёные; красные две — CodeQL (26 алертов в тестовом коде: те же, что #463; закрыть может только владелец в Security → Code scanning) и SonarCloud QG (E Security Rating из-за 2 `secrets:S6739` — `redis://alice:s3cret@…` в `tests.rs:645` и `testdata/validators_sink.json:541`, тесты проверяют, что пароль не попадает в предупреждение). Решение по ним — за владельцем (пометить false positive в Sonar либо переименовать пароль в тестовых данных). Комментарии #152: Gitar Approved (6/6 закрыто), новых замечаний нет.
- **Issues:** #468 (fast-follow: дедуп JWT-проверок secret/jwksUrl, D10). `CHANGELOG.md` `[Unreleased]` дополнен строкой про смену tracing-таргетов и перенос валидации.
