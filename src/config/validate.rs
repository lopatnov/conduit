use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use crate::config::schema::{
    AppConfig, FallbackConfig, IpFilterConfig, LoadBalanceStrategy, MetricsConfig, MiddlewareEntry,
    ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RateLimitConfig, RedirectRule,
    RewriteRule, SiteConfig, TlsConfig, UploadConfig,
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
/// ```
/// for w in feature_warnings(&config) {
///     tracing::warn!("{w}");
/// }
/// ```
pub fn feature_warnings(config: &AppConfig) -> Vec<String> {
    let mut warnings: Vec<String> = Vec::new();

    // ── global.otlp ───────────────────────────────────────────────────────────
    #[cfg(not(feature = "otlp"))]
    if config.global.as_ref().and_then(|g| g.otlp.as_ref()).is_some() {
        warnings.push(
            "global.otlp is configured but Conduit was compiled without the `otlp` feature \
             — OpenTelemetry tracing will be disabled. \
             Recompile with `--features otlp` to enable."
                .to_owned(),
        );
    }

    // ── middleware type: "wasm" ───────────────────────────────────────────────
    #[cfg(not(feature = "wasm"))]
    {
        for (i, site) in config.sites.iter().enumerate() {
            if let Some(middleware) = &site.middleware {
                let wasm_entries: Vec<usize> = middleware
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.r#type == "wasm")
                    .map(|(j, _)| j)
                    .collect();
                for j in wasm_entries {
                    warnings.push(format!(
                        "sites[{i}].middleware[{j}] has type \"wasm\" but Conduit was compiled \
                         without the `wasm` feature — this middleware entry will be ignored. \
                         Recompile with `--features wasm` to enable."
                    ));
                }
            }
        }
    }

    // ── Weak JWT secrets ─────────────────────────────────────────────────────
    // JWT HMAC secrets shorter than 32 bytes can be brute-forced or guessed.
    // RFC 7518 requires secrets be at least as long as the hash output
    // (32 bytes for HS256, 48 for HS384, 64 for HS512).  32 bytes minimum is
    // the practical floor.
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
        // Check consumer-level JWT secrets.
        if let Some(consumers_cfg) = &site.consumers {
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
    }

    // ── Metrics endpoint without authentication ───────────────────────────────
    // The /__metrics__ endpoint exposes internal counters (upstreams, routes,
    // error rates, etc.) that help attackers map the system topology.
    // A token should always be configured in production.
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

    warnings
}

// ── Cross-site checks ──────────────────────────────────────────────────────

fn effective_port(site: &SiteConfig) -> u16 {
    site.port
        .unwrap_or(if site.tls.is_some() { 443 } else { 80 })
}

fn validate_no_duplicate_host_port(config: &AppConfig, errors: &mut Vec<ValidationError>) {
    let mut seen: HashMap<(String, u16), usize> = HashMap::new();
    for (i, site) in config.sites.iter().enumerate() {
        let host = site.host.clone().unwrap_or_else(|| "*".to_string());
        let port = effective_port(site);
        if let Some(prev) = seen.insert((host.clone(), port), i) {
            errors.push(ValidationError::new(
                format!("sites[{i}]"),
                format!("Duplicate host+port '{host}:{port}' — already defined at sites[{prev}]"),
            ));
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

    // TCP proxy site validation.
    if let Some(tcp) = &site.tcp {
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
                    format!(
                        "TCP target \"{t}\" must be a plain host:port — no http:// prefix"
                    ),
                ));
            } else if !t.contains(':') {
                errors.push(ValidationError::new(
                    format!("{prefix}.tcp.targets[{i}]"),
                    format!("TCP target \"{t}\" must include a port, e.g. \"host:3306\""),
                ));
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

    if let Some(proxy) = &site.proxy {
        validate_proxy(proxy, &format!("{prefix}.proxy"), errors);
    }
    // Validate each entry in the `routes` array (Phase 3.6).
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

fn validate_consumers(
    cfg: &crate::config::schema::ConsumersConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    // Validate sharedJwt config if present.
    if let Some(ref sj) = cfg.shared_jwt {
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

    let mut seen_usernames = std::collections::HashSet::new();
    for (i, c) in cfg.consumers.iter().enumerate() {
        let entry_prefix = format!("{prefix}.consumers[{i}]");
        if c.username.is_empty() {
            errors.push(ValidationError::new(
                format!("{entry_prefix}.username"),
                "consumer username must not be empty",
            ));
        }
        // When sharedJwt is configured at the ConsumersConfig level, individual
        // consumers don't need their own credentials — they're identified by the
        // sharedJwt sub claim.  A credential is only required when sharedJwt is absent.
        let has_shared_jwt = cfg.shared_jwt.is_some();
        if !has_shared_jwt && c.api_key.is_none() && c.basic_auth.is_none() && c.jwt.is_none() {
            errors.push(ValidationError::new(
                entry_prefix.clone(),
                "consumer requires at least one credential: apiKey, basicAuth, jwt (or configure consumers.sharedJwt)",
            ));
        }
        if let Some(ref jwt_cfg) = c.jwt {
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
    } else if !cfg.url.starts_with("http://") && !cfg.url.starts_with("https://") {
        errors.push(ValidationError::new(
            format!("{prefix}.url"),
            "forwardAuth.url must be an http:// or https:// URL",
        ));
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

    // mTLS client auth — ca path must be non-empty; file existence checked at startup.
    if let Some(ref ca) = tls.client_auth {
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

    // Cert expiry check — only for manual certificates (ACME manages renewal itself).
    if !has_acme {
        if let Some(ref cert_path) = tls.cert {
            check_cert_expiry(cert_path, &format!("{prefix}.cert"), errors);
        }
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
        if swr as u64 > ttl * 10 {
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
        assert!(!e.is_empty(), "http:// prefix must be rejected for TCP targets");
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
        assert!(w[0].contains("wasm"), "warning must mention 'wasm': {}", w[0]);
        assert!(
            w[0].contains("--features wasm"),
            "warning must mention compile flag: {}",
            w[0]
        );
    }

    #[test]
    #[cfg(not(feature = "wasm"))]
    fn warning_per_wasm_entry() {
        // Two wasm entries → two warnings (one per entry).
        let w = warns(
            r#"{ "port": 8080,
                 "middleware": [
                     { "type": "wasm", "path": "a.wasm" },
                     { "type": "script", "path": "b.rhai" },
                     { "type": "wasm", "path": "c.wasm" }
                 ] }"#,
        );
        assert_eq!(w.len(), 2, "two wasm entries → two warnings: {w:?}");
    }

    #[test]
    #[cfg(not(feature = "otlp"))]
    fn warning_for_otlp_without_feature() {
        let w = warns(
            r#"{ "global": { "otlp": { "endpoint": "http://otel:4317" } },
                 "sites": [{ "port": 8080 }] }"#,
        );
        assert_eq!(w.len(), 1, "expected exactly one otlp warning: {w:?}");
        assert!(w[0].contains("otlp"), "warning must mention 'otlp': {}", w[0]);
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
}
