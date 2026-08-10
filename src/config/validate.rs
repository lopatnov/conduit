use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use url::Url as ParsedUrl;

use crate::config::schema::{
    ApiKeyConfig, AppConfig, Consumer, ConsumerJwtConfig, ConsumersSharedJwtConfig, FallbackConfig,
    IpFilterConfig, LoadBalanceStrategy, MetricsConfig, MiddlewareEntry, ProxyConfig,
    ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RateLimitConfig, RedirectRule, RewriteRule,
    SiteConfig, TcpConfig, TlsClientAuth, TlsConfig, UploadConfig,
};

// ── Public API ─────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Clone, serde::Serialize)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}

impl ValidationError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

pub fn validate(config: &AppConfig) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    validate_no_duplicate_host_port(config, &mut errors);
    validate_http_redirect_ports(config, &mut errors);

    for (i, site) in config.sites.iter().enumerate() {
        validate_site(site, &format!("sites[{i}]"), &mut errors);
    }

    errors
}

/// Return human-readable warnings for config options that require a compile-time
/// feature which is not currently enabled.
///
/// The server still starts — all warnings describe things that will be silently
/// ignored at runtime.  Callers should log each entry with `tracing::warn!`.
///
/// ```text
/// for w in feature_warnings(&config) {
///     tracing::warn!("{w}");
/// }
/// ```
pub fn feature_warnings(config: &AppConfig) -> Vec<String> {
    let mut warnings: Vec<String> = Vec::new();

    check_global_feature_warnings(config, &mut warnings);
    check_per_site_feature_warnings(config, &mut warnings);
    check_proxy_loop_warnings(config, &mut warnings);
    check_jwt_secret_warnings(config, &mut warnings);
    check_metrics_auth_warnings(config, &mut warnings);

    warnings
}

/// Check warnings for global-level feature flags (e.g. OTLP).
fn check_global_feature_warnings(config: &AppConfig, warnings: &mut Vec<String>) {
    // ── global.otlp ───────────────────────────────────────────────────────────
    #[cfg(not(feature = "otlp"))]
    if config
        .global
        .as_ref()
        .and_then(|g| g.otlp.as_ref())
        .is_some()
    {
        warnings.push(
            "global.otlp is configured but Conduit was compiled without the `otlp` feature \
             — OpenTelemetry tracing will be disabled. \
             Recompile with `--features otlp` to enable."
                .to_owned(),
        );
    }
    #[cfg(feature = "otlp")]
    let _ = (config, &warnings);
}

/// Check per-site feature-gated option warnings.
fn check_per_site_feature_warnings(config: &AppConfig, warnings: &mut Vec<String>) {
    for (i, site) in config.sites.iter().enumerate() {
        check_site_middleware_feature_warnings(i, site, warnings);
        check_site_simple_feature_warnings(i, site, warnings);
    }
}

/// Check middleware-level feature warnings (wasm, rhai) for a single site.
fn check_site_middleware_feature_warnings(i: usize, site: &SiteConfig, warnings: &mut Vec<String>) {
    let Some(middleware) = &site.middleware else {
        return;
    };
    for (j, entry) in middleware.iter().enumerate() {
        // ── middleware type: "wasm" ───────────────────────────────────────────
        #[cfg(not(feature = "wasm"))]
        if entry.r#type == "wasm" {
            warnings.push(format!(
                "sites[{i}].middleware[{j}] has type \"wasm\" but Conduit was compiled \
                 without the `wasm` feature — this middleware entry will be ignored. \
                 Recompile with `--features wasm` to enable."
            ));
        }
        // ── Rhai scripting (feature: rhai) ────────────────────────────────────
        #[cfg(not(feature = "rhai"))]
        if entry.r#type == "script" {
            warnings.push(format!(
                "sites[{i}].middleware[{j}] has type \"script\" but Conduit was compiled \
                 without the `rhai` feature — this entry will be ignored. \
                 Recompile with `--features rhai` to enable."
            ));
        }
        #[cfg(all(feature = "wasm", feature = "rhai"))]
        let _ = (i, j, entry, &warnings);
    }
}

/// Check simple (non-middleware) per-site feature warnings.
fn check_site_simple_feature_warnings(i: usize, site: &SiteConfig, warnings: &mut Vec<String>) {
    // ── JWT authentication (feature: jwt) ────────────────────────────────────
    #[cfg(not(feature = "jwt"))]
    if site.jwt_auth.is_some() {
        warnings.push(format!(
            "sites[{i}].jwtAuth is configured but Conduit was compiled without the `jwt` \
             feature — JWT authentication will be disabled. \
             Recompile with `--features jwt` to enable."
        ));
    }

    // ── ForwardAuth (feature: forward-auth) ──────────────────────────────────
    #[cfg(not(feature = "forward-auth"))]
    if site.forward_auth.is_some() {
        warnings.push(format!(
            "sites[{i}].forwardAuth is configured but Conduit was compiled without the \
             `forward-auth` feature — ForwardAuth will be disabled. \
             Recompile with `--features forward-auth` to enable."
        ));
    }

    // ── ACME / auto-TLS (feature: acme) ──────────────────────────────────────
    #[cfg(not(feature = "acme"))]
    if site.tls.as_ref().and_then(|t| t.acme.as_ref()).is_some() {
        warnings.push(format!(
            "sites[{i}].tls.acme is configured but Conduit was compiled without the `acme` \
             feature — automatic TLS certificate provisioning will be disabled. \
             Recompile with `--features acme` to enable."
        ));
    }

    // ── TCP proxy (feature: tcp) ──────────────────────────────────────────────
    #[cfg(not(feature = "tcp"))]
    if site.tcp.is_some() {
        warnings.push(format!(
            "sites[{i}].tcp is configured but Conduit was compiled without the `tcp` \
             feature — TCP proxy mode will be disabled. \
             Recompile with `--features tcp` to enable."
        ));
    }

    // ── Redis (feature: redis) ────────────────────────────────────────────────
    #[cfg(not(feature = "redis"))]
    {
        let uses_redis = site
            .rate_limit
            .as_ref()
            .and_then(|rl| rl.store.as_deref())
            .map(|s| s.starts_with("redis://") || s.starts_with("rediss://"))
            .unwrap_or(false);
        if uses_redis {
            warnings.push(format!(
                "sites[{i}].rateLimit.store uses Redis but Conduit was compiled without the \
                 `redis` feature — falling back to in-memory rate limiting. \
                 Recompile with `--features redis` to enable."
            ));
        }
    }

    // ── Cache (feature: cache) ────────────────────────────────────────────────
    #[cfg(not(feature = "cache"))]
    {
        let has_cache = site_has_cache_config(site);
        if has_cache {
            warnings.push(format!(
                "sites[{i}] has proxy routes with cache configured but Conduit was compiled \
                 without the `cache` feature — response caching will be disabled. \
                 Recompile with `--features cache` to enable."
            ));
        }
    }

    // ── Upload (feature: upload) ──────────────────────────────────────────────
    #[cfg(not(feature = "upload"))]
    if site.upload.is_some() {
        warnings.push(format!(
            "sites[{i}].upload is configured but Conduit was compiled without the `upload` \
             feature — file upload will be disabled. \
             Recompile with `--features upload` to enable."
        ));
    }

    // ── Fault injection (feature: fault-injection) ────────────────────────────
    #[cfg(not(feature = "fault-injection"))]
    if site.fault_injection.is_some() {
        warnings.push(format!(
            "sites[{i}].faultInjection is configured but Conduit was compiled without the \
             `fault-injection` feature — fault injection will be disabled. \
             Recompile with `--features fault-injection` to enable."
        ));
    }

    // Suppress unused-variable warning when all per-site features are enabled.
    #[cfg(all(
        feature = "jwt",
        feature = "forward-auth",
        feature = "acme",
        feature = "tcp",
        feature = "redis",
        feature = "cache",
        feature = "upload",
        feature = "fault-injection"
    ))]
    let _ = (i, site, warnings);
}

/// Return `true` when any proxy route in the site has a `cache` config block.
#[cfg(not(feature = "cache"))]
fn site_has_cache_config(site: &SiteConfig) -> bool {
    match &site.proxy {
        Some(crate::config::schema::ProxyConfig::Routes(routes)) => routes.values().any(|t| {
            matches!(
                t,
                crate::config::schema::ProxyRouteTarget::Full(cfg) if cfg.cache.is_some()
            )
        }),
        _ => false,
    }
}

/// Warn when a proxy target points back to a port Conduit itself is listening on.
fn check_proxy_loop_warnings(config: &AppConfig, warnings: &mut Vec<String>) {
    let listening_ports: Vec<u16> = config.sites.iter().map(effective_port).collect();
    for (i, site) in config.sites.iter().enumerate() {
        let targets = collect_proxy_targets(site);
        for target in &targets {
            if let Some(port) = loopback_port(target) {
                if listening_ports.contains(&port) {
                    warnings.push(format!(
                        "sites[{i}] proxies to '{target}' which appears to point back to \
                         Conduit itself (loopback + port {port} is a configured listening port) \
                         — this will create an infinite request loop."
                    ));
                }
            }
        }
    }
}

/// Warn when JWT HMAC secrets are shorter than the 32-byte minimum.
fn check_jwt_secret_warnings(config: &AppConfig, warnings: &mut Vec<String>) {
    for (i, site) in config.sites.iter().enumerate() {
        if let Some(jwt) = &site.jwt_auth {
            if let Some(secret) = &jwt.secret {
                if secret.len() < 32 {
                    warnings.push(format!(
                        "sites[{i}].jwtAuth.secret is only {} bytes — minimum recommended \
                         length is 32 bytes for HS256.  A short secret can be brute-forced. \
                         Use a cryptographically random secret of at least 32 bytes.",
                        secret.len()
                    ));
                }
            }
        }
        check_consumer_jwt_secret_warnings(i, site, warnings);
    }
}

/// Warn when consumer-level JWT secrets are too short.
fn check_consumer_jwt_secret_warnings(i: usize, site: &SiteConfig, warnings: &mut Vec<String>) {
    let Some(consumers_cfg) = &site.consumers else {
        return;
    };
    for (j, consumer) in consumers_cfg.consumers.iter().enumerate() {
        if let Some(jwt) = &consumer.jwt {
            if let Some(secret) = &jwt.secret {
                if secret.len() < 32 {
                    warnings.push(format!(
                        "sites[{i}].consumers.consumers[{j}].jwt.secret is only {} bytes \
                         — minimum recommended length is 32 bytes.",
                        secret.len()
                    ));
                }
            }
        }
    }
}

/// Warn when a metrics endpoint has no auth token configured.
fn check_metrics_auth_warnings(config: &AppConfig, warnings: &mut Vec<String>) {
    for (i, site) in config.sites.iter().enumerate() {
        if let Some(metrics) = &site.metrics {
            if metrics.token.is_none() {
                warnings.push(format!(
                    "sites[{i}].metrics is configured without a token — the \
                     /__metrics__ endpoint is publicly accessible. \
                     Set metrics.token to require Bearer authentication in production."
                ));
            }
        }
    }
}

// ── Proxy-loop helpers ─────────────────────────────────────────────────────

/// Extract all URLs from a single `ProxyRouteTarget` into `out`.
fn collect_route_target_urls(target: &ProxyRouteTarget, out: &mut Vec<String>) {
    match target {
        ProxyRouteTarget::Url(u) => out.push(u.clone()),
        ProxyRouteTarget::RoundRobin(urls) => out.extend(urls.iter().cloned()),
        ProxyRouteTarget::Full(cfg) => {
            for t in &cfg.targets {
                let url = match t {
                    crate::config::schema::ProxyTarget::Simple(u) => u.clone(),
                    crate::config::schema::ProxyTarget::Weighted(w) => w.url.clone(),
                };
                out.push(url);
            }
        }
    }
}

