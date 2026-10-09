# Источники для сверки (перенесено из CLAUDE.md, #512)

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
