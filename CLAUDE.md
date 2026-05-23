# Conduit — Высокопроизводительный реверс-прокси на Rust

> Основан на Cloudflare Pingora · HTTP/1.1, HTTP/2, HTTP/3-ready · Один бинарник · JSON-конфиг


---

## Language & Localization

### Documentation and code language
- All documentation (README, CLAUDE.md, inline docs, API descriptions) — **English only**
- All code comments — **English only**
- Commit messages — **English only**
- Variable names, function names, class names — **English only**

### Note on UI localization
Conduit is a developer tool (reverse proxy) — it has no end-user UI to localize.
All CLI output, error messages, and log entries are in **English only**.
Documentation translations may be added to a `docs/` folder in Phase 4 if community demand arises.

## Видение проекта

**Conduit** — production-grade реверс-прокси и файловый сервер на Rust с использованием
фреймворка Pingora от Cloudflare. Целевая аудитория — разработчики и DevOps-инженеры, которым
нужна простота `express-reverse-proxy` и производительность уровня Nginx, в одном бинарнике без
зависимостей.

Приоритеты (по убыванию важности):

1. **Корректность** — ни один запрос не теряется, не портится, не уходит не туда
2. **Скорость** — 150k+ req/s для статики; p99 задержка прокси < 2 мс
3. **Простота** — один JSON-файл описывает всё; `conduit init` запускает за 30 секунд
4. **Production-ready** — TLS, HTTP/2, кэш, горячая перезагрузка, Prometheus, структурированные логи
5. **Портфолио** — демонстрация продвинутого Rust, async networking, системного проектирования
6. **Качество кода** — демонстрация возможностей написания качественного кода на Rust

---

## Сравнение с аналогами