/// Collect every proxy upstream URL configured for a site (from all proxy modes).
fn collect_proxy_targets(site: &SiteConfig) -> Vec<String> {
    use crate::config::schema::ProxyConfig;
    let mut out = Vec::new();

    if let Some(proxy) = &site.proxy {
        match proxy {
            ProxyConfig::Single(url) => out.push(url.clone()),
            ProxyConfig::Routes(routes) => {
                for target in routes.values() {
                    collect_route_target_urls(target, &mut out);
                }
            }
        }
    }

    // Also check routes[] array targets (same type: ProxyRouteTarget).
    if let Some(routes) = &site.routes {
        for route in routes {
            if let Some(target) = &route.proxy {
                collect_route_target_urls(target, &mut out);
            }
        }
    }
    out
}

/// If `url` has a loopback host (127.x.x.x, ::1, localhost), return its port.
fn loopback_port(url: &str) -> Option<u16> {
    let parsed = ParsedUrl::parse(url).ok()?;
    let host = parsed.host_str()?;
    let is_loopback = host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host == "[::1]"
        || host.starts_with("127.");
    if is_loopback {
        parsed.port().or_else(|| match parsed.scheme() {
            "https" => Some(443),
            _ => Some(80),
        })
    } else {
        None
    }
}

// ── Cross-site checks ──────────────────────────────────────────────────────

fn effective_port(site: &SiteConfig) -> u16 {
    site.port
        .unwrap_or(if site.tls.is_some() { 443 } else { 80 })
}

fn check_tcp_site_port_conflicts(
    i: usize,
    port: u16,
    tcp_ports: &mut HashMap<u16, usize>,
    seen: &HashMap<(String, u16), usize>,
    errors: &mut Vec<ValidationError>,
) {
    if let Some(prev) = tcp_ports.insert(port, i) {
        errors.push(ValidationError::new(
            format!("sites[{i}].port"),
            format!("Port {port} is already used by a TCP proxy site at sites[{prev}]"),
        ));
    }
    for key in seen.keys().filter(|(_, p)| *p == port) {
        errors.push(ValidationError::new(
            format!("sites[{i}].port"),
            format!(
                "TCP proxy port {port} conflicts with HTTP site '{}:{port}'",
                key.0
            ),
        ));
    }
}

fn check_http_site_port_conflicts(
    i: usize,
    site: &SiteConfig,
    port: u16,
    tcp_ports: &HashMap<u16, usize>,
    seen: &mut HashMap<(String, u16), usize>,
    errors: &mut Vec<ValidationError>,
) {
    if let Some(tcp_idx) = tcp_ports.get(&port) {
        errors.push(ValidationError::new(
            format!("sites[{i}].port"),
            format!("Port {port} is already used by a TCP proxy site at sites[{tcp_idx}]"),
        ));
    }
    let host = site.host.clone().unwrap_or_else(|| "*".to_string());
    if let Some(prev) = seen.insert((host.clone(), port), i) {
        errors.push(ValidationError::new(
            format!("sites[{i}]"),
            format!("Duplicate host+port '{host}:{port}' — already defined at sites[{prev}]"),
        ));
    }
}

fn validate_no_duplicate_host_port(config: &AppConfig, errors: &mut Vec<ValidationError>) {
    // Track ports claimed by TCP proxy sites — TCP binds the OS port regardless of host,
    // so no other site (HTTP or TCP) may use the same port number.
    let mut tcp_ports: HashMap<u16, usize> = HashMap::new();
    let mut seen: HashMap<(String, u16), usize> = HashMap::new();

    for (i, site) in config.sites.iter().enumerate() {
        let port = effective_port(site);
        if site.tcp.is_some() {
            check_tcp_site_port_conflicts(i, port, &mut tcp_ports, &seen, errors);
        } else {
            check_http_site_port_conflicts(i, site, port, &tcp_ports, &mut seen, errors);
        }
    }
}

fn validate_http_redirect_ports(config: &AppConfig, errors: &mut Vec<ValidationError>) {
    let mut seen: HashMap<u16, usize> = HashMap::new();
    for (i, site) in config.sites.iter().enumerate() {
        if let Some(tls) = &site.tls {
            if let Some(port) = tls.http_redirect_port {
                if let Some(prev) = seen.insert(port, i) {
                    errors.push(ValidationError::new(
                        format!("sites[{i}].tls.httpRedirectPort"),
                        format!(
                            "HTTP port {port} already redirects to HTTPS at \
                             sites[{prev}].tls.httpRedirectPort"
                        ),
                    ));
                }
            }
        }
    }
}

// ── Per-site checks ────────────────────────────────────────────────────────

fn validate_site(site: &SiteConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    if let Some(tls) = &site.tls {
        validate_tls(tls, &format!("{prefix}.tls"), errors);
    }
    if let Some(tcp) = &site.tcp {
        validate_tcp_site(tcp, site, prefix, errors);
    }
    if let Some(proxy) = &site.proxy {
        validate_proxy(proxy, &format!("{prefix}.proxy"), errors);
    }
    if let Some(routes) = &site.routes {
        for (i, route) in routes.iter().enumerate() {
            if let Some(ProxyRouteTarget::Full(cfg)) = &route.proxy {
                validate_route_config(cfg, &format!("{prefix}.routes[{i}].proxy"), errors);
            }
        }
    }
    if let Some(ip_filter) = &site.ip_filter {
        validate_ip_filter(ip_filter, prefix, errors);
    }
    if let Some(upload) = &site.upload {
        validate_upload(upload, prefix, errors);
    }
    if let Some(metrics) = &site.metrics {
        validate_metrics(metrics, prefix, errors);
    }
    if let Some(rate_limit) = &site.rate_limit {
        validate_rate_limit(rate_limit, prefix, errors);
    }
    if let Some(redirects) = &site.redirects {
        validate_redirect_rules(redirects, prefix, errors);
    }
    if let Some(fb) = &site.fallback {
        validate_fallback(fb, prefix, errors);
    }
    if let Some(middleware) = &site.middleware {
        validate_middleware(middleware, prefix, errors);
    }
    if let Some(api_key_cfg) = &site.api_key {
        validate_api_key(api_key_cfg, prefix, errors);
    }
    if let Some(jwt) = &site.jwt_auth {
        validate_jwt_auth(jwt, &format!("{prefix}.jwtAuth"), errors);
    }
    if let Some(fa) = &site.forward_auth {
        validate_forward_auth(fa, &format!("{prefix}.forwardAuth"), errors);
    }
    if let Some(c) = &site.consumers {
        validate_consumers(c, &format!("{prefix}.consumers"), errors);
    }
    if let Some(ref limits) = site.limits {
        validate_limits(limits, &format!("{prefix}.limits"), errors);
    }
}

/// Validate a TCP proxy site configuration.
fn validate_tcp_site(
    tcp: &TcpConfig,
    site: &SiteConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if tcp.targets.is_empty() {
        errors.push(ValidationError::new(
            format!("{prefix}.tcp.targets"),
            "at least one target is required for a TCP proxy site",
        ));
    }
    for (i, t) in tcp.targets.iter().enumerate() {
        // Targets must be "host:port" — no http:// prefix.
        if t.starts_with("http://") || t.starts_with("https://") {
            errors.push(ValidationError::new(
                format!("{prefix}.tcp.targets[{i}]"),
                format!("TCP target \"{t}\" must be a plain host:port — no http:// prefix"),
            ));
        } else {
            // Validate host:port using SocketAddr parsing (handles IPv4 and IPv6).
            let valid = t.parse::<std::net::SocketAddr>().is_ok()
                || t.rsplit_once(':')
                    .map(|(host, port)| {
                        !host.is_empty()
                            && !port.is_empty()
                            && port.chars().all(|c| c.is_ascii_digit())
                    })
                    .unwrap_or(false);
            if !valid {
                errors.push(ValidationError::new(
                    format!("{prefix}.tcp.targets[{i}]"),
                    format!(
                        "TCP target \"{t}\" must include a port, e.g. \"host:3306\" \
                         or \"[::1]:3306\" for IPv6"
                    ),
                ));
            }
        }
    }
    // TCP sites cannot be combined with HTTP features.
    if site.proxy.is_some() {
        errors.push(ValidationError::new(
            format!("{prefix}.tcp"),
            "tcp cannot be combined with proxy on the same site",
        ));
    }
    if site.static_files.is_some() {
        errors.push(ValidationError::new(
            format!("{prefix}.tcp"),
            "tcp cannot be combined with static on the same site",
        ));
    }
}

/// Validate an API key configuration block.
///
/// Empty strings in the key list create a bypass: when a client sends no
/// X-Api-Key header, `provided` defaults to `""` which matches `""`.
fn validate_api_key(api_key_cfg: &ApiKeyConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    for (i, key) in api_key_cfg.keys.iter().enumerate() {
        if key.is_empty() {
            errors.push(ValidationError::new(
                format!("{prefix}.apiKey.keys[{i}]"),
                "API key must not be empty — an empty key allows unauthenticated access",
            ));
        }
    }
    if api_key_cfg.keys.is_empty() {
        errors.push(ValidationError::new(
            format!("{prefix}.apiKey.keys"),
            "apiKey.keys must contain at least one key",
        ));
    }
}

fn validate_consumers(
    cfg: &crate::config::schema::ConsumersConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if let Some(ref sj) = cfg.shared_jwt {
        validate_shared_jwt(sj, prefix, errors);
    }
    let has_shared_jwt = cfg.shared_jwt.is_some();
    let mut seen_usernames = std::collections::HashSet::new();
    for (i, c) in cfg.consumers.iter().enumerate() {
        validate_consumer_entry(c, i, prefix, has_shared_jwt, &mut seen_usernames, errors);
    }
}

/// Validate the `consumers.sharedJwt` block.
fn validate_shared_jwt(
    sj: &ConsumersSharedJwtConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    let sj_prefix = format!("{prefix}.sharedJwt");
    let has_secret = sj.secret.is_some();
    let has_jwks = sj.jwks_url.is_some();
    if !has_secret && !has_jwks {
        errors.push(ValidationError::new(
            sj_prefix.clone(),
            "consumers.sharedJwt requires either \"secret\" (HS256) or \"jwksUrl\" (RS256/ES256)",
        ));
    }
    if has_secret && has_jwks {
        errors.push(ValidationError::new(
            sj_prefix.clone(),
            "consumers.sharedJwt.secret and sharedJwt.jwksUrl are mutually exclusive",
        ));
    }
    if let Some(url) = &sj.jwks_url {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            errors.push(ValidationError::new(
                format!("{sj_prefix}.jwksUrl"),
                "consumers.sharedJwt.jwksUrl must be an http:// or https:// URL",
            ));
        }
    }
}

/// Validate a single consumer entry.
fn validate_consumer_entry(
    c: &Consumer,
    i: usize,
    prefix: &str,
    has_shared_jwt: bool,
    seen_usernames: &mut std::collections::HashSet<String>,
    errors: &mut Vec<ValidationError>,
) {
    let entry_prefix = format!("{prefix}.consumers[{i}]");
    if c.username.is_empty() {
        errors.push(ValidationError::new(
            format!("{entry_prefix}.username"),
            "consumer username must not be empty",
        ));
    }
    // Empty consumer API key creates a bypass (same as site-level).
    if let Some(ref key) = c.api_key {
        if key.is_empty() {
            errors.push(ValidationError::new(
                format!("{entry_prefix}.apiKey"),
                "consumer apiKey must not be empty — an empty key allows unauthenticated access",
            ));
        }
    }
    // When sharedJwt is configured, individual consumers are identified by the
    // sharedJwt sub claim and don't need their own credentials.
    if !has_shared_jwt && c.api_key.is_none() && c.basic_auth.is_none() && c.jwt.is_none() {
        errors.push(ValidationError::new(
            entry_prefix.clone(),
            "consumer requires at least one credential: apiKey, basicAuth, jwt (or configure consumers.sharedJwt)",
        ));
    }
    if let Some(ref jwt_cfg) = c.jwt {
        validate_consumer_jwt(jwt_cfg, &entry_prefix, errors);
    }
    if !seen_usernames.insert(c.username.clone()) {
        errors.push(ValidationError::new(
            format!("{entry_prefix}.username"),
            format!("consumer username {:?} is duplicated", c.username),
        ));
    }
    if let Some(ref ba) = c.basic_auth {
        if ba.password.is_empty() {
            errors.push(ValidationError::new(
                format!("{entry_prefix}.basicAuth.password"),
                "consumer basicAuth.password must not be empty",
            ));
        }
    }
}

