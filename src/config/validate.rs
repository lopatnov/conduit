use std::collections::HashMap;

use crate::config::schema::{
    AppConfig, FallbackConfig, LoadBalanceStrategy, ProxyConfig, ProxyRouteConfig,
    ProxyRouteTarget, ProxyTarget, RateLimitConfig, RedirectRule, SiteConfig, TlsConfig,
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
    if let ProxyConfig::Routes(routes) = proxy {
        for (route, target) in routes {
            if let ProxyRouteTarget::Full(cfg) = target {
                validate_route_config(cfg, &format!("{prefix}[\"{route}\"]"), errors);
            }
        }
    }
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
}