| | Nginx | Caddy | Traefik | express-reverse-proxy | **Conduit** |
|---|---|---|---|---|---|
| Язык | C | Go | Go | Node.js | **Rust** |
| Конфиг | DSL | Caddyfile/JSON | TOML/YAML | JSON | **JSON + Schema** |
| Admin API | ❌ | ✅ | ✅ dashboard | ❌ | ✅ |
| Один бинарник | ✅ | ✅ | ✅ | ❌ | ✅ |
| Auto-TLS (Let's Encrypt) | ❌ | ✅ | ✅ | ❌ | ✅ Phase 3 |
| HTTP/2 к upstream | ✅ | ✅ | ✅ | ❌ | ✅ |
| Proxy кэш | ✅ | ✅ | ✅ | ❌ | ✅ |
| Hot-reload dev | ❌ | ❌ | ❌ | ✅ | ✅ |
| Загрузка файлов | ❌ | ❌ | ❌ | ✅ | ✅ |
| Prometheus | плагин | плагин | ✅ | ❌ | ✅ |
| `conduit validate` CI | ❌ | ❌ | ❌ | ❌ | ✅ |
| Upstream health | ❌ | ✅ | ✅ | ❌ | ✅ |
| Балансировка | RR, LC, IPHash | RR | RR, WRR, LR | ❌ | **RR, WRR, Random, LC, LRT, IPHash, CH** |
| Динамич. upstream | ❌ | ❌ | ❌ | ❌ | ✅ (в памяти) |
| IP allow/deny | ✅ | ✅ | ✅ | ❌ | ✅ |
| Скриптинг | Lua | ❌ | ❌ | ❌ | Rhai Phase 4 |
| Config reload | `nginx -s reload` | `caddy reload` | auto | ❌ | `conduit reload` |
| Роутинг по заголовку | ✅ | ✅ | ✅ | ❌ | ✅ Phase 3 |
| Docker Compose | contrib | contrib | native | ❌ | contrib |

---

## Стек технологий

### Ядро

| Crate | Версия | Роль | Почему |
|---|---|---|---|
| `pingora` + `pingora-core` + `pingora-proxy` + `pingora-load-balancing` + `pingora-cache` | `0.8` | Proxy + Cache framework | Единственный production-ready Rust proxy framework. CloudFlare. HTTP/2, connection pooling, upstream health, TLS, встроенный cache. 0.8 исправляет 3 критических CVE. |
| `tokio` | `1` | Async runtime | Pingora использует внутри — выбора нет. |
| `axum` | `0.8` | HTTP framework | Admin API (порт 2019) + Upload сервер (loopback). Статика/health/metrics — напрямую в Pingora. |

### Конфиг и парсинг

| Crate | Версия | Роль |
|---|---|---|
| `serde` + `serde_json` | `1` | Парсинг JSON конфига. |
| `serde_path_to_error` | `0.1` | Точные пути к ошибкам. Критичен для UX. |
| `indexmap` | `2` | Сохранение порядка proxy routes и middleware chain. |
| `humantime` | `2` | Парсинг `"1d"`, `"30m"` в `maxAge`, `ttlSecs`. |

### Производительность и состояние

| Crate | Версия | Роль |
|---|---|---|
| `arc-swap` | `1` | Wait-free чтение конфига при hot reload. |
| `dashmap` | `6` | Concurrent sharded HashMap для rate limiter. |

### Функциональность

| Crate | Версия | Роль |
|---|---|---|
| `async-compression` | `0.4` | Async gzip + brotli. |
| `notify` | `7` | FS watcher для hot reload браузера. |
| `multer` | `3` | Async multipart/form-data. Нативен для Axum/Hyper. |
| `uuid` | `1` | UUID v4 для имён файлов при upload. |
| `mime_guess` | `2` | Content-Type по расширению. |
| `regex` | `1` | Path rewrite, IP matching. Компилируется один раз при старте. |
| `rhai` | `1` | Встраиваемый скриптовый язык для middleware. Phase 4. |
| `instant-acme` | `0.7` | ACME/Let's Encrypt клиент. Phase 3. |
| `rcgen` | `0.13` | Генерация self-signed сертификатов (dev). |

### Логирование и метрики

| Crate | Версия | Роль |
|---|---|---|
| `tracing` + `tracing-subscriber` | `0.1` / `0.3` | Structured logging + JSON. Pingora использует внутри. |
| `prometheus` | `0.13` | Prometheus metrics. |

### CLI и UX

| Crate | Версия | Роль |
|---|---|---|
| `clap` | `4` | CLI с derive-макросами. `clap_complete` + `clap_mangen` бесплатно. |
| `dialoguer` | `0.11` | Prompts для `conduit init`. |
| `indicatif` | `0.17` | Progress bars для `conduit probe`. |

### Обработка ошибок и утилиты

| Crate | Версия | Роль |
|---|---|---|
| `thiserror` | `2` | Типизированные ошибки в lib-модулях. |
| `anyhow` | `1` | `?` в binary entry points. |
| `bytes` | `1` | Байтовые буферы. Используется Pingora/Hyper. |

### Dev зависимости

| Crate | Версия | Роль |
|---|---|---|
| `reqwest` | `0.12` | HTTP-клиент в тестах. HTTP/2 + rustls. |
| `criterion` | `0.5` | Статистически корректные бенчмарки. |
| `serial_test` | `3` | `#[serial]` для тестов Admin API. |
| `tempfile` | `3` | Временные директории для тестов. |

### Что намеренно НЕ используется

| | Почему нет |
|---|---|
| `native-tls` / `tokio-tls` | Проблемы при кросс-компиляции. Pingora использует rustls. |
| `log` + `env_logger` | Устарел. `tracing` строго лучше для async. |
| `opentelemetry` | Избыточно. Нам нужен только Prometheus. |
| `rayon` | Нет CPU-параллельных вычислений — всё async I/O. |
| `once_cell` / `lazy_static` | Заменён `std::sync::OnceLock` (Rust 1.70+). |
| `bollard` (Docker API) | Docker service discovery не нужен целевой аудитории. |

---

## Фундаментальные архитектурные решения

**Не пересматривать без явного обсуждения.**

---

### Решение 1: Где что обрабатывается

| Тип | Обработка | Причина |
|---|---|---|
| Статика | Напрямую в Pingora | Hot path. 150k+ req/s. |
| Health, Metrics, Hot-reload, Fallback | Напрямую в Pingora | Простая логика, нет смысла в IPC. |
| **Upload** | **Axum loopback `127.0.0.1:0`** | `multer` нативен для Hyper. Кастомный адаптер к Pingora session — хрупко. Не hot path. |
| **Admin API** | **Axum порт 2019** | Управление. Не hot path. |

```rust
pub enum UpstreamTarget {
    Proxy  { peer: SocketAddr, strip_prefix: Option<String> },
    Local  (LocalHandler),
    Upload { addr: SocketAddr },
}

pub enum LocalHandler {
    StaticFile { root: PathBuf, options: Arc<StaticOptions> },
    Health     { config: Arc<HealthCheckConfig> },
    Metrics    { token: Option<String> },
    HotReload  { config: Arc<HotReloadConfig> },
    Fallback   { config: Arc<FallbackConfig> },
}
```

`Local` → `handle_local()` → `return Ok(true)` (Pingora прерывает pipeline).
`Upload`/`Proxy` → `return Ok(false)` (Pingora продолжает через `upstream_response_filter`).

Общий код для Local — `write_local_response()` в `src/handler/response.rs`:
compression, X-Response-Time, custom headers, Prometheus, inflight--.

---

### Решение 2: Upload сервер — `port: 0`, не конфигурируется

**Стартует только если `upload` задан хотя бы в одном сайте.**

```rust
let upload_addr = if config.sites.iter().any(|s| s.upload.is_some()) {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(run_upload_server(listener, app_state.clone()));
    Some(addr)
} else {
    None
};
```

Порт не конфигурируется — несколько экземпляров Conduit не конфликтуют автоматически.
Admin API конфигурируемый (`global.admin.bind`) — потому что адрес нужен для `conduit reload`.

---

### Решение 3: CLI — только subcommands

Всё через subcommands, нет флагов запускающих альтернативное поведение:

```
conduit                                      # запустить сервер
conduit init                                 # создать conduit.json
conduit validate                             # проверить конфиг
conduit probe                                # проверить upstream'ы
conduit fmt                                  # форматировать конфиг → stdout
conduit fmt --write                          # форматировать → перезаписать файл
conduit reload                               # перезагрузить конфиг (Admin API)
conduit status                               # статус сервера
conduit upstreams                            # состояние upstream'ов
conduit upstreams add --route /api \
  --target http://b3:4000                    # добавить upstream в память
conduit upstreams remove --route /api \
  --target http://b1:4000                    # убрать upstream из памяти
conduit upstreams weight --route /api \
  --target http://b1:4000 --weight 5        # изменить вес (только WRR)
conduit shutdown                             # graceful shutdown
```

**Важно:** `upstreams add/remove/weight` — только в памяти. При рестарте сервера
изменения теряются — используй `conduit reload` чтобы применить конфиг из файла.

---

### Решение 4: Версионирование конфига через VersionProbe

```rust
#[derive(Deserialize, Default)]
struct VersionProbe { version: Option<u32> }

pub fn load_config(path: &Path) -> Result<AppConfig> {
    let text = fs::read_to_string(path)?;
    let probe: VersionProbe = serde_json::from_str(&text).unwrap_or_default();
    if probe.version.unwrap_or(1) > CONFIG_VERSION {
        return Err(ConfigError::UnsupportedVersion { ... });
    }
    let jd = &mut serde_json::Deserializer::from_str(&text);
    let raw: ConfigFile = serde_path_to_error::deserialize(jd)?;
    Ok(normalize(raw))
}
```

---

### Решение 5: Два уровня конфигурации

```
AppConfig
  ├── global: GlobalConfig     — процесс: workers, backlog, admin, shutdownTimeoutSecs
  └── sites:  Vec<SiteConfig>  — виртуальные хосты
```

**`ConfigFile` untagged enum — порядок КРИТИЧЕН** (serde пробует сверху вниз):

```rust
#[serde(untagged)]
pub enum ConfigFile {
    Full(AppConfig),         // ПЕРВЫЙ: { "global": {...}, "sites": [...] }
    Sites(Vec<SiteConfig>),  // ВТОРОЙ: [{...}]
    Single(SiteConfig),      // ТРЕТИЙ:  { "port": 8080 }
}
// НЕ МЕНЯТЬ ПОРЯДОК. Single — catch-all (все поля Option<>).
```

---

### Решение 6: `static` — зарезервированное слово Rust

```rust
#[serde(rename = "static")]
pub static_files: Option<StaticConfig>,
```

В коде: `site.static_files`. В JSON: `"static"`.

---

### Решение 7: Proxy — `#[serde(untagged)]`

```rust
#[serde(untagged)]
pub enum ProxyConfig {
    Single(String),
    Routes(IndexMap<String, ProxyRouteTarget>),
}

#[serde(untagged)]
pub enum ProxyRouteTarget {
    Url(String),              // "http://upstream:4000"
    RoundRobin(Vec<String>),  // ["http://b1:4000", "http://b2:4000"]
    Full(ProxyRouteConfig),   // { "targets": [...], "strategy": "..." }
}
```

`IndexMap` — сохранение порядка (первый совпавший маршрут побеждает).
`Url`/`RoundRobin` не поддерживают опции — используй `Full`.

---

### Решение 8: Bool/object shorthand для всех опциональных блоков

Следующие поля принимают `false | true | объект`:
`logging`, `compression`, `securityHeaders`, `cors`, `hotReload`, `healthCheck`, `responseTime`.

Реализация через `#[serde(untagged)]` enum для каждого:

```rust
// Пример для compression — остальные аналогично
#[serde(untagged)]
pub enum CompressionConfig {
    Enabled(bool),                  // false / true
    Options(CompressionOptions),    // { "algorithms": [...], ... }
}

// Пример для logging — добавляем Format shorthand
#[serde(untagged)]
pub enum LoggingConfig {
    Enabled(bool),              // false / true
    Format(LogFormat),          // "dev", "json", "combined" — строка напрямую
    Options(LoggingOptions),    // { "format": "...", "file": "..." }
}
```

---

### Решение 9: Ошибки конфига через `serde_path_to_error`

```rust
let raw: ConfigFile = serde_path_to_error::deserialize(jd)
    .map_err(|e| ConfigError::Parse {
        path:    e.path().to_string(),
        message: e.inner().to_string()
    })?;
// Пример: sites[0].rateLimit.windowSecs — ожидается число, найдена строка "60s"
```

---

### Решение 10: Кэширование — только proxy ответов, через `pingora-cache`

**Кэшируем только proxy ответы** — статика имеет ETag/Last-Modified (client-side кэш).

Конфигурация живёт внутри `proxy.*` потому что кэш — это свойство upstream'а:

```jsonc
"proxy": {
  "/api": {
    "targets": ["http://backend:4000"],
    "cache": {
      "store":       "memory",        // "memory" | "redis://..." | "disk:./cache"
      "maxSizeMb":   256,
      "ttlSecs":     300,
      "varyHeaders": ["Accept-Encoding", "Accept-Language"],
      "skipPaths":   ["/api/auth/**", "/api/user/**"],
      "skipIfCookie": true,           // не кэшировать персональные данные
      "methods":     ["GET", "HEAD"]  // только идемпотентные
    }
  }
}
```

Кастомный cache key включает: host + scheme + URI path. Так исправлен CVE-2026-2836
(дефолтный ключ в Pingora 0.8 убран — нужна явная реализация).

```rust
pub struct ConduitCacheKey {
    pub host:   String,
    pub scheme: String,
    pub path:   String,
    pub query:  Option<String>,
}
// impl CacheKey for ConduitCacheKey
```

---

### Решение 11: Auto-TLS (Let's Encrypt) — через `instant-acme`

Caddy-killer feature. Реализуется в Phase 3.

```jsonc
"tls": {
  "acme": {
    "email":      "admin@example.com",
    "directory":  "https://acme-v02.api.letsencrypt.org/directory",
    "storage":    "./certs",          // где хранить cert/key
    "challenge":  "http-01"           // "http-01" | "dns-01" | "tls-alpn-01"
  }
}
// Если задан acme — cert/key не нужны, сертификат получается и обновляется автоматически
```

При наличии `tls.acme`:
- При старте: запросить/обновить сертификат через ACME
- Фоновая задача: проверять срок и обновлять за 30 дней до истечения
- `conduit validate` проверяет домен и текущий статус сертификата

---

### Решение 12: IP allow/deny

Простая фича, высокий приоритет. Применяется до auth и rate limit.

```jsonc
"ipFilter": {
  "allow": ["10.0.0.0/8", "192.168.0.0/16", "203.0.113.5"],
  "deny":  ["0.0.0.0/0"],
  "trustProxy": true    // брать IP из X-Forwarded-For если за другим прокси
}
// Если задан allow — всё остальное блокируется (whitelist mode)
// Если задан только deny — whitelist открытый (blacklist mode)
```

---

### Решение 13: Request size limits

```jsonc
"limits": {
  "maxBodyBytes":    1048576,   // 1 МБ — для API запросов
  "maxHeaderBytes":  8192,      // 8 КБ заголовки
  "timeoutSecs":     30         // общий таймаут запроса
}
// 413 Request Entity Too Large при превышении body
// 431 Request Header Fields Too Large при превышении headers
```

---

### Решение 14: Proxy retries

```jsonc
"proxy": {
  "/api": {
    "targets": ["http://b1:4000", "http://b2:4000"],
    "retry": {
      "attempts":   3,
      "conditions": ["connection_error", "5xx"],  // когда retry
      "backoffMs":  100                            // задержка между попытками
    }
  }
}
```

---

### Решение 15: Middleware chain (Rhai — Phase 4)

Порядок выполнения middleware в `request_filter` конфигурируется через `middleware` массив.
В Phase 4 добавляются Rhai скрипты как тип middleware.

```jsonc
"middleware": [
  { "type": "ipFilter",       "config": { "deny": ["10.0.0.1"] } },
  { "type": "rateLimit",      "config": { "limit": 100 } },
  { "type": "auth",           "config": { "type": "basic" } },
  { "type": "script",         "path": "./scripts/custom-auth.rhai" },
  { "type": "headers",        "config": { "X-Powered-By": "Conduit" } }
]
```

Rhai скрипт получает доступ к request context:

```rust
// custom-auth.rhai
let token = request.header("Authorization");
if token == "" {
    response.status = 401;
    response.header("WWW-Authenticate", "Bearer");
    return false;  // прервать pipeline
}
true  // продолжить
```

---

### Решение 16: Горячая перезагрузка

*Холодные* (требуют рестарта): `global.*`, `port`, `tls.*` (кроме `tls.acme`), `http2.*`

*Горячие* (через `conduit reload`):
`headers`, `redirects`, `fallback`, `rateLimit`, `basicAuth`, `apiKey`, `ipFilter`, `limits`,
`logging`, `cors`, `securityHeaders`, `responseTime`, `compression`,
`proxy.*` (включая `cache.*`), `static`, `staticOptions`, `hotReload`, `upload`,
`metrics`, `healthCheck`, `middleware`

Механизм: `Arc<ArcSwap<ReloadableConfig>>` — wait-free чтение в hot path.
При cold change: Admin API возвращает ошибку с перечнем полей.

---

### Решение 17: Log writer — атомарная смена файла

`Arc<Mutex<LogWriter>>` отдельно от конфига. flush → close → open при смене `logging.file`.

---

### Решение 18: Rate limiter

`DashMap<RateLimitKey, TokenBucket>` в `AppState`. Очистка каждые 60 сек.

```rust
pub struct AppState {
    pub config:       Arc<ArcSwap<ReloadableConfig>>,
    pub rate_limiter: Arc<DashMap<RateLimitKey, TokenBucket>>,
    pub metrics:      Arc<MetricsRegistry>,
    pub upstreams:    Arc<UpstreamRegistry>,
    pub log_writer:   Arc<Mutex<LogWriter>>,
    pub inflight:     Arc<AtomicUsize>,
    pub upload_addr:  Option<SocketAddr>,
}
```

---

### Решение 19: Graceful shutdown

`Arc<AtomicUsize>` inflight: +1 в начале `request_filter`, -1 после ответа.
SIGTERM/Admin API → перестать принимать → дождаться нуля → flush logs → exit.
Таймаут: `global.shutdownTimeoutSecs` (default: 30).

---

### Решение 20: CGI — опциональная Phase 5

Ни Nginx нативно, ни Caddy, ни Traefik. Только при явном запросе пользователей.

---

### Решение 21: Тестовая инфраструктура

- `port: 0` — OS выдаёт порт, нет конфликтов
- `rcgen` — TLS сертификат в памяти
- `reqwest` с `danger_accept_invalid_certs(true)`
- `serial_test` — для тестов Admin API
- Mock upstream — `tokio::net::TcpListener` без Axum
- `#[cfg(unix)]` / `#[cfg(windows)]`

---

## Ключевые типы (`config/schema.rs`)

```rust
pub const CONFIG_VERSION: u32 = 1;

#[serde(untagged)]
pub enum ConfigFile {
    Full(AppConfig),         // ПЕРВЫЙ
    Sites(Vec<SiteConfig>),  // ВТОРОЙ
    Single(SiteConfig),      // ТРЕТИЙ — НЕ МЕНЯТЬ ПОРЯДОК
}

pub struct GlobalConfig {
    pub workers:               Option<usize>,
    pub backlog:               Option<u32>,
    pub shutdown_timeout_secs: Option<u64>,
    pub admin:                 Option<AdminConfig>,
    pub providers:             Option<ProvidersConfig>,  // зарезервировано
}

pub struct AdminConfig {
    pub bind: Option<String>,   // default: "127.0.0.1:2019"
}

pub struct SiteConfig {
    pub host:    Option<String>,
    pub port:    Option<u16>,

    pub tls:     Option<TlsConfig>,
    pub http2:   Option<Http2Config>,

    // bool | string | object shorthand
    pub logging:          Option<LoggingConfig>,
    pub compression:      Option<CompressionConfig>,
    pub response_time:    Option<ResponseTimeConfig>,
    pub security_headers: Option<SecurityHeadersConfig>,
    pub cors:             Option<CorsConfig>,
    pub hot_reload:       Option<HotReloadConfig>,
    pub health_check:     Option<HealthCheckConfig>,

    // только объект
    pub rate_limit: Option<RateLimitConfig>,
    pub basic_auth: Option<BasicAuthConfig>,
    pub api_key:    Option<ApiKeyConfig>,
    pub ip_filter:  Option<IpFilterConfig>,
    pub limits:     Option<LimitsConfig>,
    pub headers:    Option<IndexMap<String, String>>,
    pub redirects:  Option<Vec<RedirectRule>>,
    pub middleware: Option<Vec<MiddlewareEntry>>,  // Phase 2.x / Phase 4

    #[serde(rename = "static")]
    pub static_files:   Option<StaticConfig>,
    pub static_options: Option<StaticOptions>,

    pub proxy:   Option<ProxyConfig>,   // нельзя с routes одновременно
    pub upload:  Option<UploadConfig>,
    pub metrics: Option<MetricsConfig>,
    pub fallback: Option<FallbackConfig>,

    // Phase 3.5:
    // pub routes: Option<Vec<RouteConfig>>,
    // Phase 5 (optional):
    // pub cgi: Option<CgiConfig>,
}

// Bool/object shorthand — одинаковый паттерн для всех
#[serde(untagged)]
pub enum CompressionConfig {
    Enabled(bool),
    Options(CompressionOptions),
}

#[serde(untagged)]
pub enum LoggingConfig {
    Enabled(bool),
    Format(LogFormat),       // "dev" | "json" | "combined" | ...
    Options(LoggingOptions),
}

// Аналогично: SecurityHeadersConfig, CorsConfig, HotReloadConfig,
//             HealthCheckConfig, ResponseTimeConfig

// Proxy
#[serde(untagged)]
pub enum ProxyConfig {
    Single(String),
    Routes(IndexMap<String, ProxyRouteTarget>),
}

#[serde(untagged)]
pub enum ProxyRouteTarget {
    Url(String),
    RoundRobin(Vec<String>),
    Full(ProxyRouteConfig),
}

// Shorthand: строка или массив строк — равные веса
// Full форма: массив объектов с весами (только для WeightedRoundRobin)
#[serde(untagged)]
pub enum ProxyTarget {
    Simple(String),                  // "http://b1:4000"
    Weighted(WeightedTarget),        // { "url": "http://b1:4000", "weight": 3 }
}

pub struct WeightedTarget {
    pub url:    String,
    pub weight: u32,   // default: 1
}

pub struct ProxyRouteConfig {
    pub targets:      Vec<ProxyTarget>,    // строки или weighted объекты
    pub strategy:     LoadBalanceStrategy, // default: RoundRobin
    pub http2:        bool,
    pub strip_prefix: bool,
    pub hash_key:     Option<String>,  // для IpHash/ConsistentHash: "ip" | "header:X-Key" | "url"
    pub timeout:      Option<ProxyTimeout>,
    pub health_check: Option<UpstreamHealthCheck>,
    pub pool:         Option<ConnectionPoolConfig>,
    pub cache:        Option<CacheConfig>,
    pub retry:        Option<RetryConfig>,
    // Phase 3.6: pub rewrite: Option<Vec<RewriteRule>>,
}

pub struct CacheConfig {
    pub store:          String,              // "memory" | "redis://..." | "disk:./cache"
    pub max_size_mb:    Option<u64>,
    pub ttl_secs:       Option<u64>,
    pub vary_headers:   Option<Vec<String>>,
    pub skip_paths:     Option<Vec<String>>,
    pub skip_if_cookie: Option<bool>,
    pub methods:        Option<Vec<String>>, // default: ["GET", "HEAD"]
}

pub struct RetryConfig {
    pub attempts:    u32,
    pub conditions:  Vec<String>,            // "connection_error" | "5xx" | "timeout"
    pub backoff_ms:  Option<u64>,
}

pub enum LoadBalanceStrategy {
    RoundRobin,         // по кругу, равномерно
    WeightedRoundRobin, // по кругу с весами (веса задаются в конфиге статически)
    Random,             // случайный выбор
    LeastConn,          // наименьшее число активных соединений
    LeastResponseTime,  // наименьшая latency (измеряется в фоне)
    IpHash,             // один IP → один upstream (sticky sessions)
    ConsistentHash,     // Ketama хеш (Pingora нативно) — минимальное перераспределение
}

pub struct IpFilterConfig {
    pub allow:       Option<Vec<String>>,    // CIDR или IP
    pub deny:        Option<Vec<String>>,
    pub trust_proxy: Option<bool>,
}

pub struct LimitsConfig {
    pub max_body_bytes:   Option<u64>,
    pub max_header_bytes: Option<u64>,
    pub timeout_secs:     Option<u64>,
}

pub struct FallbackConfig {
    pub status:    Option<u16>,
    pub body:      Option<serde_json::Value>,
    pub file:      Option<String>,
    pub headers:   Option<IndexMap<String, String>>,
    // Нет поля "redirect" — используй headers: { "Location": "..." } + status: 307
    pub by_accept: Option<IndexMap<String, FallbackRule>>,
}

pub struct FallbackRule {
    pub status:  Option<u16>,
    pub body:    Option<serde_json::Value>,
    pub file:    Option<String>,
    pub headers: Option<IndexMap<String, String>>,
}

pub struct UploadConfig {
    pub path:                 String,
    pub dir:                  String,
    pub max_file_size_bytes:  Option<u64>,
    pub max_total_size_bytes: Option<u64>,
    pub max_files:            Option<usize>,
    pub allowed_mime_types:   Option<Vec<String>>,
    pub field_name:           Option<String>,
}

// TLS с поддержкой ACME
pub struct TlsConfig {
    pub cert:               Option<String>,
    pub key:                Option<String>,
    pub ca:                 Option<String>,
    pub http_redirect_port: Option<u16>,
    pub versions:           Option<Vec<String>>,
    pub ciphers:            Option<Vec<String>>,
    pub acme:               Option<AcmeConfig>,  // Auto-TLS Phase 3
}

pub struct AcmeConfig {
    pub email:     String,
    pub directory: Option<String>,  // default: Let's Encrypt production
    pub storage:   Option<String>,  // default: "./certs"
    pub challenge: Option<String>,  // default: "http-01"
}

// Middleware chain (Phase 2.x конфиг, Phase 4 Rhai)
pub struct MiddlewareEntry {
    pub r#type:  String,                          // "ipFilter" | "rateLimit" | "script" | ...
    pub config:  Option<serde_json::Value>,       // зависит от type
    pub path:    Option<String>,                  // для type: "script"
}

pub enum LogFormat {
    Combined, Common, Dev, Short, Json,
}

pub struct LoggingOptions {
    pub format: Option<LogFormat>,
    pub file:   Option<String>,
}

// StaticConfig — untagged: Single(String) | Multi(Vec<String>) | Mapped(IndexMap<String, String>)
```

### `proxy/ctx.rs`

```rust
pub struct RequestCtx {
    pub site_idx:   usize,
    pub site:       Arc<SiteConfig>,
    pub upstream:   UpstreamTarget,
    pub start_time: Instant,
    pub request_id: Uuid,
    pub inflight:   Arc<AtomicUsize>,
    pub accept_enc: AcceptEncoding,   // разобран один раз в request_filter
}

pub enum UpstreamTarget {
    Proxy  { peer: SocketAddr, strip_prefix: Option<String> },
    Local  (LocalHandler),
    Upload { addr: SocketAddr },
}

pub enum LocalHandler {
    StaticFile { root: PathBuf, options: Arc<StaticOptions> },
    Health     { config: Arc<HealthCheckConfig> },
    Metrics    { token: Option<String> },
    HotReload  { config: Arc<HotReloadConfig> },
    Fallback   { config: Arc<FallbackConfig> },
}

pub struct AcceptEncoding { pub brotli: bool, pub gzip: bool, pub deflate: bool }
```

---

## Pipeline обработки запроса

### Local запрос

```
request_filter()
  ├─ inflight++
  ├─ logging: start_time
  ├─ ip_filter: 403 если deny
  ├─ cors: OPTIONS preflight
  ├─ compression: Accept-Encoding → ctx.accept_enc
  ├─ health: /__health__ → Local (минует auth!)
  ├─ metrics: /__metrics__ → Local (минует auth!)
  ├─ hot_reload: /__hot-reload__ → Local
  ├─ limits: 413/431 если превышено
  ├─ rate_limit: 429
  ├─ auth: 401
  ├─ middleware chain (фильтры из конфига)
  ├─ headers: запланировать
  ├─ redirects: первое совпадение
  ├─ router: ctx.upstream = Local(handler)
  └─ handle_local() → write_local_response() → Ok(true)
```

### Upload / Proxy запрос

```
request_filter() → те же фильтры → Ok(false)
upstream_peer()  → loopback addr (Upload) | балансировка (Proxy)
upstream_request_filter() → Host, X-Forwarded-*, strip_prefix
upstream_response_filter()
  ├─ cache: сохранить ответ если cacheable (только Proxy)
  ├─ custom headers
  ├─ compression
  ├─ X-Response-Time
  ├─ inflight--
  └─ Prometheus
```

---

## Структура проекта

```
conduit/
├── CLAUDE.md
├── Cargo.toml
├── Cargo.lock
├── README.md
├── BENCHMARKS.md
├── CHANGELOG.md
├── LICENSE                            Apache-2.0
├── Makefile
├── .github/
│   └── workflows/
│       ├── ci.yml                     fmt + clippy + test (linux/macos/windows)
│       └── release.yml                cross + crates.io + Docker Hub + GitHub Release
├── schema/
│   └── conduit.schema.json            JSON Schema (вручную синхронизируется с schema.rs)
├── examples/
│   ├── minimal.json
│   ├── spa-with-api.json
│   ├── multi-site.json
│   ├── tls-h2.json
│   ├── tls-acme.json
│   ├── load-balanced.json
│   ├── with-cache.json
│   └── dev-hot-reload.json
├── contrib/
│   ├── conduit.service                systemd unit
│   ├── Dockerfile                     multi-stage: builder + scratch
│   └── docker-compose.yml
├── src/
│   ├── main.rs
│   ├── cli/
│   │   ├── mod.rs
│   │   ├── args.rs
│   │   └── init.rs
│   ├── config/
│   │   ├── mod.rs
│   │   ├── schema.rs
│   │   ├── parse.rs
│   │   ├── validate.rs
│   │   ├── env.rs
│   │   └── defaults.rs
│   ├── server/
│   │   ├── mod.rs
│   │   ├── builder.rs
│   │   ├── tls.rs
│   │   ├── acme.rs                    Let's Encrypt (Phase 3)
│   │   └── shutdown.rs
│   ├── admin/
│   │   ├── mod.rs
│   │   └── api.rs
│   ├── upload/
│   │   ├── mod.rs
│   │   └── server.rs
│   ├── proxy/
│   │   ├── mod.rs
│   │   ├── service.rs
│   │   ├── ctx.rs
│   │   ├── router.rs
│   │   ├── upstream.rs
│   │   └── cache.rs
│   ├── handler/
│   │   ├── mod.rs
│   │   ├── response.rs
│   │   ├── static_files.rs
│   │   ├── health.rs
│   │   ├── metrics.rs
│   │   ├── hot_reload.rs
│   │   └── fallback.rs
│   ├── filter/
│   │   ├── mod.rs
│   │   ├── auth.rs
│   │   ├── compression.rs
│   │   ├── cors.rs
│   │   ├── headers.rs
│   │   ├── ip_filter.rs
│   │   ├── limits.rs
│   │   ├── logging.rs
│   │   ├── rate_limit.rs
│   │   ├── redirects.rs
│   │   ├── response_time.rs
│   │   └── security_headers.rs
│   └── util/
│       ├── mod.rs
│       ├── log_writer.rs
│       ├── mime.rs
│       ├── path.rs
│       └── net.rs
├── benches/
│   ├── static_files.rs
│   └── proxy_passthrough.rs
└── tests/
    ├── common/mod.rs
    ├── config_parse.rs
    ├── static_files.rs
    ├── proxy.rs
    ├── cache.rs
    ├── redirects.rs
    ├── auth.rs
    ├── ip_filter.rs
    ├── rate_limit.rs
    ├── tls.rs
    ├── upload.rs
    ├── hot_reload.rs
    ├── admin_api.rs
    ├── graceful_shutdown.rs
    ├── virtual_hosting.rs
    └── match_routing.rs
```

---

## CLI — полная спецификация

```
conduit [OPTIONS]                            запустить сервер
conduit init [OPTIONS]                       создать conduit.json интерактивно
conduit validate [OPTIONS]                   проверить конфиг (0 = OK)
conduit probe [OPTIONS]                      HEAD к каждому upstream
conduit fmt [OPTIONS]                        форматировать конфиг → stdout
conduit fmt --write [OPTIONS]                форматировать → перезаписать файл
conduit reload [--admin ADDR]                перезагрузить конфиг
conduit status [--admin ADDR]                статус сервера
conduit upstreams [--admin ADDR]             состояние всех upstream'ов
conduit upstreams add --route PATH           добавить upstream (только в памяти)
  --target URL [--weight N] [--admin ADDR]
conduit upstreams remove --route PATH        убрать upstream из памяти
  --target URL [--admin ADDR]
conduit upstreams weight --route PATH        изменить вес (только WRR)
  --target URL --weight N [--admin ADDR]
conduit shutdown [--admin ADDR]              graceful shutdown

OPTIONS для сервера:
  -c, --config <FILE>   [default: conduit.json]
      --version
  -h, --help

OPTIONS для управляющих команд:
      --admin <ADDR>    [default: $CONDUIT_ADMIN или 127.0.0.1:2019]

ENV:
  RUST_LOG        уровень логирования
  CONDUIT_ADMIN   адрес Admin API
```

---

## Справочник конфигурации

```jsonc
{
  "version": 1,
  "host":    "app.example.com",
  "port":    8080,

  // ── TLS ────────────────────────────────────────────────────────────────
  "tls": {
    // Вариант A: ручные сертификаты
    "cert": "./certs/cert.pem",
    "key":  "./certs/key.pem",
    "ca":   "./certs/ca.pem",
    "httpRedirectPort": 80,          // только один сайт на порт!
    "versions": ["TLSv1.3"],         // rustls-строки, НЕ OpenSSL
    "ciphers":  ["TLS_AES_256_GCM_SHA384"],

    // Вариант B: Auto-TLS (Phase 3) — cert/key не нужны
    "acme": {
      "email":     "admin@example.com",
      "storage":   "./certs",
      "challenge": "http-01"         // "http-01" | "tls-alpn-01"
    }
  },

  "http2": { "maxConcurrentStreams": 100, "initialWindowSize": 65535 },

  // false | true | "dev" | "json" | { "format": "...", "file": "..." }
  "logging": { "format": "combined", "file": "./logs/access.log" },

  // false | true | объект
  "compression": { "algorithms": ["br", "gzip"], "level": 6, "minBytes": 1024 },

  // false | true | { "digits": 3 }
  "responseTime": true,

  // false | true | объект
  "securityHeaders": true,

  // false | true | объект
  "cors": {
    "origins": ["https://app.example.com"],
    "methods": ["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"],
    "allowedHeaders": ["Content-Type", "Authorization"],
    "credentials": false,
    "maxAgeSecs": 86400
  },

  "ipFilter": {
    "allow": ["10.0.0.0/8", "192.168.0.0/16"],
    "deny":  ["1.2.3.4"],
    "trustProxy": true
  },

  "limits": {
    "maxBodyBytes":   1048576,
    "maxHeaderBytes": 8192,
    "timeoutSecs":    30
  },

  "rateLimit": {
    "windowSecs": 60, "limit": 100,
    "algorithm":  "token-bucket",
    "keyBy":      "ip",
    "skipPaths":  ["/__health__", "/__metrics__"]
  },

  "basicAuth": {
    "users": { "admin": "$ADMIN_PASSWORD" },
    "challenge": true, "realm": "My App",
    "skipPaths": ["/__health__", "/__metrics__"]
  },

  "headers": { "X-Powered-By": "Conduit" },

  "redirects": [
    { "from": "/old",        "to": "/new",                          "status": 301 },
    { "from": "/blog/:slug", "to": "/posts/:slug",                  "status": 308 }
  ],

  "static": "./dist",
  "staticOptions": {
    "etag": true, "lastModified": true, "maxAge": "1d",
    "index": ["index.html"], "dotFiles": "ignore", "preCompressed": false
  },

  // false | true | объект
  "hotReload": false,

  // ── Реверс-прокси ────────────────────────────────────────────────────────
  "proxy": "http://localhost:4000",
  // Полная форма с балансировкой:
  // "proxy": {
  //   "/api": {
  //     "targets": ["http://b1:4000", "http://b2:4000"],
  //     // Или с весами (только для weighted-round-robin):
  //     // "targets": [
  //     //   { "url": "http://b1:4000", "weight": 3 },
  //     //   { "url": "http://b2:4000", "weight": 1 }
  //     // ],
  //     // Стратегии:
  //     // "round-robin" | "weighted-round-robin" | "random"
  //     // "least-conn" | "least-response-time"
  //     // "ip-hash" | "consistent-hash"
  //     "strategy":  "round-robin",
  //     "hash_key":  "ip",         // для ip-hash / consistent-hash
  //     "http2":     false, "stripPrefix": true,
  //     "timeout":   { "connectMs": 2000, "sendMs": 10000, "readMs": 30000 },
  //     "pool":      { "maxIdle": 10, "idleTimeoutSecs": 60 },
  //     "retry":     { "attempts": 3, "conditions": ["connection_error", "5xx"] },
  //     "healthCheck": { "path": "/health", "intervalSecs": 10,
  //                      "unhealthyThreshold": 2, "healthyThreshold": 1 },
  //     "cache": {
  //       "store": "memory", "maxSizeMb": 256, "ttlSecs": 300,
  //       "varyHeaders": ["Accept-Encoding"],
  //       "skipIfCookie": true, "skipPaths": ["/api/auth/**"]
  //     }
  //   }
  // }

  "upload": {
    "path": "/upload", "dir": "./uploads",
    "maxFileSizeBytes": 10485760, "maxTotalSizeBytes": 52428800,
    "maxFiles": 5, "allowedMimeTypes": ["image/jpeg", "image/png"]
  },

  // false | true | объект
  "healthCheck": true,

  "metrics": { "path": "/__metrics__", "token": "$METRICS_TOKEN" },

  "fallback": { "status": 404, "file": "./404.html" }
}
```

### Глобальные настройки

```jsonc
{
  "global": {
    "workers": 4, "backlog": 1024, "shutdownTimeoutSecs": 30,
    "admin": { "bind": "127.0.0.1:2019" }
  },
  "sites": [{ "port": 8080, "static": "./dist" }]
}
```

---

## Примеры конфигурации

### Минимальный

```json
{ "port": 8080, "static": "./dist", "proxy": { "/api": "http://localhost:4000" } }
```

### SPA + API (production, Auto-TLS)

```json
{
  "port": 443,
  "tls": { "acme": { "email": "admin@example.com" } },
  "compression": true, "securityHeaders": true,
  "logging": { "format": "json", "file": "/var/log/conduit/access.log" },
  "static": "./dist",
  "staticOptions": { "maxAge": "7d", "preCompressed": true },
  "proxy": {
    "/api": {
      "targets": ["http://api1:4000", "http://api2:4000"],
      "strategy": "least-conn", "stripPrefix": true,
      "healthCheck": { "path": "/health", "intervalSecs": 10 },
      "cache": { "store": "memory", "ttlSecs": 60, "skipIfCookie": true }
    }
  },
  "healthCheck": true,
  "metrics": { "path": "/__metrics__", "token": "$METRICS_TOKEN" },
  "fallback": { "byAccept": {
    "html": { "status": 200, "file": "./dist/index.html" },
    "json": { "status": 404, "body": { "error": "Not Found" } },
    "*":    { "status": 200, "file": "./dist/index.html" }
  }}
}
```

### Weighted load balancing + IP hash

```json
{
  "port": 443,
  "tls": { "cert": "$CERT", "key": "$KEY" },
  "proxy": {
    "/api": {
      "targets": [
        { "url": "http://powerful:4000", "weight": 3 },
        { "url": "http://normal:4000",   "weight": 1 }
      ],
      "strategy": "weighted-round-robin"
    },
    "/auth": {
      "targets": ["http://auth1:5000", "http://auth2:5000"],
      "strategy": "ip-hash",
      "hash_key": "ip"
    }
  }
}
```

### Разработка (hot reload)

```json
{
  "port": 3000, "logging": "dev",
  "hotReload": { "extensions": [".html", ".css", ".js", ".ts"] },
  "static": "./src",
  "proxy": { "/api": "http://localhost:4000" },
  "cors": true,
  "fallback": { "status": 200, "file": "./src/index.html" }
}
```

### Multi-site (виртуальный хостинг)

```json
[
  {
    "host": "app.example.com", "port": 443,
    "tls": { "acme": { "email": "admin@example.com" }, "httpRedirectPort": 80 },
    "static": "./dist", "proxy": { "/api": "http://localhost:4000" }
  },
  {
    "host": "admin.example.com", "port": 443,
    "tls": { "cert": "$CERT", "key": "$KEY" },
    "basicAuth": { "users": { "admin": "$ADMIN_PASS" }, "challenge": true },
    "static": "./admin-ui"
  },
  { "host": "*", "port": 443, "tls": { "cert": "$CERT", "key": "$KEY" },
    "fallback": { "status": 404, "body": "Unknown host" } }
]
```

---

## Целевые показатели производительности

| Метрика | express-reverse-proxy | Цель Conduit |
|---|---|---|
| Статика 1 КБ | ~8 тыс. req/s | **≥ 150 тыс. req/s** |
| Прокси | ~6 тыс. req/s | **≥ 80 тыс. req/s** |
| P99 задержка прокси | ~15 мс | **≤ 2 мс** |
| Память | ~60 МБ | **≤ 10 МБ** |
| Время старта | ~500 мс | **≤ 50 мс** |
| Бинарник (musl, stripped) | N/A | **≤ 15 МБ** |

*Бинарник увеличился с 12 до 15 МБ из-за добавления `pingora-cache` и `instant-acme`.*

---

## Зависимости (`Cargo.toml`)

```toml
[dependencies]
pingora                = "0.8"
pingora-core           = "0.8"
pingora-proxy          = "0.8"
pingora-load-balancing = "0.8"
pingora-cache          = "0.8"

clap         = { version = "4", features = ["derive"] }
serde        = { version = "1", features = ["derive"] }
serde_json   = "1"
serde_path_to_error = "0.1"
indexmap     = { version = "2", features = ["serde"] }
humantime    = "2"

tokio = { version = "1", features = ["full"] }
axum  = "0.8"

async-compression = { version = "0.4", features = ["gzip", "brotli", "deflate", "futures-io"] }

tracing            = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }

prometheus = "0.13"
arc-swap   = "1"
dashmap    = "6"
mime_guess = "2"
notify     = "7"
regex      = "1"

multer = "3"
uuid   = { version = "1", features = ["v4"] }

# Auto-TLS (Phase 3)
instant-acme = "0.7"
rcgen        = "0.13"

# Rhai скрипты (Phase 4)
# rhai = "1"

dialoguer = "0.11"
indicatif = "0.17"

anyhow    = "1"
thiserror = "2"
bytes     = "1"

[dev-dependencies]
reqwest     = { version = "0.12", features = ["http2", "rustls-tls"] }
criterion   = { version = "0.5", features = ["html_reports"] }
tempfile    = "3"
serial_test = "3"
```

---

## Стратегия публикации

1. **crates.io** — `cargo publish` при пуше тега `v*`
2. **GitHub Releases** — через `cross`:
   - `x86_64-unknown-linux-gnu`
   - `x86_64-unknown-linux-musl` ← для Docker FROM scratch
   - `aarch64-unknown-linux-gnu`
   - `x86_64-apple-darwin`
   - `aarch64-apple-darwin`
   - `x86_64-pc-windows-msvc`
3. **Docker Hub** — `lopatnov/conduit:latest` (musl + scratch)
4. **npm-обёртка** (Phase 3.9) — `npx conduit`

---

## Фазы разработки

Принцип: **от простого к сложному**. Каждая фаза = работающий артефакт.

---

### Фаза 1.1 — Cargo.toml + структура проекта ✅ 2026-05-23

- [x] `Cargo.toml` с зафиксированными версиями (`dashmap = "6"`, v7 RC)
- [x] Пустые модули с `mod.rs`
- [x] `src/main.rs` — точка входа, печатает версию
- [x] CI: fmt + clippy + test matrix (linux/macos/windows)

**Артефакт:** компилируется, печатает `conduit 0.0.1`. ✅

---

### Фаза 1.2 — Конфиг: парсинг и схема ✅ 2026-05-23

- [x] `src/config/schema.rs` — все типы:
      - `#[serde(rename = "static")]`
      - порядок `ConfigFile`: Full → Sites → Single (Single — `Box<SiteConfig>`)
      - bool/object shorthand enums для всех полей
      - `FallbackConfig` + `FallbackRule`
      - `LoadBalanceStrategy` enum (7 вариантов, kebab-case)
      - `ProxyTarget` + `WeightedTarget`
      - `CacheConfig`, `RetryConfig`, `IpFilterConfig`, `LimitsConfig`
- [x] `src/config/parse.rs` — `VersionProbe`, `normalize()`, `load_config()`, `from_str()`
- [x] `src/config/env.rs` — `$VAR` интерполяция
- [x] `src/config/defaults.rs` — семантические константы (impl Default через derive)
- [x] `tests/config_parse.rs` — 41 тест, все зелёные

**Артефакт:** парсинг конфига работает. ✅

---

### Фаза 1.3 — Конфиг: валидация и CLI ✅ 2026-05-23

- [x] `src/config/validate.rs`:
      - дублирующие host+port
      - несколько `httpRedirectPort` на порт
      - TLS: cert/key или acme, но не оба
      - `WeightedRoundRobin` требует `WeightedTarget` (не строки)
      - пустой `targets` — ошибка
      - невалидный redirect status — ошибка
      - rateLimit.windowSecs/limit == 0 — ошибка
      - 18 unit tests, все зелёные
- [x] `src/cli/args.rs` — все subcommands включая `upstreams add/remove/weight`
- [x] `conduit validate` — вывод ошибок, exit code 1 при ошибках
- [x] `conduit fmt` + `conduit fmt --write`
- [x] `schema/conduit.schema.json`
- [x] `examples/minimal.json`, `examples/spa-with-api.json`

**Артефакт:** `conduit validate` и `conduit fmt` работают. ✅

---

### Фаза 1.4 — HTTP/1.1 + health + Admin API ✅ 2026-05-23

- [x] `src/proxy/ctx.rs` — `RequestCtx`, `UpstreamTarget`, `LocalHandler`, `AcceptEncoding`
- [x] `src/handler/response.rs` — `write_response()` helper
- [x] `src/handler/health.rs`, `src/handler/fallback.rs`
- [x] `src/proxy/service.rs` — `AppState` + `ConduitProxy impl ProxyHttp`
- [x] `src/proxy/router.rs` — host matching + path routing
- [x] `src/server/builder.rs` — Pingora bootstrap, proxy service + admin background service
- [x] `src/admin/api.rs` (Axum BackgroundService):
      - `GET /status`, `POST /reload` (stub), `POST /shutdown` (graceful, respects inflight)
      - `GET /upstreams`, `POST /upstreams/add|remove|weight` (stubs)
- [x] `src/filter/logging.rs`, `src/filter/headers.rs` (stubs for Phase 2.x)
- [x] `src/util/log_writer.rs` (stub for Phase 2.4)
- [x] `src/server/shutdown.rs` (stub; full graceful shutdown Phase 2.7)
- [x] `conduit reload/status/shutdown/upstreams` subcommands (raw TCP HTTP client)
- [x] `tests/common/mod.rs` — subprocess-based test helper
- [x] Integration tests: health 200, 404 fallback, Admin API /status — 3 tests green
- Note: Admin API uses `BackgroundService` (Pingora-managed). Config must use full
  `{"global":...,"sites":[...]}` form to honour `global.admin.bind`.

**Артефакт:** минимальный HTTP сервер. ✅

---

### Фаза 1.5 — Статические файлы ✅ 2026-05-23

- [x] `src/handler/static_files.rs` — ETag, Last-Modified, Cache-Control, Range, dotfiles
- [x] Все три формы `static`
- [x] `src/util/mime.rs`
- [x] Integration tests: 200, 304, 404, 206

**Артефакт:** заменяет `python -m http.server`. ✅

---

### Фаза 1.6 — IP фильтрация + лимиты запроса ✅ 2026-05-23

- [x] `src/filter/ip_filter.rs` — CIDR matching (IPv4/IPv6/mapped), X-Forwarded-For, 403
- [x] `src/filter/limits.rs` — maxBodyBytes (413), maxHeaderBytes (431); timeout deferred
- [x] Integration tests: IP block, whitelist, 413, 431, health exempt

---

### Фаза 1.7 — Реверс-прокси ✅ 2026-05-23

- [x] `src/proxy/upstream.rs` — URL parsing, round-robin, все формы ProxyConfig
- [x] `proxy.*.stripPrefix` — path rewriting in `upstream_request_filter`
- [x] X-Forwarded-For, X-Forwarded-Proto
- [x] `src/filter/compression.rs`, `src/filter/response_time.rs` (stubs)
- [x] Integration tests: passthrough, path mapping, round-robin, stripPrefix, XFF, health bypass
- Note: `proxy.*.timeout` and `pool` deferred to Phase 1.8/2.5

**Артефакт:** заменяет express-reverse-proxy. ✅

---

### Фаза 1.8 — Proxy retries ✅ 2026-05-23

- [x] `proxy.*.retry` — attempts, conditions, backoff
- [x] Integration tests: retry on 5xx, retry on connection_error

---

### Фаза 1.9 — TLS + HTTP/2 ✅ 2026-05-23

- [x] `src/server/tls.rs` — rustls, ALPN
- [x] `tls.httpRedirectPort` — HTTP→HTTPS redirect proxy service
- [x] `http2.*` — ALPN H2 negotiation via `enable_h2()`
- [x] Integration tests: TLS, H2, HTTP→HTTPS
- Note: `tls.versions`/`tls.ciphers` deferred (rustls manages cipher suites internally).
  `conduit validate` cert expiry check deferred to Phase 3.1 (ACME integration).
- Fix: reqwest dev-dep needs `default-features = false` on Windows (native-tls has no ALPN).

**Артефакт: `conduit 0.1.0`** — crates.io + GitHub Releases.

---

### Фаза 1.10 — `conduit init` + `conduit probe` ✅ 2026-05-23

- [x] `src/cli/init.rs` — dialoguer wizard (port, static, proxy, TLS, health, logging)
- [x] `conduit probe` — HEAD к upstream, latency; indicatif progress bar
- [x] Все примеры: multi-site, tls-h2, tls-acme, load-balanced, dev-hot-reload, with-cache

---

### Фаза 2.1 — Редиректы + виртуальный хостинг

- [ ] `:param` захват, 301/302/307/308
- [ ] Host matching, catch-all `*`
- [ ] Валидация дублирующих host+port

---

### Фаза 2.2 — Аутентификация + Rate limiting

- [ ] Basic Auth + API key + skipPaths
- [ ] Token bucket, DashMap, IP/header key
- [ ] Очистка DashMap каждые 60 сек

---

### Фаза 2.3 — CORS + Security headers

- [ ] Preflight OPTIONS + CORS
- [ ] HSTS, CSP, X-Frame-Options, Referrer-Policy

---

### Фаза 2.4 — Prometheus метрики

- [ ] `LocalHandler::Metrics` → Prometheus text format
- [ ] `conduit_requests_total`, `conduit_request_duration_seconds`
- [ ] `conduit_upstream_health`, `conduit_upstream_inflight`
- [ ] `conduit_cache_hits_total`, `conduit_cache_misses_total` (заглушки до Phase 2.6)
- [ ] `metrics.token`, JSON логи + LogWriter

**Артефакт: `conduit 0.2.0`**

---

### Фаза 2.5 — Upstream health + базовая балансировка

- [ ] Фоновый Tokio task для health check на каждую группу upstream
- [ ] unhealthyThreshold / healthyThreshold
- [ ] `healthCheck.includeUpstreams` в `/__health__`
- [ ] `strategy: "least-conn"` — AtomicUsize inflight на upstream
- [ ] `strategy: "random"`
- [ ] `proxy.*.http2: true`
- [ ] `GET /upstreams` в Admin API — live/down/latency
- [ ] `conduit upstreams` subcommand

---

### Фаза 2.5b — Расширенные стратегии балансировки

- [ ] `strategy: "weighted-round-robin"` — `ProxyTarget::Weighted { url, weight }`
- [ ] `strategy: "ip-hash"` — hash по IP клиента, `hash_key: "ip"`
- [ ] `strategy: "consistent-hash"` — Ketama (Pingora нативно), `hash_key: "ip" | "header:X-Key" | "url"`
- [ ] `strategy: "least-response-time"` — фоновый замер latency
- [ ] `conduit upstreams` показывает стратегию + веса + latency
- [ ] Integration tests: weighted distribution, ip-hash sticky, consistent-hash rebalance

---

### Фаза 2.5c — Динамическое управление upstream'ами

*(Масштабирование без рестарта — только в памяти)*

- [ ] `POST /upstreams/add` — `{ "route": "/api", "target": "http://b3:4000", "weight": 1 }`
- [ ] `POST /upstreams/remove` — `{ "route": "/api", "target": "http://b1:4000" }`
- [ ] `POST /upstreams/weight` — `{ "route": "/api", "target": "http://b1:4000", "weight": 5 }`
- [ ] `conduit upstreams add/remove/weight` subcommands
- [ ] `UpstreamRegistry` хранит runtime upstream'ы отдельно от конфига
- [ ] `conduit reload` сбрасывает runtime, берёт конфиг из файла
- [ ] Integration tests: add → трафик идёт на новый, remove → не идёт

---

### Фаза 2.6 — Proxy кэш

- [ ] `src/proxy/cache.rs` — `ConduitCacheKey` (host + scheme + path + query)
- [ ] `pingora-cache` интеграция в `upstream_response_filter`
- [ ] In-memory store
- [ ] `cache.skipIfCookie`, `cache.skipPaths`, `cache.methods`, `cache.varyHeaders`
- [ ] `conduit_cache_hits_total`, `conduit_cache_misses_total` метрики
- [ ] Integration tests: cache hit, cache miss, skipIfCookie

---

### Фаза 2.7 — Горячая перезагрузка конфига

- [ ] `ArcSwap<ReloadableConfig>`
- [ ] `POST /reload` — hot/cold разделение + ArcSwap::store()
- [ ] LogWriter смена, rate limiter reset, cache flush при изменении `cache.*`
- [ ] Integration tests: reload без рестарта

---

### Фаза 3.1 — Auto-TLS (Let's Encrypt)

- [ ] `src/server/acme.rs` — `instant-acme` + `rcgen`
- [ ] HTTP-01 challenge handler
- [ ] Хранение cert/key на диске
- [ ] Фоновое обновление за 30 дней до истечения
- [ ] `conduit validate` — статус ACME сертификата
- [ ] `examples/tls-acme.json`

---

### Фаза 3.2 — Загрузка файлов (Axum loopback)

- [ ] `src/upload/server.rs` — Axum на `127.0.0.1:0`
- [ ] UUID filename + sanitized_ext, `maxTotalSizeBytes`, форма массива
- [ ] Integration tests: upload, download, 413, 403

---

### Фаза 3.3 — Hot reload браузера

- [ ] SSE + notify watcher + debounce
- [ ] `/__hot-reload__/client.js`

---

### Фаза 3.4 — Pre-compressed статика

- [ ] `staticOptions.preCompressed: true` — `.br` → `.gz` → on-the-fly

---

### Фаза 3.5 — SNI + WebSocket

- [ ] Multi-cert TLS через SNI
- [ ] WebSocket proxying

---

### Фаза 3.6 — Расширенный роутинг (`routes`)

```json
"routes": [
  { "match": { "path": "/api/**", "method": ["POST", "PUT"] }, "proxy": "http://api:4000" },
  { "match": { "path": "/public/**" }, "static": "./public" }
]
```

- [ ] `MatchConfig`: glob path, method, headers, query
- [ ] Backward compat: top-level `proxy`/`static` → routes при нормализации

---

### Фаза 3.7 — Path rewrite

- [ ] `rewrite: Vec<RewriteRule>` — regex from/to

---

### Фаза 3.7b — Группы upstream'ов

```jsonc
"proxy": {
  "/api": {
    "groups": [
      { "name": "group-a", "targets": ["http://b1:4000", "http://b2:4000"], "strategy": "least-conn" },
      { "name": "group-b", "targets": ["http://b3:4000", "http://b4:4000"], "strategy": "least-conn" }
    ],
    "groupStrategy": "ip-hash"
  }
}
```

- [ ] `GroupedProxyConfig` + `groupStrategy`
- [ ] `UpstreamGroup` в `UpstreamRegistry`
- [ ] Admin API: `POST /upstreams/add` с полем `group`
- [ ] Integration tests: ip-hash между группами, least-conn внутри

---

### Фаза 3.8 — Кэш: Redis + disk store

- [ ] `cache.store: "redis://..."` — Redis backend
- [ ] `cache.store: "disk:./cache"` — disk backend

---

### Фаза 3.9 — DX polish

- [ ] Shell completions (`clap_complete`)
- [ ] Man page (`clap_mangen`)
- [ ] `contrib/Dockerfile` + `contrib/docker-compose.yml`
- [ ] npm wrapper `npx conduit`

**Артефакт: `conduit 0.3.0`**

---

### Фаза 4.1 — Rhai middleware

- [ ] Добавить `rhai = "1"` в зависимости
- [ ] `MiddlewareEntry { type: "script", path: "..." }`
- [ ] Rhai context: `request.header()`, `request.path`, `response.status`, `response.header()`
- [ ] Integration tests: скрипт блокирует запрос, скрипт добавляет заголовок

---

### Фаза 4.2 — Redis rate limit

- [ ] `rateLimit.store: "redis://..."` + graceful degradation в memory

---

### Фаза 5 (опционально) — CGI

Только при явном запросе пользователей.

---

### Фаза 6 — HTTP/3

**Триггер:** слияние Pingora Issue #95.

**Артефакт: `conduit 1.0.0`**

---

## Заметки для Claude (сохраняются между сессиями)

**Проект:** `C:\projects\conduit`

### Архитектурные решения

1. **Обработка запросов:** статика/health/metrics/hot-reload/fallback → Pingora напрямую.
   Upload → Axum loopback `127.0.0.1:0`. Admin API → Axum порт 2019. **Р.1**

2. **Upload:** стартует только если `upload` в конфиге. Порт не конфигурируется. **Р.2**

3. **CLI:** только subcommands. `conduit init/validate/probe/fmt/reload/status/upstreams/shutdown`.
   `upstreams add/remove/weight` — только в памяти. **Р.3**

4. **`version`** — `VersionProbe` до полного парсинга. **Р.4**

5. **`ConfigFile` enum:** Full → Sites → Single (НЕ МЕНЯТЬ ПОРЯДОК). **Р.5**

6. **`static` поле:** `#[serde(rename = "static")]`. В коде: `static_files`. **Р.6**

7. **`ProxyConfig` untagged:** `Single` → `Routes(IndexMap)`.
   `ProxyRouteTarget`: `Url` → `RoundRobin` → `Full`. **Р.7**

8. **Bool/object shorthand:** `logging`, `compression`, `securityHeaders`, `cors`,
   `hotReload`, `healthCheck`, `responseTime` — через `#[serde(untagged)]` enum.
   `logging` дополнительно принимает строку `LogFormat` напрямую. **Р.8**

9. **`serde_path_to_error`** — единственный способ парсинга. **Р.9**

10. **Кэш** — только proxy ответы, конфиг внутри `proxy.*`.
    `ConduitCacheKey` = host + scheme + path + query. CVE-2026-2836 исправлен. **Р.10**

11. **Auto-TLS** — `tls.acme`, `instant-acme` crate, Phase 3.1. **Р.11**

12. **IP filter** — CIDR, Phase 1.6. **Р.12**

13. **Limits** — maxBodyBytes/maxHeaderBytes/timeoutSecs, Phase 1.6. **Р.13**

14. **Proxy retries** — Phase 1.8. **Р.14**

15. **Rhai** — Phase 4.1. `rhai` crate закомментирован до тех пор. **Р.15**

16. **Hot/cold reload:** port, tls.cert/key/versions/ciphers, workers, backlog, admin — cold.
    `tls.acme` параметры — горячие. **Р.16**

17. **`LogWriter`** — `Arc<Mutex<LogWriter>>` отдельно от конфига. **Р.17**

18. **Rate limiter** — `DashMap` v6. **Р.18**

19. **Graceful shutdown** — `Arc<AtomicUsize>` inflight. **Р.19**

20. **`FallbackConfig`:** нет поля `redirect`. `FallbackRule` определена явно. **Р.20**

21. **`LoadBalanceStrategy`** — 7 вариантов: `RoundRobin | WeightedRoundRobin | Random |
    LeastConn | LeastResponseTime | IpHash | ConsistentHash`.
    Веса статические в конфиге через `ProxyTarget::Weighted { url, weight }`.
    Для IpHash/ConsistentHash — `hash_key: "ip" | "header:X-Key" | "url"`. Phase 2.5b. **Р.21**

22. **Динамические upstream'ы** — только в памяти. `UpstreamRegistry` отдельно от конфига.
    `conduit reload` сбрасывает runtime. Admin API: `POST /upstreams/add|remove|weight`. Phase 2.5c. **Р.22**

23. **Группы upstream'ов** — `groups` + `groupStrategy`. Фаза 3.7b. **Р.23**

24. **`AcceptEncoding`** — разбирается один раз в `request_filter`, хранится в `ctx`. **Р.24**

25. **CGI** — опциональная Phase 5. **Р.25**

26. **Тесты** — port 0, rcgen, serial_test для Admin API, mock = `TcpListener` без Axum. **Р.26**

27. **Docker service discovery** — не нужен. `contrib/docker-compose.yml` достаточно. **Р.27**

### Прочие правила

- `pingora-cache = "0.8"` — кастомный cache key обязателен (CVE-2026-2836)
- Pingora `"0.8"` — 3 CVE исправлено, только 0.8+
- `schema/conduit.schema.json` — вручную синхронизировать со `schema.rs`
- HTTP/3 только после Pingora Issue #95
- `src/main.rs` тонкий: CLI → load_config → server::run()
- `tls.ciphers` — rustls-строки, НЕ OpenSSL
- Admin API bind — только loopback
- `hotReload` при `static` как IndexMap — следить за ВСЕМИ директориями
- Phase 3.6: `routes` backward-compatible с top-level `proxy`/`static`
- tracing spans в hot path — только `Level::TRACE`
- Бинарник ≤15 МБ (вырос из-за `pingora-cache` + `instant-acme`)
- `WeightedRoundRobin` валидация: targets должны быть `WeightedTarget`, не строки