/// Validate a consumer-level JWT config block.
fn validate_consumer_jwt(
    jwt_cfg: &ConsumerJwtConfig,
    entry_prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    let has_secret = jwt_cfg.secret.is_some();
    let has_jwks = jwt_cfg.jwks_url.is_some();
    if !has_secret && !has_jwks {
        errors.push(ValidationError::new(
            format!("{entry_prefix}.jwt"),
            "consumer jwt requires either \"secret\" (HS256) or \"jwksUrl\" (RS256/ES256)",
        ));
    }
    if has_secret && has_jwks {
        errors.push(ValidationError::new(
            format!("{entry_prefix}.jwt"),
            "consumer jwt.secret and jwt.jwksUrl are mutually exclusive",
        ));
    }
    if let Some(url) = &jwt_cfg.jwks_url {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            errors.push(ValidationError::new(
                format!("{entry_prefix}.jwt.jwksUrl"),
                "consumer jwt.jwksUrl must be an http:// or https:// URL",
            ));
        }
    }
}

fn validate_forward_auth(
    cfg: &crate::config::schema::ForwardAuthConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if cfg.url.is_empty() {
        errors.push(ValidationError::new(
            format!("{prefix}.url"),
            "forwardAuth.url must not be empty",
        ));
        return;
    } else if !cfg.url.starts_with("http://") && !cfg.url.starts_with("https://") {
        errors.push(ValidationError::new(
            format!("{prefix}.url"),
            "forwardAuth.url must be an http:// or https:// URL",
        ));
        return;
    }

    // Warn if the URL targets the Conduit Admin API (default 127.0.0.1:2019).
    // A misconfigured forwardAuth pointing to the admin API would allow an
    // attacker to exploit the proxy's own admin endpoint as the auth server.
    if let Ok(parsed) = ParsedUrl::parse(&cfg.url) {
        let host = parsed.host_str().unwrap_or("");
        let port = parsed.port().unwrap_or(80);
        let is_loopback =
            host == "localhost" || host == "127.0.0.1" || host == "::1" || host.starts_with("127.");
        if is_loopback && port == 2019 {
            errors.push(ValidationError::new(
                format!("{prefix}.url"),
                "forwardAuth.url points to 127.0.0.1:2019 — this is the Conduit Admin API. \
                 Routing external auth requests through the admin API is a security risk. \
                 Use a dedicated auth service instead.",
            ));
        }
    }
    if let Some(0) = cfg.timeout_ms {
        errors.push(ValidationError::new(
            format!("{prefix}.timeoutMs"),
            "forwardAuth.timeoutMs must be > 0",
        ));
    }
}

fn validate_limits(
    cfg: &crate::config::schema::LimitsConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if cfg.max_inflight_requests == Some(0) {
        errors.push(ValidationError::new(
            format!("{prefix}.maxInflightRequests"),
            "limits.maxInflightRequests must be >= 1 (set to null/omit to disable)",
        ));
    }
    if cfg.max_body_bytes == Some(0) {
        errors.push(ValidationError::new(
            format!("{prefix}.maxBodyBytes"),
            "limits.maxBodyBytes must be >= 1 (set to null/omit to disable)",
        ));
    }
    if cfg.timeout_secs == Some(0) {
        errors.push(ValidationError::new(
            format!("{prefix}.timeoutSecs"),
            "limits.timeoutSecs must be >= 1 (set to null/omit to disable)",
        ));
    }
}

fn validate_jwt_auth(
    cfg: &crate::config::schema::JwtAuthConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    let has_secret = cfg.secret.is_some();
    let has_jwks = cfg.jwks_url.is_some();
    if !has_secret && !has_jwks {
        errors.push(ValidationError::new(
            prefix.to_owned(),
            "jwtAuth requires either \"secret\" (HS256) or \"jwksUrl\" (RS256/ES256)",
        ));
    }
    if has_secret && has_jwks {
        errors.push(ValidationError::new(
            prefix.to_owned(),
            "jwtAuth.secret and jwtAuth.jwksUrl are mutually exclusive",
        ));
    }
    if let Some(url) = &cfg.jwks_url {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            errors.push(ValidationError::new(
                format!("{prefix}.jwksUrl"),
                "jwksUrl must be an http:// or https:// URL",
            ));
        }
    }
    // Matches schema/conduit.schema.json's documented minimum: below 60s, the
    // JWKS cache (currently a synchronous per-request fetch on expiry — #163)
    // would refetch on nearly every request instead of caching meaningfully.
    if let Some(refresh) = cfg.jwks_refresh_secs {
        if refresh < 60 {
            errors.push(ValidationError::new(
                format!("{prefix}.jwksRefreshSecs"),
                "jwtAuth.jwksRefreshSecs must be >= 60",
            ));
        }
    }
}

fn validate_middleware(
    entries: &[MiddlewareEntry],
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    for (i, entry) in entries.iter().enumerate() {
        let entry_prefix = format!("{prefix}.middleware[{i}]");
        match entry.r#type.as_str() {
            // Both script and wasm require a `path` field.
            "script" | "wasm" => {
                if entry.path.is_none() {
                    errors.push(ValidationError::new(
                        format!("{entry_prefix}.path"),
                        format!(
                            "middleware type {:?} requires a \"path\" field",
                            entry.r#type
                        ),
                    ));
                }
            }
            "ipFilter" | "rateLimit" | "auth" | "headers" => {
                // Built-in types — currently executed via top-level config fields;
                // listed here so the validator accepts them without error.
            }
            other => {
                errors.push(ValidationError::new(
                    format!("{entry_prefix}.type"),
                    format!("unknown middleware type \"{other}\""),
                ));
            }
        }
    }
}

fn validate_rate_limit(
    rate_limit: &RateLimitConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if rate_limit.window_secs == 0 {
        errors.push(ValidationError::new(
            format!("{prefix}.rateLimit.windowSecs"),
            "windowSecs must be greater than 0",
        ));
    }
    if rate_limit.limit == 0 {
        errors.push(ValidationError::new(
            format!("{prefix}.rateLimit.limit"),
            "limit must be greater than 0",
        ));
    }
    // Validate the store field: must be "memory", a redis:// URL (plaintext),
    // or a rediss:// URL (TLS — requires Redis with in-transit encryption,
    // e.g. AWS ElastiCache TLS, Azure Cache for Redis).
    if let Some(store) = &rate_limit.store {
        let valid_store =
            store == "memory" || store.starts_with("redis://") || store.starts_with("rediss://");
        if !valid_store {
            errors.push(ValidationError::new(
                format!("{prefix}.rateLimit.store"),
                format!(
                    "invalid store \"{store}\" — must be \"memory\", \
                     a redis:// URL (plaintext), or a rediss:// URL (TLS)"
                ),
            ));
        }
    }
}

fn validate_redirect_rules(
    redirects: &[RedirectRule],
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    for (j, rule) in redirects.iter().enumerate() {
        if let Some(status) = rule.status {
            if !matches!(status, 301 | 302 | 307 | 308) {
                errors.push(ValidationError::new(
                    format!("{prefix}.redirects[{j}].status"),
                    format!("Invalid redirect status {status} — must be 301, 302, 307, or 308"),
                ));
            }
        }
    }
}

fn validate_fallback(fb: &FallbackConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    if let Some(status) = fb.status {
        if !(100..=599).contains(&status) {
            errors.push(ValidationError::new(
                format!("{prefix}.fallback.status"),
                format!(
                    "Invalid fallback status {status} \
                     — must be a valid HTTP status code (100–599)"
                ),
            ));
        }
    }
}

fn validate_upload(cfg: &UploadConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    if !cfg.path.starts_with('/') {
        errors.push(ValidationError::new(
            format!("{prefix}.upload.path"),
            format!("Upload path '{}' must start with '/'", cfg.path),
        ));
    }
    if cfg.dir.trim().is_empty() {
        errors.push(ValidationError::new(
            format!("{prefix}.upload.dir"),
            "Upload directory path must not be empty",
        ));
    }
}

fn validate_metrics(cfg: &MetricsConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    if let Some(path) = &cfg.path {
        if !path.starts_with('/') {
            errors.push(ValidationError::new(
                format!("{prefix}.metrics.path"),
                format!("Metrics path '{path}' must start with '/'"),
            ));
        }
    }
}

fn validate_ip_filter(cfg: &IpFilterConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    for (field, list) in [
        ("allow", cfg.allow.as_deref()),
        ("deny", cfg.deny.as_deref()),
    ] {
        let Some(entries) = list else { continue };
        for (i, entry) in entries.iter().enumerate() {
            if !is_valid_ip_or_cidr(entry) {
                errors.push(ValidationError::new(
                    format!("{prefix}.ipFilter.{field}[{i}]"),
                    format!("Invalid IP address or CIDR block: '{entry}'"),
                ));
            }
        }
    }
}

/// Return `true` when `s` is a valid IPv4, IPv6, or CIDR notation address.
fn is_valid_ip_or_cidr(s: &str) -> bool {
    use std::net::IpAddr;
    if s.contains('/') {
        // CIDR: split on '/' and validate both parts.
        let mut parts = s.splitn(2, '/');
        let addr = parts.next().unwrap_or("");
        let prefix = parts.next().unwrap_or("");
        let Ok(ip) = addr.parse::<IpAddr>() else {
            return false;
        };
        let Ok(prefix_len) = prefix.parse::<u32>() else {
            return false;
        };
        let max_prefix = match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        prefix_len <= max_prefix
    } else {
        s.parse::<IpAddr>().is_ok()
    }
}

fn validate_tls(tls: &TlsConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    let has_acme = tls.acme.is_some();
    let has_cert = tls.cert.is_some();
    let has_key = tls.key.is_some();

    if has_acme && (has_cert || has_key) {
        errors.push(ValidationError::new(
            prefix,
            "Cannot combine 'acme' with 'cert'/'key' — use Auto-TLS or manual certificates, not both",
        ));
    }

    // XOR: one is set but not the other
    if !has_acme && (has_cert ^ has_key) {
        let missing = if has_cert { "key" } else { "cert" };
        errors.push(ValidationError::new(
            prefix,
            format!("TLS 'cert' and 'key' must both be set — '{missing}' is missing"),
        ));
    }

    if let Some(ref ca) = tls.client_auth {
        validate_tls_client_auth(ca, prefix, has_cert, has_acme, errors);
    }

    // Cert expiry check — only for manual certificates (ACME manages renewal itself).
    if !has_acme {
        if let Some(ref cert_path) = tls.cert {
            check_cert_expiry(cert_path, &format!("{prefix}.cert"), errors);
        }
    }
}

/// Validate the `tls.clientAuth` (mTLS) configuration block.
fn validate_tls_client_auth(
    ca: &TlsClientAuth,
    prefix: &str,
    has_cert: bool,
    has_acme: bool,
    errors: &mut Vec<ValidationError>,
) {
    if ca.ca.is_empty() {
        errors.push(ValidationError::new(
            format!("{prefix}.clientAuth.ca"),
            "tls.clientAuth.ca must be a path to a PEM CA file",
        ));
    }
    // clientAuth requires cert+key (or acme) to make sense.
    if !has_cert && !has_acme {
        errors.push(ValidationError::new(
            format!("{prefix}.clientAuth"),
            "tls.clientAuth requires tls.cert+tls.key or tls.acme to be configured",
        ));
    }
}

