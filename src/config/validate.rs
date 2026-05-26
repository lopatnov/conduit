use std::collections::HashMap;

use crate::config::schema::{
    AppConfig, FallbackConfig, IpFilterConfig, LoadBalanceStrategy, ProxyConfig, ProxyRouteConfig,
    ProxyRouteTarget, ProxyTarget, RateLimitConfig, RedirectRule, RewriteRule, SiteConfig,
    TlsConfig,
};

// ── Public API ─────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Clone)]
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
    if let Some(rate_limit) = &site.rate_limit {
        validate_rate_limit(rate_limit, prefix, errors);
    }
    if let Some(redirects) = &site.redirects {
        validate_redirect_rules(redirects, prefix, errors);
    }
    if let Some(fb) = &site.fallback {
        validate_fallback(fb, prefix, errors);
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
    // When `groups` is configured, `targets` may be empty.
    if cfg.targets.is_empty() && cfg.groups.is_none() {
        errors.push(ValidationError::new(
            format!("{prefix}.targets"),
            "At least one target is required (or use 'groups' for two-level balancing)",
        ));
    }
    if let Some(groups) = &cfg.groups {
        for (i, group) in groups.iter().enumerate() {
            if group.targets.is_empty() {
                errors.push(ValidationError::new(
                    format!("{prefix}.groups[{i}].targets"),
                    "At least one target is required in each group",
                ));
            }
            // Within a group, weighted-round-robin also requires weighted targets.
            if group.strategy == Some(LoadBalanceStrategy::WeightedRoundRobin) {
                let has_simple = group
                    .targets
                    .iter()
                    .any(|t| matches!(t, ProxyTarget::Simple(_)));
                if has_simple {
                    errors.push(ValidationError::new(
                        format!("{prefix}.groups[{i}].targets"),
                        "Strategy 'weighted-round-robin' requires weighted targets: \
                         { \"url\": \"...\", \"weight\": N }",
                    ));
                }
            }
        }
    }

    if cfg.strategy == Some(LoadBalanceStrategy::WeightedRoundRobin) {
        let has_simple = cfg
            .targets
            .iter()
            .any(|t| matches!(t, ProxyTarget::Simple(_)));
        if has_simple {
            errors.push(ValidationError::new(
                format!("{prefix}.targets"),
                "Strategy 'weighted-round-robin' requires weighted targets: \
                 { \"url\": \"...\", \"weight\": N }",
            ));
        }
    }

    // Validate target URLs.
    for (i, target) in cfg.targets.iter().enumerate() {
        let url = match target {
            ProxyTarget::Simple(u)   => u.as_str(),
            ProxyTarget::Weighted(w) => w.url.as_str(),
        };
        if !is_valid_upstream_url(url) {
            errors.push(ValidationError::new(
                format!("{prefix}.targets[{i}]"),
                format!("Invalid upstream URL '{url}' — must start with http:// or https://"),
            ));
        }
    }

    // Validate rewrite rule regexes at config-load time so bad patterns are
    // caught by `conduit validate` rather than silently failing at request time.
    if let Some(rules) = &cfg.rewrite {
        validate_rewrite_rules(rules, prefix, errors);
    }
}

fn validate_rewrite_rules(
    rules: &[RewriteRule],
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
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
        assert!(
            errs(
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
            .is_empty()
        );
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
        assert!(!e.is_empty(), "invalid regex in routes array must be caught");
    }
}