/// Check a PEM certificate file for expiry.
///
/// - If the cert is already expired → validation error.
/// - If the cert expires within 30 days → validation error with "WARNING:" prefix
///   (surfaced as an error so `conduit validate` exits non-zero, prompting renewal).
/// - If the file does not exist yet → silently ignored (cert may be provisioned later).
/// - If the file cannot be parsed → silently ignored (startup will fail with a clearer error).
fn check_cert_expiry(cert_path: &str, prefix: &str, errors: &mut Vec<ValidationError>) {
    let pem_bytes = match std::fs::read(cert_path) {
        Ok(b) => b,
        Err(_) => return, // file not found yet — skip
    };

    // Parse the first PEM block.
    let (_, pem) = match x509_parser::pem::parse_x509_pem(&pem_bytes) {
        Ok(v) => v,
        Err(_) => return, // not a valid PEM — skip
    };
    let cert = match pem.parse_x509() {
        Ok(c) => c,
        Err(_) => return, // unparseable DER — skip
    };

    let not_after = cert.validity().not_after.to_datetime();

    // Convert `not_after` (x509_parser's `time::OffsetDateTime`) to `SystemTime`.
    let unix_secs = not_after.unix_timestamp();
    let expires_at = if unix_secs >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_secs(unix_secs as u64)
    } else {
        SystemTime::UNIX_EPOCH
    };

    let now = SystemTime::now();
    let warn_threshold = now + Duration::from_secs(30 * 24 * 3600);

    if expires_at <= now {
        errors.push(ValidationError::new(
            prefix,
            format!("TLS certificate has expired (not_after = {unix_secs})"),
        ));
    } else if expires_at <= warn_threshold {
        let days_left = expires_at.duration_since(now).unwrap_or_default().as_secs() / 86400;
        errors.push(ValidationError::new(
            prefix,
            format!("WARNING: TLS certificate expires in {days_left} day(s) — renew soon"),
        ));
    }
}

fn validate_proxy(proxy: &ProxyConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    match proxy {
        ProxyConfig::Single(url) => {
            if !is_valid_upstream_url(url) {
                errors.push(ValidationError::new(
                    prefix,
                    format!("Invalid upstream URL '{url}' — must start with http:// or https://"),
                ));
            }
        }
        ProxyConfig::Routes(routes) => {
            for (route, target) in routes {
                let route_prefix = format!("{prefix}[\"{route}\"]");
                validate_proxy_route_target(target, &route_prefix, errors);
            }
        }
    }
}

fn validate_proxy_route_target(
    target: &ProxyRouteTarget,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    match target {
        ProxyRouteTarget::Url(url) => {
            if !is_valid_upstream_url(url) {
                errors.push(ValidationError::new(
                    prefix,
                    format!("Invalid upstream URL '{url}' — must start with http:// or https://"),
                ));
            }
        }
        ProxyRouteTarget::RoundRobin(urls) => {
            for (i, url) in urls.iter().enumerate() {
                if !is_valid_upstream_url(url) {
                    errors.push(ValidationError::new(
                        format!("{prefix}[{i}]"),
                        format!(
                            "Invalid upstream URL '{url}' — must start with http:// or https://"
                        ),
                    ));
                }
            }
        }
        ProxyRouteTarget::Full(cfg) => {
            validate_route_config(cfg, prefix, errors);
        }
    }
}

/// Return `true` when the string is an absolute http:// or https:// URL with a non-empty host.
fn is_valid_upstream_url(url: &str) -> bool {
    let rest = if let Some(r) = url.strip_prefix("http://") {
        r
    } else if let Some(r) = url.strip_prefix("https://") {
        r
    } else {
        return false;
    };
    // After stripping the scheme the host must be non-empty.
    !rest.split('/').next().unwrap_or("").is_empty()
}

fn validate_route_config(cfg: &ProxyRouteConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    if cfg.targets.is_empty() && cfg.groups.is_none() {
        errors.push(ValidationError::new(
            format!("{prefix}.targets"),
            "At least one target is required (or use 'groups' for two-level balancing)",
        ));
    }
    if let Some(groups) = &cfg.groups {
        validate_groups_config(groups, prefix, errors);
    }
    if cfg.strategy == Some(LoadBalanceStrategy::WeightedRoundRobin) {
        check_weighted_targets(&cfg.targets, &format!("{prefix}.targets"), errors);
    }
    validate_target_urls(&cfg.targets, prefix, errors);
    if let Some(rules) = &cfg.rewrite {
        validate_rewrite_rules(rules, prefix, errors);
    }
    if let Some(mirror) = &cfg.mirror {
        if !mirror.starts_with("http://") && !mirror.starts_with("https://") {
            errors.push(ValidationError::new(
                format!("{prefix}.mirror"),
                "mirror URL must be http:// or https://",
            ));
        }
    }
    if let Some(tls) = &cfg.upstream_tls {
        // Warn if verify: false is set (not an error, just a potential misconfiguration).
        if tls.verify == Some(false) {
            tracing::debug!(
                "{prefix}.upstreamTls.verify is false — upstream certificate will not be verified"
            );
        }
    }
    if let Some(cache) = &cfg.cache {
        validate_cache_config(cache, &format!("{prefix}.cache"), errors);
    }
}

/// Validate the `cache` config block on a proxy route.
fn validate_cache_config(
    cache: &crate::config::schema::CacheConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    let store = &cache.store;
    let valid = store == "memory"
        || store.starts_with("redis://")
        || store.starts_with("rediss://")
        || store.starts_with("disk:");
    if !valid {
        errors.push(ValidationError::new(
            format!("{prefix}.store"),
            format!(
                "invalid store \"{store}\" — must be \"memory\", \
                 a redis:// URL, a rediss:// URL (TLS), or disk:<path>"
            ),
        ));
    }
    if let (Some(swr), Some(ttl)) = (cache.stale_while_revalidate_secs, cache.ttl_secs) {
        if swr as u64 > ttl.saturating_mul(10) {
            // Not a hard error, just a suspicious config.
            tracing::debug!(
                "{prefix}.staleWhileRevalidateSecs ({swr}) is more than 10× ttlSecs ({ttl})"
            );
        }
    }
}

/// Validate upstream groups: non-empty targets and WRR strategy requirements.
fn validate_groups_config(
    groups: &[crate::config::schema::UpstreamGroup],
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    for (i, group) in groups.iter().enumerate() {
        if group.targets.is_empty() {
            errors.push(ValidationError::new(
                format!("{prefix}.groups[{i}].targets"),
                "At least one target is required in each group",
            ));
        }
        if group.strategy == Some(LoadBalanceStrategy::WeightedRoundRobin) {
            check_weighted_targets(
                &group.targets,
                &format!("{prefix}.groups[{i}].targets"),
                errors,
            );
        }
    }
}

/// Emit an error when any target in `targets` is a `Simple` (unweighted) URL
/// and the strategy is `weighted-round-robin`.
fn check_weighted_targets(targets: &[ProxyTarget], field: &str, errors: &mut Vec<ValidationError>) {
    let has_simple = targets.iter().any(|t| matches!(t, ProxyTarget::Simple(_)));
    if has_simple {
        errors.push(ValidationError::new(
            field.to_string(),
            "Strategy 'weighted-round-robin' requires weighted targets: \
             { \"url\": \"...\", \"weight\": N }",
        ));
    }
}

/// Validate that every target URL starts with `http://` or `https://`.
fn validate_target_urls(targets: &[ProxyTarget], prefix: &str, errors: &mut Vec<ValidationError>) {
    for (i, target) in targets.iter().enumerate() {
        let url = match target {
            ProxyTarget::Simple(u) => u.as_str(),
            ProxyTarget::Weighted(w) => w.url.as_str(),
        };
        if !is_valid_upstream_url(url) {
            errors.push(ValidationError::new(
                format!("{prefix}.targets[{i}]"),
                format!("Invalid upstream URL '{url}' — must start with http:// or https://"),
            ));
        }
    }
}

fn validate_rewrite_rules(rules: &[RewriteRule], prefix: &str, errors: &mut Vec<ValidationError>) {
    for (i, rule) in rules.iter().enumerate() {
        if let Err(e) = regex::Regex::new(&rule.from) {
            errors.push(ValidationError::new(
                format!("{prefix}.rewrite[{i}].from"),
                format!("Invalid regex '{}': {e}", rule.from),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::from_str;

    fn parse(json: &str) -> AppConfig {
        from_str(json).expect("parse failed")
    }

    fn errs(json: &str) -> Vec<ValidationError> {
        validate(&parse(json))
    }

    #[test]
    fn valid_config_no_errors() {
        assert!(errs(r#"{ "port": 8080 }"#).is_empty());
    }

    #[test]
    fn duplicate_host_port_detected() {
        let e = errs(r#"[{ "port": 8080 }, { "port": 8080 }]"#);
        assert_eq!(e.len(), 1);
        assert!(e[0].message.contains("Duplicate"), "got: {}", e[0].message);
    }

    #[test]
    fn different_hosts_no_error() {
        assert!(errs(
            r#"[
                { "host": "a.example.com", "port": 443 },
                { "host": "b.example.com", "port": 443 }
            ]"#
        )
        .is_empty());
    }

    #[test]
    fn duplicate_http_redirect_port() {
        let e = errs(
            r#"[
                { "port": 443, "tls": { "cert": "a.pem", "key": "a.key", "httpRedirectPort": 80 } },
                { "port": 444, "tls": { "cert": "b.pem", "key": "b.key", "httpRedirectPort": 80 } }
            ]"#,
        );
        assert!(e.iter().any(|e| e.message.contains("80")));
    }

    #[test]
    fn tls_acme_and_cert_conflict() {
        let e = errs(
            r#"{ "tls": { "cert": "a.pem", "key": "a.key", "acme": { "email": "a@b.com" } } }"#,
        );
        assert_eq!(e.len(), 1);
        assert!(e[0].message.contains("acme"), "got: {}", e[0].message);
    }

    #[test]
    fn tls_missing_key() {
        let e = errs(r#"{ "tls": { "cert": "a.pem" } }"#);
        assert_eq!(e.len(), 1);
        assert!(e[0].message.contains("key"), "got: {}", e[0].message);
    }

    #[test]
    fn tls_acme_only_valid() {
        assert!(errs(r#"{ "tls": { "acme": { "email": "a@b.com" } } }"#).is_empty());
    }

    #[test]
    fn tls_cert_and_key_valid() {
        assert!(errs(r#"{ "tls": { "cert": "a.pem", "key": "a.key" } }"#).is_empty());
    }

    #[test]
    fn weighted_rr_with_simple_targets_invalid() {
        let e = errs(
            r#"{
                "proxy": {
                    "/api": {
                        "targets": ["http://b1:4000", "http://b2:4000"],
                        "strategy": "weighted-round-robin"
                    }
                }
            }"#,
        );
        assert!(!e.is_empty());
        assert!(e[0].message.contains("weighted"), "got: {}", e[0].message);
    }

    #[test]
    fn weighted_rr_with_weighted_targets_valid() {
        assert!(errs(
            r#"{
                "proxy": {
                    "/api": {
                        "targets": [
                            { "url": "http://b1:4000", "weight": 3 },
                            { "url": "http://b2:4000", "weight": 1 }
                        ],
                        "strategy": "weighted-round-robin"
                    }
                }
            }"#
        )
        .is_empty());
    }

    #[test]
    fn invalid_redirect_status() {
        let e = errs(r#"{ "redirects": [{ "from": "/a", "to": "/b", "status": 200 }] }"#);
        assert!(!e.is_empty());
        assert!(e[0].message.contains("200"), "got: {}", e[0].message);
    }

    #[test]
    fn valid_redirect_status() {
        assert!(
            errs(r#"{ "redirects": [{ "from": "/a", "to": "/b", "status": 301 }] }"#).is_empty()
        );
    }

    #[test]
    fn rate_limit_zero_window_invalid() {
        let e = errs(r#"{ "rateLimit": { "windowSecs": 0, "limit": 100 } }"#);
        assert!(!e.is_empty());
    }

    #[test]
    fn empty_proxy_targets_invalid() {
        let e = errs(r#"{ "proxy": { "/api": { "targets": [] } } }"#);
        assert!(!e.is_empty());
        assert!(
            e[0].message.contains("target") || e[0].message.contains("empty"),
            "got: {}",
            e[0].message
        );
    }

    #[test]
    fn fallback_status_below_range_invalid() {
        let e = errs(r#"{ "fallback": { "status": 99 } }"#);
        assert!(!e.is_empty(), "status 99 is below 100 and must be rejected");
        assert!(
            e[0].message.contains("99") || e[0].message.contains("status"),
            "got: {}",
            e[0].message
        );
    }

    #[test]
    fn fallback_status_above_range_invalid() {
        let e = errs(r#"{ "fallback": { "status": 600 } }"#);
        assert!(
            !e.is_empty(),
            "status 600 is above 599 and must be rejected"
        );
    }

    #[test]
    fn fallback_status_in_range_valid() {
        assert!(errs(r#"{ "fallback": { "status": 404 } }"#).is_empty());
        assert!(errs(r#"{ "fallback": { "status": 200 } }"#).is_empty());
    }

    #[test]
    fn fallback_no_status_valid() {
        assert!(errs(r#"{ "fallback": {} }"#).is_empty());
    }

    #[test]
    fn tls_missing_cert() {
        let e = errs(r#"{ "tls": { "key": "a.key" } }"#);
        assert_eq!(e.len(), 1);
        assert!(e[0].message.contains("cert"), "got: {}", e[0].message);
    }

    #[test]
    fn groups_empty_targets_in_group_invalid() {
        let e = errs(
            r#"{
                "proxy": {
                    "/api": {
                        "groups": [
                            { "name": "a", "targets": [] },
                            { "name": "b", "targets": ["http://b1:4000"] }
                        ]
                    }
                }
            }"#,
        );
        assert!(!e.is_empty(), "empty group targets must be rejected");
        assert!(
            e[0].message.contains("target") || e[0].message.contains("group"),
            "got: {}",
            e[0].message
        );
    }

    #[test]
    fn groups_weighted_rr_with_simple_targets_invalid() {
        let e = errs(
            r#"{
                "proxy": {
                    "/api": {
                        "groups": [
                            {
                                "name": "a",
                                "targets": ["http://b1:4000", "http://b2:4000"],
                                "strategy": "weighted-round-robin"
                            }
                        ]
                    }
                }
            }"#,
        );
        assert!(!e.is_empty());
        assert!(e[0].message.contains("weighted"), "got: {}", e[0].message);
    }

    #[test]
    fn groups_no_top_level_targets_valid() {
        assert!(
            errs(
                r#"{
                    "proxy": {
                        "/api": {
                            "groups": [
                                { "name": "a", "targets": ["http://b1:4000"] },
                                { "name": "b", "targets": ["http://b2:4000"] }
                            ]
                        }
                    }
                }"#
            )
            .is_empty(),
            "groups without top-level targets must be valid"
        );
    }

    #[test]
    fn invalid_rewrite_regex_detected() {
        let e = errs(
            r#"{
                "proxy": {
                    "/api": {
                        "targets": ["http://b:4000"],
                        "rewrite": [{ "from": "(unclosed", "to": "/" }]
                    }
                }
            }"#,
        );
        assert!(!e.is_empty(), "invalid regex must be caught");
        assert!(
            e[0].path.contains("rewrite"),
            "error path must reference rewrite field, got: {}",
            e[0].path
        );
    }

    #[test]
    fn valid_rewrite_rules_no_errors() {
        assert!(errs(
            r#"{
                    "proxy": {
                        "/api": {
                            "targets": ["http://b:4000"],
                            "rewrite": [
                                { "from": "^/v[0-9]+/(.+)$", "to": "/$1" }
                            ]
                        }
                    }
                }"#
        )
        .is_empty());
    }

    #[test]
    fn invalid_upstream_url_single_proxy() {
        let e = errs(r#"{ "proxy": "not-a-url" }"#);
        assert!(!e.is_empty(), "non-HTTP URL must be rejected");
        assert!(e[0].message.contains("http://") || e[0].message.contains("https://"));
    }

    #[test]
    fn invalid_upstream_url_in_roundrobin_array() {
        let e = errs(r#"{ "proxy": { "/api": ["http://ok:4000", "ftp://bad:4000"] } }"#);
        assert!(!e.is_empty());
        assert!(e[0].message.contains("ftp://bad"));
    }

    #[test]
    fn valid_upstream_urls_no_errors() {
        assert!(errs(r#"{ "proxy": "http://localhost:4000" }"#).is_empty());
        assert!(errs(r#"{ "proxy": "https://api.example.com" }"#).is_empty());
        assert!(errs(
            r#"{ "proxy": { "/api": { "targets": ["http://b1:4000", "http://b2:4000"] } } }"#
        )
        .is_empty());
    }

    #[test]
    fn ip_filter_invalid_cidr_detected() {
        let e = errs(r#"{ "ipFilter": { "deny": ["999.999.0.0/8", "not-an-ip"] } }"#);
        assert_eq!(e.len(), 2, "both invalid entries must be flagged");
        assert!(e.iter().all(|e| e.path.contains("ipFilter")));
    }

    #[test]
    fn ip_filter_valid_entries_no_errors() {
        assert!(errs(
            r#"{ "ipFilter": { "allow": ["10.0.0.0/8", "192.168.1.1", "::1", "2001:db8::/32"] } }"#
        )
        .is_empty());
    }

    #[test]
    fn ip_filter_prefix_too_large_invalid() {
        let e = errs(r#"{ "ipFilter": { "deny": ["10.0.0.0/33"] } }"#);
        assert!(!e.is_empty(), "/33 is invalid for IPv4");
    }

    #[test]
    fn upload_path_without_leading_slash_invalid() {
        let e = errs(r#"{ "upload": { "path": "upload", "dir": "./uploads" } }"#);
        assert!(
            !e.is_empty(),
            "upload path without leading slash must be rejected"
        );
        assert!(e[0].path.contains("upload.path"), "got: {}", e[0].path);
    }

    #[test]
    fn upload_valid_config_no_errors() {
        assert!(errs(r#"{ "upload": { "path": "/upload", "dir": "./uploads" } }"#).is_empty());
    }

    #[test]
    fn metrics_path_without_leading_slash_invalid() {
        let e = errs(r#"{ "metrics": { "path": "metrics" } }"#);
        assert!(
            !e.is_empty(),
            "metrics path without leading slash must be rejected"
        );
        assert!(e[0].path.contains("metrics.path"), "got: {}", e[0].path);
    }

    #[test]
    fn metrics_valid_path_no_errors() {
        assert!(errs(r#"{ "metrics": { "path": "/__metrics__" } }"#).is_empty());
    }

    #[test]
    fn routes_array_rewrite_regex_validated() {
        let e = errs(
            r#"{
                "routes": [
                    {
                        "match": { "path": "/api/**" },
                        "proxy": {
                            "targets": ["http://b:4000"],
                            "rewrite": [{ "from": "[bad", "to": "/" }]
                        }
                    }
                ]
            }"#,
        );
        assert!(
            !e.is_empty(),
            "invalid regex in routes array must be caught"
        );
    }

    // ── rateLimit.store validation ────────────────────────────────────────────

    #[test]
    fn rate_limit_memory_store_valid() {
        assert!(
            errs(r#"{ "rateLimit": { "windowSecs": 60, "limit": 100, "store": "memory" } }"#)
                .is_empty()
        );
    }

    #[test]
    fn rate_limit_redis_store_valid() {
        assert!(errs(r#"{ "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://127.0.0.1:6379" } }"#)
            .is_empty());
    }

    #[test]
    fn rate_limit_invalid_store_rejected() {
        let e = errs(
            r#"{ "rateLimit": { "windowSecs": 60, "limit": 100, "store": "memcached://localhost" } }"#,
        );
        assert!(!e.is_empty(), "invalid store must be rejected");
        assert!(e[0].path.contains("store"), "got: {}", e[0].path);
    }

    // ── proxy cache store validation ─────────────────────────────────────────

    fn proxy_with_cache(store: &str) -> Vec<ValidationError> {
        errs(&format!(
            r#"{{ "proxy": {{ "/api": {{ "targets": ["http://b:4000"], "cache": {{ "store": "{store}", "ttlSecs": 60 }} }} }} }}"#
        ))
    }

    #[test]
    fn cache_store_memory_valid() {
        assert!(proxy_with_cache("memory").is_empty());
    }

    #[test]
    fn cache_store_redis_url_valid() {
        assert!(proxy_with_cache("redis://localhost:6379").is_empty());
    }

    #[test]
    fn cache_store_rediss_tls_valid() {
        assert!(proxy_with_cache("rediss://redis.example.com:6380").is_empty());
    }

    #[test]
    fn cache_store_disk_valid() {
        assert!(proxy_with_cache("disk:/var/cache/conduit").is_empty());
    }

    #[test]
    fn cache_store_invalid_rejected() {
        let e = proxy_with_cache("memcached://localhost");
        assert!(!e.is_empty(), "invalid cache store must be rejected");
        assert!(e[0].path.contains("store"), "got: {}", e[0].path);
    }

    // ── TCP proxy validation ──────────────────────────────────────────────────

    #[test]
    fn tcp_proxy_valid() {
        let e = errs(r#"{ "port": 3306, "tcp": { "targets": ["mysql:3306"] } }"#);
        assert!(e.is_empty(), "valid TCP site must pass: {e:?}");
    }

    #[test]
    fn tcp_proxy_no_targets_rejected() {
        let e = errs(r#"{ "port": 3306, "tcp": { "targets": [] } }"#);
        assert!(!e.is_empty());
        assert!(e[0].path.contains("targets"), "got: {}", e[0].path);
    }

    #[test]
    fn tcp_proxy_http_prefix_rejected() {
        let e = errs(r#"{ "port": 3306, "tcp": { "targets": ["http://mysql:3306"] } }"#);
        assert!(
            !e.is_empty(),
            "http:// prefix must be rejected for TCP targets"
        );
    }

    #[test]
    fn tcp_proxy_missing_port_rejected() {
        let e = errs(r#"{ "port": 3306, "tcp": { "targets": ["just-a-host"] } }"#);
        assert!(!e.is_empty(), "target without port must be rejected");
    }

    #[test]
    fn tcp_proxy_combined_with_proxy_rejected() {
        let e = errs(
            r#"{ "port": 3306, "tcp": { "targets": ["mysql:3306"] }, "proxy": "http://b:4000" }"#,
        );
        assert!(!e.is_empty(), "tcp + proxy must be rejected");
    }

    // ── middleware validation ─────────────────────────────────────────────────

    #[test]
    fn middleware_script_without_path_is_invalid() {
        let e = errs(r#"{ "middleware": [{ "type": "script" }] }"#);
        assert!(!e.is_empty(), "script entry without path must be rejected");
        assert!(
            e[0].message.contains("path"),
            "error must mention missing path, got: {}",
            e[0].message
        );
    }

    #[test]
    fn middleware_script_with_path_is_valid() {
        assert!(
            errs(r#"{ "middleware": [{ "type": "script", "path": "./my.rhai" }] }"#).is_empty()
        );
    }

    #[test]
    fn middleware_builtin_type_is_valid() {
        assert!(errs(r#"{ "middleware": [{ "type": "ipFilter" }] }"#).is_empty());
        assert!(errs(r#"{ "middleware": [{ "type": "rateLimit" }] }"#).is_empty());
        assert!(errs(r#"{ "middleware": [{ "type": "auth" }] }"#).is_empty());
        assert!(errs(r#"{ "middleware": [{ "type": "headers" }] }"#).is_empty());
    }

    #[test]
    fn middleware_unknown_type_is_invalid() {
        let e = errs(r#"{ "middleware": [{ "type": "magic" }] }"#);
        assert!(!e.is_empty(), "unknown middleware type must be rejected");
        assert!(
            e[0].message.contains("unknown middleware type"),
            "got: {}",
            e[0].message
        );
    }

    #[test]
    fn middleware_mixed_entries_validated() {
        // Script with path + script without path: one error expected.
        let e = errs(
            r#"{ "middleware": [
                { "type": "script", "path": "./ok.rhai" },
                { "type": "script" }
            ] }"#,
        );
        assert_eq!(e.len(), 1, "exactly one script entry is missing path");
    }

    #[test]
    fn rate_limit_limit_zero_returns_error() {
        let e = errs(r#"{ "rateLimit": { "windowSecs": 60, "limit": 0 } }"#);
        assert!(!e.is_empty(), "limit 0 must be rejected");
        assert!(
            e.iter().any(|err| err.path.contains("limit")),
            "error path must mention limit: {:?}",
            e
        );
    }

    #[test]
    fn upload_empty_dir_invalid() {
        let e = errs(r#"{ "upload": { "path": "/upload", "dir": "" } }"#);
        assert!(!e.is_empty(), "empty upload dir must be rejected");
        assert!(
            e.iter().any(|err| err.path.contains("upload")),
            "error path must mention upload: {:?}",
            e
        );
    }

    #[test]
    fn cidr_with_non_numeric_mask_is_invalid() {
        // "10.0.0.0/abc" — the prefix length is not a valid u32.
        let e = errs(r#"{ "ipFilter": { "deny": ["10.0.0.0/abc"] } }"#);
        assert!(!e.is_empty(), "CIDR with non-numeric mask must be rejected");
        assert!(
            e.iter().any(|err| err.path.contains("ipFilter")),
            "error path must mention ipFilter: {:?}",
            e
        );
    }

    // ── TLS cert expiry ───────────────────────────────────────────────────────

    /// Helper: write `content` to a temp file and return the (dir, path_string).
    fn write_temp_file(content: &[u8], name: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        let path_str = path.to_str().unwrap().replace('\\', "/");
        (dir, path_str)
    }

    #[test]
    fn cert_expiry_invalid_pem_silently_ignored() {
        // File exists but contains garbage — check_cert_expiry should return without error.
        let (_dir, cert_path) = write_temp_file(b"this is not valid PEM content", "bad.pem");
        let json = format!(r#"{{"tls": {{"cert": "{cert_path}", "key": "k.key"}}}}"#);
        let e = errs(&json);
        // The only possible error is "missing key" or none. No cert-expiry error.
        assert!(
            e.iter()
                .all(|err| !err.message.contains("expired") && !err.message.contains("WARNING")),
            "invalid PEM must not produce cert-expiry errors: {:?}",
            e
        );
    }

    #[test]
    fn cert_expiry_expired_cert_returns_error() {
        // Generate a cert with not_after in the past (expired ~5 years ago).
        let mut params = rcgen::CertificateParams::new(vec!["example.com".to_string()]).unwrap();
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params.not_after = rcgen::date_time_ymd(2020, 12, 31);
        let key_pair = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key_pair).unwrap();
        let pem = cert.pem();

        let (_dir, cert_path) = write_temp_file(pem.as_bytes(), "expired.pem");
        let json = format!(r#"{{"tls": {{"cert": "{cert_path}", "key": "k.key"}}}}"#);
        let e = errs(&json);
        assert!(
            e.iter().any(|err| err.message.contains("expired")),
            "expired cert must produce an expiry error: {:?}",
            e
        );
    }

    #[test]
    fn cert_expiry_soon_returns_warning() {
        // Generate a cert expiring 15 days from now — within the 30-day warning window.
        use time::{Duration, OffsetDateTime};
        let soon = OffsetDateTime::now_utc() + Duration::days(15);
        let mut params = rcgen::CertificateParams::new(vec!["example.com".to_string()]).unwrap();
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params.not_after = soon;
        let key_pair = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key_pair).unwrap();
        let pem = cert.pem();

        let (_dir, cert_path) = write_temp_file(pem.as_bytes(), "soon.pem");
        let json = format!(r#"{{"tls": {{"cert": "{cert_path}", "key": "k.key"}}}}"#);
        let e = errs(&json);
        assert!(
            e.iter().any(|err| err.message.contains("WARNING")),
            "soon-expiring cert must produce a WARNING: {:?}",
            e
        );
    }

    #[test]
    fn proxy_route_url_variant_invalid_scheme_rejected() {
        // `{ "/api": "ftp://bad" }` parses as ProxyRouteTarget::Url("ftp://bad").
        // Exercises the Url arm of validate_proxy_route_target (lines 381-385).
        let e = errs(r#"{ "proxy": { "/api": "ftp://bad" } }"#);
        assert!(!e.is_empty(), "non-HTTP proxy URL must be rejected");
        assert!(
            e.iter()
                .any(|err| err.message.contains("Invalid upstream URL")),
            "error must mention invalid URL: {:?}",
            e
        );
    }

    #[test]
    fn proxy_route_full_invalid_target_url_rejected() {
        // Full form with an ftp:// target URL exercises lines 474-476 in validate_route_config.
        let e = errs(r#"{ "proxy": { "/api": { "targets": ["ftp://bad:4000"] } } }"#);
        assert!(!e.is_empty(), "non-HTTP target URL must be rejected");
        assert!(
            e.iter()
                .any(|err| err.message.contains("Invalid upstream URL")),
            "error must mention invalid URL: {:?}",
            e
        );
    }

    // ── feature_warnings ─────────────────────────────────────────────────────

    fn warns(json: &str) -> Vec<String> {
        feature_warnings(&parse(json))
    }

    #[test]
    fn no_warnings_for_plain_config() {
        // A config with no feature-gated options produces no warnings.
        assert!(warns(r#"{ "port": 8080, "proxy": "http://up:4000" }"#).is_empty());
    }

    #[test]
    #[cfg(feature = "wasm")]
    fn no_warning_when_wasm_feature_enabled() {
        // When the wasm feature IS compiled in, no warning is emitted.
        let w = warns(
            r#"{ "port": 8080,
                 "middleware": [{ "type": "wasm", "path": "p.wasm" }] }"#,
        );
        assert!(w.is_empty(), "wasm feature active → no warning: {w:?}");
    }

    #[test]
    #[cfg(not(feature = "wasm"))]
    fn warning_for_wasm_middleware_without_feature() {
        let w = warns(
            r#"{ "port": 8080,
                 "middleware": [{ "type": "wasm", "path": "plugin.wasm" }] }"#,
        );
        assert_eq!(w.len(), 1, "expected exactly one warning: {w:?}");
        assert!(
            w[0].contains("wasm"),
            "warning must mention 'wasm': {}",
            w[0]
        );
        assert!(
            w[0].contains("--features wasm"),
            "warning must mention compile flag: {}",
            w[0]
        );
    }

    #[test]
    #[cfg(not(feature = "wasm"))]
    fn warning_per_wasm_entry() {
        // Two wasm + one script entry → warnings depend on which features are off.
        let w = warns(
            r#"{ "port": 8080,
                 "middleware": [
                     { "type": "wasm", "path": "a.wasm" },
                     { "type": "script", "path": "b.rhai" },
                     { "type": "wasm", "path": "c.wasm" }
                 ] }"#,
        );
        // At least 2 warnings for the two WASM entries.
        assert!(w.len() >= 2, "at least two wasm warnings expected: {w:?}");
        let wasm_warns = w.iter().filter(|m| m.contains("wasm")).count();
        assert_eq!(wasm_warns, 2, "exactly two wasm warnings: {w:?}");
    }

    #[test]
    #[cfg(not(feature = "otlp"))]
    fn warning_for_otlp_without_feature() {
        let w = warns(
            r#"{ "global": { "otlp": { "endpoint": "http://otel:4317" } },
                 "sites": [{ "port": 8080 }] }"#,
        );
        assert_eq!(w.len(), 1, "expected exactly one otlp warning: {w:?}");
        assert!(
            w[0].contains("otlp"),
            "warning must mention 'otlp': {}",
            w[0]
        );
        assert!(
            w[0].contains("--features otlp"),
            "warning must mention compile flag: {}",
            w[0]
        );
    }

    #[test]
    #[cfg(feature = "otlp")]
    fn no_warning_when_otlp_feature_enabled() {
        let w = warns(
            r#"{ "global": { "otlp": { "endpoint": "http://otel:4317" } },
                 "sites": [{ "port": 8080 }] }"#,
        );
        assert!(w.is_empty(), "otlp feature active → no warning: {w:?}");
    }

    // ── proxy loop detection ──────────────────────────────────────────────────

    #[test]
    fn proxy_loop_on_own_port_warns() {
        // Port 8080 listens AND is the proxy target → loop.
        let w = warns(r#"{ "port": 8080, "proxy": "http://127.0.0.1:8080" }"#);
        assert!(!w.is_empty(), "self-referencing target must warn: {w:?}");
        assert!(
            w.iter().any(|m| m.contains("loop")),
            "warning must mention loop: {w:?}"
        );
    }

    #[test]
    fn proxy_to_different_port_no_warn() {
        let w = warns(r#"{ "port": 8080, "proxy": "http://127.0.0.1:4000" }"#);
        assert!(
            w.iter().all(|m| !m.contains("loop")),
            "proxy to different port must not warn about loop: {w:?}"
        );
    }

    #[test]
    fn proxy_to_external_host_no_warn() {
        let w = warns(r#"{ "port": 8080, "proxy": "http://api.example.com:8080" }"#);
        assert!(
            w.iter().all(|m| !m.contains("loop")),
            "proxy to external host must not warn: {w:?}"
        );
    }

    // ── weak JWT secret ───────────────────────────────────────────────────────

    #[test]
    #[cfg(feature = "jwt")] // secret-length warning only fires when jwt feature is enabled
    fn short_jwt_secret_warns() {
        let w = warns(r#"{ "port": 8080, "jwtAuth": { "secret": "short" } }"#);
        assert!(!w.is_empty(), "short secret must warn");
        let has_32_bytes_warn = w.iter().any(|m| m.contains("32 bytes"));
        assert!(has_32_bytes_warn, "warning must mention 32 bytes: {w:?}");
    }

    #[test]
    #[cfg(not(feature = "jwt"))] // when jwt feature is off, generates a different warning
    fn jwt_without_feature_warns() {
        let w = warns(r#"{ "port": 8080, "jwtAuth": { "secret": "short" } }"#);
        assert!(!w.is_empty(), "jwtAuth without jwt feature must warn");
        assert!(
            w.iter().any(|m| m.contains("jwt")),
            "warning must mention jwt: {w:?}"
        );
    }

    #[test]
    fn adequate_jwt_secret_no_warn() {
        let secret = "a".repeat(32);
        let w = warns(&format!(
            r#"{{ "port": 8080, "jwtAuth": {{ "secret": "{secret}" }} }}"#
        ));
        assert!(
            w.iter().all(|m| !m.contains("secret")),
            "32-byte secret must not warn: {w:?}"
        );
    }

    // ── metrics without auth ─────────────────────────────────────────────────

    #[test]
    fn metrics_without_token_warns() {
        let w = warns(r#"{ "port": 8080, "metrics": {} }"#);
        assert!(
            w.iter().any(|m| m.contains("metrics")),
            "metrics without token must warn: {w:?}"
        );
    }

    #[test]
    fn metrics_with_token_no_warn() {
        let w = warns(r#"{ "port": 8080, "metrics": { "token": "secret" } }"#);
        assert!(
            w.iter().all(|m| !m.contains("publicly")),
            "metrics with token must not warn about access: {w:?}"
        );
    }

    // ── forwardAuth SSRF to admin API ─────────────────────────────────────────

    #[test]
    fn forward_auth_to_admin_api_port_is_error() {
        let e = errs(r#"{ "port": 8080, "forwardAuth": { "url": "http://127.0.0.1:2019/auth" } }"#);
        assert!(
            e.iter().any(|err| err.message.contains("Admin API")),
            "forwardAuth pointing to admin port must be an error: {e:?}"
        );
    }

    #[test]
    fn forward_auth_to_normal_service_ok() {
        assert!(
            errs(
                r#"{ "port": 8080, "forwardAuth": { "url": "http://auth-service:4000/verify" } }"#
            )
            .iter()
            .all(|e| !e.message.contains("Admin API")),
            "forwardAuth to external service must not warn about admin API"
        );
    }

    // ── empty API key ─────────────────────────────────────────────────────────

    #[test]
    fn empty_api_key_is_rejected() {
        // An empty key creates a bypass: clients without the header have provided=""
        // which matches the empty key.
        let e = errs(r#"{ "port": 8080, "apiKey": { "keys": [""] } }"#);
        assert!(!e.is_empty(), "empty API key must be rejected");
        assert!(
            e.iter().any(|err| err.message.contains("empty")),
            "error must mention empty key: {e:?}"
        );
    }

    #[test]
    fn non_empty_api_key_is_valid() {
        assert!(
            errs(r#"{ "port": 8080, "apiKey": { "keys": ["secret-key-123"] } }"#).is_empty(),
            "non-empty API key must pass validation"
        );
    }

    #[test]
    fn empty_keys_list_is_rejected() {
        let e = errs(r#"{ "port": 8080, "apiKey": { "keys": [] } }"#);
        assert!(!e.is_empty(), "empty keys list must be rejected");
    }

    #[test]
    fn consumer_empty_api_key_is_rejected() {
        let e = errs(
            r#"{
            "port": 8080,
            "consumers": { "consumers": [{ "username": "bob", "apiKey": "" }] }
        }"#,
        );
        assert!(
            e.iter().any(|err| err.message.contains("empty")),
            "consumer empty apiKey must be rejected: {e:?}"
        );
    }

    // ── loopback_port helper ─────────────────────────────────────────────────

    #[test]
    fn loopback_localhost_explicit_port_detected() {
        // localhost with an explicit port should trigger loop detection.
        let w = warns(r#"{ "port": 9000, "proxy": "http://localhost:9000" }"#);
        assert!(
            w.iter().any(|m| m.contains("loop")),
            "localhost loop must warn: {w:?}"
        );
    }

    #[test]
    fn loopback_localhost_default_http_port() {
        // localhost without explicit port → defaults to 80 for http.
        let w = warns(r#"{ "port": 80, "proxy": "http://localhost" }"#);
        assert!(
            w.iter().any(|m| m.contains("loop")),
            "localhost:80 loop must warn: {w:?}"
        );
    }

    #[test]
    fn loopback_localhost_https_default_port() {
        // https://localhost without port → defaults to 443.
        let w = warns(
            r#"{ "port": 443, "tls": { "cert": "c.pem", "key": "c.key" },
                 "proxy": "https://localhost" }"#,
        );
        assert!(
            w.iter().any(|m| m.contains("loop")),
            "https://localhost:443 loop must warn: {w:?}"
        );
    }

    #[test]
    fn loopback_127_x_subnet_detected() {
        // 127.0.0.2 is still loopback.
        let w = warns(r#"{ "port": 8080, "proxy": "http://127.0.0.2:8080" }"#);
        assert!(
            w.iter().any(|m| m.contains("loop")),
            "127.0.0.2 loop must warn: {w:?}"
        );
    }

    #[test]
    fn non_loopback_host_no_loop_warn() {
        let w = warns(r#"{ "port": 8080, "proxy": "http://10.0.0.1:8080" }"#);
        assert!(
            w.iter().all(|m| !m.contains("loop")),
            "non-loopback must not warn: {w:?}"
        );
    }

    // ── is_valid_ip_or_cidr ──────────────────────────────────────────────────

    #[test]
    fn ipv6_cidr_valid_max_prefix() {
        // /128 is the maximum valid prefix for IPv6.
        assert!(
            errs(r#"{ "ipFilter": { "deny": ["::1/128"] } }"#).is_empty(),
            "::1/128 must be valid"
        );
    }

    #[test]
    fn ipv6_cidr_too_large_prefix_invalid() {
        let e = errs(r#"{ "ipFilter": { "deny": ["::1/129"] } }"#);
        assert!(!e.is_empty(), "/129 is invalid for IPv6");
    }

    #[test]
    fn ipv4_solo_address_valid() {
        assert!(
            errs(r#"{ "ipFilter": { "allow": ["192.168.1.100"] } }"#).is_empty(),
            "plain IPv4 address must be valid"
        );
    }

    #[test]
    fn ipv6_solo_address_valid() {
        assert!(
            errs(r#"{ "ipFilter": { "allow": ["2001:db8::1"] } }"#).is_empty(),
            "plain IPv6 address must be valid"
        );
    }

    // ── feature_warnings for various features ────────────────────────────────

    #[test]
    #[cfg(not(feature = "rhai"))]
    fn warning_for_rhai_middleware_without_feature() {
        let w = warns(
            r#"{ "port": 8080,
                 "middleware": [{ "type": "script", "path": "./filter.rhai" }] }"#,
        );
        assert!(
            w.iter().any(|m| m.contains("rhai")),
            "missing rhai feature must warn: {w:?}"
        );
    }

    #[test]
    #[cfg(not(feature = "forward-auth"))]
    fn warning_for_forward_auth_without_feature() {
        let w = warns(
            r#"{ "port": 8080,
                 "forwardAuth": { "url": "http://auth:4000/verify" } }"#,
        );
        assert!(
            w.iter().any(|m| m.contains("forward-auth")),
            "missing forward-auth feature must warn: {w:?}"
        );
    }

    #[test]
    #[cfg(not(feature = "tcp"))]
    fn warning_for_tcp_without_feature() {
        let w = warns(r#"{ "port": 3306, "tcp": { "targets": ["mysql:3306"] } }"#);
        assert!(
            w.iter().any(|m| m.contains("tcp")),
            "missing tcp feature must warn: {w:?}"
        );
    }

    #[test]
    #[cfg(not(feature = "fault-injection"))]
    fn warning_for_fault_injection_without_feature() {
        let w = warns(
            r#"{ "port": 8080,
                 "faultInjection": { "abort": { "percent": 50, "status": 503 } } }"#,
        );
        assert!(
            w.iter().any(|m| m.contains("fault-injection")),
            "missing fault-injection feature must warn: {w:?}"
        );
    }

    // ── TCP port conflict with HTTP sites ────────────────────────────────────

    #[test]
    #[cfg(feature = "tcp")]
    fn tcp_port_conflicts_with_http_site() {
        let e = errs(
            r#"[
                { "port": 8080 },
                { "port": 8080, "tcp": { "targets": ["db:5432"] } }
            ]"#,
        );
        assert!(
            !e.is_empty(),
            "TCP site on same port as HTTP site must error: {e:?}"
        );
    }

    // ── validate_rate_limit edge cases ───────────────────────────────────────

    #[test]
    fn rate_limit_zero_limit_is_rejected() {
        let e = errs(r#"{ "rateLimit": { "limit": 0, "windowSecs": 60 } }"#);
        assert!(!e.is_empty(), "limit=0 must be rejected");
        assert!(
            e.iter().any(|err| err.path.contains("rateLimit")),
            "error must point to rateLimit: {e:?}"
        );
    }

    #[test]
    fn rate_limit_zero_window_secs_is_rejected() {
        let e = errs(r#"{ "rateLimit": { "limit": 100, "windowSecs": 0 } }"#);
        assert!(!e.is_empty(), "windowSecs=0 must be rejected");
    }

    #[test]
    fn rate_limit_valid_config_no_errors() {
        assert!(
            errs(r#"{ "rateLimit": { "limit": 100, "windowSecs": 60 } }"#).is_empty(),
            "valid rate limit config must pass"
        );
    }

    // ── validate_consumers ────────────────────────────────────────────────────

    #[test]
    fn consumer_empty_username_is_rejected() {
        let e = errs(
            r#"{ "port": 8080,
                 "consumers": { "consumers": [{ "username": "", "apiKey": "key" }] } }"#,
        );
        assert!(
            e.iter().any(|err| err.message.contains("username")),
            "empty username must be rejected: {e:?}"
        );
    }

    #[test]
    fn consumer_duplicate_usernames_rejected() {
        let e = errs(
            r#"{ "port": 8080,
                 "consumers": { "consumers": [
                     { "username": "alice", "apiKey": "key1" },
                     { "username": "alice", "apiKey": "key2" }
                 ] } }"#,
        );
        assert!(
            e.iter().any(|err| err.message.contains("duplicated")),
            "duplicate username must be rejected: {e:?}"
        );
    }

    #[test]
    fn consumer_without_credentials_is_rejected() {
        // No apiKey, no basicAuth, no jwt → missing credentials.
        let e = errs(
            r#"{ "port": 8080,
                 "consumers": { "consumers": [{ "username": "nobody" }] } }"#,
        );
        assert!(
            !e.is_empty(),
            "consumer without any credentials must be rejected"
        );
    }

    #[test]
    fn consumer_empty_basic_auth_password_rejected() {
        let e = errs(
            r#"{ "port": 8080,
                 "consumers": { "consumers": [
                     { "username": "alice", "basicAuth": { "password": "" } }
                 ] } }"#,
        );
        assert!(
            e.iter().any(|err| err.path.contains("basicAuth")),
            "empty basicAuth password must be rejected: {e:?}"
        );
    }

    #[test]
    fn consumer_valid_config_no_errors() {
        let e = errs(
            r#"{ "port": 8080,
                 "consumers": { "consumers": [
                     { "username": "alice", "apiKey": "valid-key" }
                 ] } }"#,
        );
        assert!(e.is_empty(), "valid consumer must pass: {e:?}");
    }

    // ── validate_forward_auth extra cases ────────────────────────────────────

    #[test]
    fn forward_auth_zero_timeout_rejected() {
        let e = errs(
            r#"{ "port": 8080, "forwardAuth": { "url": "http://auth:4000/verify", "timeoutMs": 0 } }"#,
        );
        assert!(
            e.iter().any(|err| err.path.contains("timeoutMs")),
            "timeoutMs=0 must be rejected: {e:?}"
        );
    }

    #[test]
    fn forward_auth_empty_url_rejected() {
        let e = errs(r#"{ "port": 8080, "forwardAuth": { "url": "" } }"#);
        assert!(!e.is_empty(), "empty forwardAuth URL must be rejected");
    }

    #[test]
    fn forward_auth_non_http_url_rejected() {
        let e = errs(r#"{ "port": 8080, "forwardAuth": { "url": "grpc://auth:4000/verify" } }"#);
        assert!(
            !e.is_empty(),
            "non-http(s) forwardAuth URL must be rejected"
        );
    }

    // ── validate_limits ───────────────────────────────────────────────────────

    #[test]
    fn limits_zero_inflight_requests_rejected() {
        let e = errs(r#"{ "port": 8080, "limits": { "maxInflightRequests": 0 } }"#);
        assert!(!e.is_empty(), "maxInflightRequests=0 must be rejected");
        assert!(
            e.iter().any(|err| err.path.contains("maxInflightRequests")),
            "error path must mention maxInflightRequests: {e:?}"
        );
    }

    #[test]
    fn limits_zero_max_body_bytes_rejected() {
        let e = errs(r#"{ "port": 8080, "limits": { "maxBodyBytes": 0 } }"#);
        assert!(!e.is_empty(), "maxBodyBytes=0 must be rejected");
    }

    #[test]
    fn limits_valid_config_no_errors() {
        let e = errs(
            r#"{ "port": 8080, "limits": { "maxInflightRequests": 100, "maxBodyBytes": 1048576 } }"#,
        );
        assert!(e.is_empty(), "valid limits config must pass: {e:?}");
    }

    #[test]
    fn limits_zero_timeout_secs_rejected() {
        let e = errs(r#"{ "port": 8080, "limits": { "timeoutSecs": 0 } }"#);
        assert!(!e.is_empty(), "timeoutSecs=0 must be rejected");
        assert!(
            e.iter().any(|err| err.path.contains("timeoutSecs")),
            "error must mention timeoutSecs: {e:?}"
        );
    }

    #[test]
    fn limits_timeout_secs_valid() {
        let e = errs(r#"{ "port": 8080, "limits": { "timeoutSecs": 30 } }"#);
        assert!(e.is_empty(), "valid timeoutSecs must pass: {e:?}");
    }

    // ── TCP + static conflict ─────────────────────────────────────────────────

    #[test]
    #[cfg(feature = "tcp")]
    fn tcp_combined_with_static_rejected() {
        let e =
            errs(r#"{ "port": 3306, "tcp": { "targets": ["mysql:3306"] }, "static": "./dist" }"#);
        assert!(!e.is_empty(), "tcp + static must be rejected");
    }

    // ── validate_jwt_auth ─────────────────────────────────────────────────────

    #[test]
    fn jwt_auth_without_secret_or_jwks_rejected() {
        // No secret and no jwksUrl → configuration error.
        let e = errs(r#"{ "port": 8080, "jwtAuth": {} }"#);
        assert!(
            !e.is_empty(),
            "jwtAuth without secret or jwksUrl must be rejected"
        );
    }

    #[test]
    fn jwt_auth_with_both_secret_and_jwks_rejected() {
        let e = errs(
            r#"{ "port": 8080, "jwtAuth": { "secret": "abc", "jwksUrl": "https://a.com/.well-known/jwks.json" } }"#,
        );
        assert!(
            !e.is_empty(),
            "jwtAuth with both secret and jwksUrl must be rejected"
        );
    }

    #[test]
    fn jwt_auth_with_only_secret_valid() {
        // A short secret will warn but not error.
        let e = errs(
            r#"{ "port": 8080, "jwtAuth": { "secret": "my-secret-key-that-is-long-enough-32b" } }"#,
        );
        assert!(e.is_empty(), "jwtAuth with valid secret must pass: {e:?}");
    }

    #[test]
    fn jwt_auth_with_invalid_jwks_url_rejected() {
        let e = errs(r#"{ "port": 8080, "jwtAuth": { "jwksUrl": "not-a-url" } }"#);
        assert!(!e.is_empty(), "invalid jwksUrl must be rejected");
    }

    #[test]
    fn jwt_auth_jwks_refresh_secs_below_minimum_rejected() {
        let e = errs(
            r#"{ "port": 8080, "jwtAuth": {
                 "jwksUrl": "https://a.com/.well-known/jwks.json",
                 "jwksRefreshSecs": 5
            } }"#,
        );
        assert!(
            e.iter().any(|err| err.path.contains("jwksRefreshSecs")),
            "jwksRefreshSecs below 60 must be rejected: {e:?}"
        );
    }

    #[test]
    fn jwt_auth_jwks_refresh_secs_at_minimum_valid() {
        let e = errs(
            r#"{ "port": 8080, "jwtAuth": {
                 "jwksUrl": "https://a.com/.well-known/jwks.json",
                 "jwksRefreshSecs": 60
            } }"#,
        );
        assert!(e.is_empty(), "jwksRefreshSecs=60 must pass: {e:?}");
    }

    // ── check_cert_expiry (via validate_tls) ─────────────────────────────────

    #[test]
    fn cert_expiry_valid_cert_no_error() {
        // Generate a self-signed cert valid for 1 year and verify no expiry error.
        let key_pair = rcgen::KeyPair::generate().expect("keygen");
        let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .expect("params")
            .self_signed(&key_pair)
            .expect("self-signed");
        let cert_pem = cert.pem();
        let key_pem = key_pair.serialize_pem();

        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        std::fs::write(&cert_path, &cert_pem).unwrap();
        std::fs::write(&key_path, &key_pem).unwrap();

        // Use forward slashes in JSON path (avoid Windows backslash escaping issues).
        let cert_str = cert_path.to_string_lossy().replace('\\', "/");
        let key_str = key_path.to_string_lossy().replace('\\', "/");
        let config_json =
            format!(r#"{{ "port": 443, "tls": {{ "cert": "{cert_str}", "key": "{key_str}" }} }}"#);
        let e = errs(&config_json);
        // A freshly generated cert valid for 1 year must not produce expiry errors.
        assert!(
            !e.iter().any(|err| err.message.contains("expired")),
            "fresh cert must not produce expiry error: {e:?}"
        );
    }

    // ── validate_tls mTLS clientAuth ─────────────────────────────────────────

    #[test]
    fn mtls_empty_ca_path_rejected() {
        let e = errs(
            r#"{ "port": 443, "tls": { "cert": "server.pem", "key": "server.key",
                 "clientAuth": { "ca": "" } } }"#,
        );
        assert!(
            e.iter().any(|err| err.path.contains("clientAuth")),
            "empty CA path must be rejected: {e:?}"
        );
    }

    #[test]
    fn mtls_without_tls_rejected() {
        // clientAuth without cert+key or acme must be rejected.
        let e = errs(r#"{ "port": 8080, "tls": { "clientAuth": { "ca": "ca.pem" } } }"#);
        assert!(
            e.iter()
                .any(|err| err.message.contains("cert") || err.path.contains("clientAuth")),
            "clientAuth without cert/key must be rejected: {e:?}"
        );
    }

    // ── is_valid_upstream_url ─────────────────────────────────────────────────

    #[test]
    fn upstream_url_http_valid() {
        let e =
            errs(r#"{ "port": 8080, "proxy": { "/": { "targets": ["http://backend:4000"] } } }"#);
        assert!(e.is_empty(), "valid http upstream must pass: {e:?}");
    }

    #[test]
    fn upstream_url_no_scheme_rejected() {
        let e = errs(r#"{ "port": 8080, "proxy": { "/": { "targets": ["backend:4000"] } } }"#);
        assert!(
            !e.is_empty(),
            "upstream without scheme must be rejected: {e:?}"
        );
    }

    #[test]
    fn upstream_url_https_valid() {
        let e = errs(
            r#"{ "port": 8080, "proxy": { "/": { "targets": ["https://api.example.com:443"] } } }"#,
        );
        assert!(e.is_empty(), "valid https upstream must pass: {e:?}");
    }

    // ── validate_rewrite_rules ────────────────────────────────────────────────

    #[test]
    fn rewrite_rule_invalid_regex_rejected() {
        let e = errs(
            r#"{ "port": 8080, "proxy": {
                "/": { "targets": ["http://b:4000"],
                        "rewrite": [{ "from": "[invalid", "to": "/v2/$1" }] } } }"#,
        );
        assert!(
            !e.is_empty(),
            "invalid rewrite regex must be rejected: {e:?}"
        );
    }

    #[test]
    fn rewrite_rule_valid_regex_passes() {
        let e = errs(
            r#"{ "port": 8080, "proxy": {
                "/": { "targets": ["http://b:4000"],
                        "rewrite": [{ "from": "^/v1/(.*)", "to": "/v2/$1" }] } } }"#,
        );
        assert!(e.is_empty(), "valid rewrite regex must pass: {e:?}");
    }

    // ── validate_route_config: mirror URL ────────────────────────────────────

    #[test]
    fn mirror_url_http_valid() {
        let e = errs(
            r#"{ "port": 8080, "proxy": {
                "/": { "targets": ["http://b:4000"],
                        "mirror": "http://mirror:4001" } } }"#,
        );
        assert!(e.is_empty(), "valid mirror URL must pass: {e:?}");
    }

    #[test]
    fn mirror_url_no_scheme_rejected() {
        let e = errs(
            r#"{ "port": 8080, "proxy": {
                "/": { "targets": ["http://b:4000"],
                        "mirror": "mirror:4001" } } }"#,
        );
        assert!(
            !e.is_empty(),
            "mirror URL without scheme must be rejected: {e:?}"
        );
    }

    // ── loopback_port with IPv6 ───────────────────────────────────────────────

    #[test]
    fn loopback_ipv6_loop_detected() {
        let w = warns(r#"{ "port": 8080, "proxy": "http://[::1]:8080" }"#);
        assert!(
            w.iter().any(|m| m.contains("loop")),
            "IPv6 loopback loop must warn: {w:?}"
        );
    }

    // ── validate_upload: empty dir ───────────────────────────────────────────

    #[test]
    fn upload_whitespace_dir_rejected() {
        let e = errs(r#"{ "upload": { "path": "/upload", "dir": "   " } }"#);
        assert!(!e.is_empty(), "whitespace-only dir must be rejected");
    }

    // ── check_weighted_targets ────────────────────────────────────────────────

    #[test]
    fn weighted_round_robin_with_simple_target_rejected() {
        let e = errs(
            r#"{ "port": 8080, "proxy": {
                "/": {
                    "targets": ["http://b:4000"],
                    "strategy": "weighted-round-robin"
                }
            } }"#,
        );
        assert!(
            !e.is_empty(),
            "simple target with weighted-round-robin must be rejected: {e:?}"
        );
        assert!(
            e.iter().any(|err| err.message.contains("weighted")),
            "error must mention weighted: {e:?}"
        );
    }

    #[test]
    fn weighted_round_robin_with_weighted_target_passes() {
        let e = errs(
            r#"{ "port": 8080, "proxy": {
                "/": {
                    "targets": [{ "url": "http://b:4000", "weight": 3 }],
                    "strategy": "weighted-round-robin"
                }
            } }"#,
        );
        assert!(
            e.is_empty(),
            "weighted target with weighted-round-robin must pass: {e:?}"
        );
    }

    // ── validate_groups_config ────────────────────────────────────────────────

    #[test]
    fn group_with_empty_targets_rejected() {
        let e = errs(
            r#"{ "port": 8080, "proxy": {
                "/": {
                    "targets": [],
                    "groups": [{ "name": "empty-group", "targets": [] }]
                }
            } }"#,
        );
        assert!(
            !e.is_empty(),
            "group with no targets must be rejected: {e:?}"
        );
        assert!(
            e.iter().any(|err| err.path.contains("groups")),
            "error must mention groups: {e:?}"
        );
    }

    #[test]
    fn group_with_targets_passes() {
        let e = errs(
            r#"{ "port": 8080, "proxy": {
                "/": {
                    "targets": [],
                    "groups": [{ "name": "g1", "targets": [{ "url": "http://b:4000", "weight": 1 }] }]
                }
            } }"#,
        );
        assert!(e.is_empty(), "group with targets must pass: {e:?}");
    }

    // ── validate_proxy_route_target: RoundRobin ───────────────────────────────

    #[test]
    fn round_robin_with_invalid_url_rejected() {
        let e = errs(
            r#"{ "port": 8080, "proxy": {
                "/": ["not-a-url", "http://valid:4000"]
            } }"#,
        );
        assert!(
            !e.is_empty(),
            "RoundRobin with invalid URL must be rejected: {e:?}"
        );
    }

    #[test]
    fn round_robin_all_valid_passes() {
        let e = errs(
            r#"{ "port": 8080, "proxy": {
                "/": ["http://a:4000", "http://b:4000"]
            } }"#,
        );
        assert!(e.is_empty(), "valid RoundRobin must pass: {e:?}");
    }

    // ── validate_proxy: Single with invalid URL ────────────────────────────────

    #[test]
    fn single_proxy_invalid_url_rejected() {
        let e = errs(r#"{ "port": 8080, "proxy": "not-a-url" }"#);
        assert!(
            !e.is_empty(),
            "Single proxy with invalid URL must be rejected: {e:?}"
        );
    }

    // ── IPv6 TCP targets ──────────────────────────────────────────────────────

    #[test]
    #[cfg(feature = "tcp")]
    fn ipv6_tcp_target_valid() {
        let e = errs(r#"{ "port": 3306, "tcp": { "targets": ["[::1]:3306"] } }"#);
        assert!(e.is_empty(), "IPv6 TCP target must pass: {e:?}");
    }

    // ── proxy loop detection via routes[] array ───────────────────────────────

    #[test]
    fn proxy_loop_detection_via_routes_array() {
        let w = warns(
            r#"{ "port": 8080, "routes": [{ "match": { "path": "/**" }, "proxy": "http://127.0.0.1:8080" }] }"#,
        );
        assert!(
            w.iter().any(|m| m.contains("loop")),
            "routes array pointing back to self must warn: {w:?}"
        );
    }

    #[test]
    fn proxy_loop_detection_routes_array_external_no_warn() {
        let w = warns(
            r#"{ "port": 8080, "routes": [{ "match": { "path": "/**" }, "proxy": "http://api.example.com:8080" }] }"#,
        );
        assert!(
            w.iter().all(|m| !m.contains("loop")),
            "external host must not warn: {w:?}"
        );
    }

    // ── consumers.sharedJwt validation ───────────────────────────────────────

    #[test]
    fn consumers_shared_jwt_no_secret_or_jwks_rejected() {
        let e = errs(
            r#"{ "port": 8080,
                 "consumers": { "consumers": [{ "username": "u", "apiKey": "k" }],
                                "sharedJwt": {} } }"#,
        );
        assert!(
            !e.is_empty(),
            "sharedJwt without secret or jwksUrl must error: {e:?}"
        );
    }
}
