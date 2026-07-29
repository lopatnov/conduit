use crate::config::schema::{SecurityHeadersConfig, SecurityHeadersOptions};

/// Build the security response headers for the given config.
///
/// Returns an empty vec when `securityHeaders: false`.
pub fn header_entries(cfg: &SecurityHeadersConfig) -> Vec<(String, String)> {
    match cfg {
        SecurityHeadersConfig::Enabled(false) => vec![],
        SecurityHeadersConfig::Enabled(true) => defaults(),
        SecurityHeadersConfig::Options(opts) => custom(opts),
    }
}

/// Check whether the incoming `Host` header is allowed.
///
/// Returns `true` when the host is allowed. Returns `false` when the host is
/// explicitly rejected — the caller should return 400.
///
/// Matches `traefik AllowedHosts` pattern: blocks Host header injection attacks where
/// applications generate absolute URLs (e.g. password-reset links) from the untrusted Host.
///
/// Precedence:
/// 1. `securityHeaders.allowedHosts`, when configured, is authoritative — matched
///    against explicitly (or `"*"` to allow any host).
/// 2. Otherwise, fall back to the site's own `host:` config value (`site_host`), so
///    Host-header injection is rejected by default without requiring extra
///    configuration — a site declaring `host: example.com` implicitly only accepts
///    that Host. Catch-all sites (`host` unset or `"*"`) have no single expected
///    value, so they keep allowing any Host.
pub fn is_host_allowed(
    cfg: Option<&SecurityHeadersConfig>,
    site_host: Option<&str>,
    host: &str,
) -> bool {
    // Strip port from host for matching.
    let host_no_port = host.split(':').next().unwrap_or(host);

    if let Some(SecurityHeadersConfig::Options(opts)) = cfg {
        if let Some(allowed) = &opts.allowed_hosts {
            if allowed.is_empty() {
                return true;
            }
            return allowed
                .iter()
                .any(|a| a == "*" || a == host || a == host_no_port);
        }
    }

    match site_host {
        Some(h) if h != "*" => h.eq_ignore_ascii_case(host) || h.eq_ignore_ascii_case(host_no_port),
        _ => true, // catch-all site or no site host configured → allow any host
    }
}

fn defaults() -> Vec<(String, String)> {
    vec![
        ("X-Content-Type-Options".to_owned(), "nosniff".to_owned()),
        ("X-Frame-Options".to_owned(), "SAMEORIGIN".to_owned()),
        (
            "Referrer-Policy".to_owned(),
            "strict-origin-when-cross-origin".to_owned(),
        ),
        // X-XSS-Protection is deprecated in modern browsers (use CSP instead)
        // but kept for legacy browser compatibility.
        ("X-XSS-Protection".to_owned(), "1; mode=block".to_owned()),
    ]
}

fn custom(opts: &SecurityHeadersOptions) -> Vec<(String, String)> {
    let mut h = Vec::new();

    // Always included — no valid reason to omit them.
    h.push(("X-Content-Type-Options".to_owned(), "nosniff".to_owned()));
    h.push(("X-XSS-Protection".to_owned(), "1; mode=block".to_owned()));

    let frame = opts.x_frame_options.as_deref().unwrap_or("SAMEORIGIN");
    h.push(("X-Frame-Options".to_owned(), frame.to_owned()));

    let referrer = opts
        .referrer_policy
        .as_deref()
        .unwrap_or("strict-origin-when-cross-origin");
    h.push(("Referrer-Policy".to_owned(), referrer.to_owned()));

    // HSTS — compose the directive from individual flags.
    // traefik pattern: separate hstsMaxAgeSecs / hstsIncludeSubDomains / hstsPreload fields.
    if let Some(secs) = opts.hsts_max_age_secs {
        let mut hsts = format!("max-age={secs}");
        // includeSubDomains defaults to true when max-age is set (matches nginx default).
        if opts.hsts_include_subdomains.unwrap_or(true) {
            hsts.push_str("; includeSubDomains");
        }
        if opts.hsts_preload.unwrap_or(false) {
            hsts.push_str("; preload");
        }
        h.push(("Strict-Transport-Security".to_owned(), hsts));
    }

    if let Some(csp) = &opts.csp {
        h.push(("Content-Security-Policy".to_owned(), csp.clone()));
    }

    // Permissions-Policy — replaces deprecated Feature-Policy.
    // Controls access to browser APIs (camera, microphone, geolocation, etc.).
    // Pattern from traefik `PermissionsPolicy` option.
    if let Some(pp) = &opts.permissions_policy {
        h.push(("Permissions-Policy".to_owned(), pp.clone()));
    }

    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_returns_empty() {
        assert!(header_entries(&SecurityHeadersConfig::Enabled(false)).is_empty());
    }

    #[test]
    fn enabled_returns_defaults() {
        let h = header_entries(&SecurityHeadersConfig::Enabled(true));
        assert!(h
            .iter()
            .any(|(k, v)| k == "X-Content-Type-Options" && v == "nosniff"));
        assert!(h
            .iter()
            .any(|(k, v)| k == "X-Frame-Options" && v == "SAMEORIGIN"));
        assert!(h.iter().any(|(k, _)| k == "Referrer-Policy"));
        assert!(h.iter().any(|(k, _)| k == "X-XSS-Protection"));
    }

    #[test]
    fn hsts_includes_subdomains_by_default() {
        let opts = SecurityHeadersOptions {
            hsts_max_age_secs: Some(31_536_000),
            ..Default::default()
        };
        let h = header_entries(&SecurityHeadersConfig::Options(opts));
        let sts = h
            .iter()
            .find(|(k, _)| k == "Strict-Transport-Security")
            .unwrap();
        assert!(
            sts.1.contains("includeSubDomains"),
            "includeSubDomains must be on by default"
        );
        assert!(!sts.1.contains("preload"), "preload must be off by default");
    }

    #[test]
    fn hsts_preload_and_no_subdomains() {
        let opts = SecurityHeadersOptions {
            hsts_max_age_secs: Some(31_536_000),
            hsts_include_subdomains: Some(false),
            hsts_preload: Some(true),
            ..Default::default()
        };
        let h = header_entries(&SecurityHeadersConfig::Options(opts));
        let sts = h
            .iter()
            .find(|(k, _)| k == "Strict-Transport-Security")
            .unwrap();
        assert!(!sts.1.contains("includeSubDomains"));
        assert!(sts.1.contains("preload"));
    }

    #[test]
    fn permissions_policy_injected() {
        let opts = SecurityHeadersOptions {
            permissions_policy: Some("geolocation=(), microphone=()".to_owned()),
            ..Default::default()
        };
        let h = header_entries(&SecurityHeadersConfig::Options(opts));
        assert!(h
            .iter()
            .any(|(k, v)| { k == "Permissions-Policy" && v.contains("geolocation=()") }));
    }

    #[test]
    fn options_with_hsts_and_csp() {
        let opts = SecurityHeadersOptions {
            hsts_max_age_secs: Some(31_536_000),
            csp: Some("default-src 'self'".to_owned()),
            x_frame_options: Some("DENY".to_owned()),
            ..Default::default()
        };
        let h = header_entries(&SecurityHeadersConfig::Options(opts));
        assert!(h
            .iter()
            .any(|(k, v)| k == "Strict-Transport-Security" && v.starts_with("max-age=31536000")));
        assert!(h
            .iter()
            .any(|(k, v)| k == "Content-Security-Policy" && v == "default-src 'self'"));
        assert!(h.iter().any(|(k, v)| k == "X-Frame-Options" && v == "DENY"));
    }

    // ── is_host_allowed ───────────────────────────────────────────────────────

    #[test]
    fn no_allowed_hosts_and_no_site_host_allows_everything() {
        let cfg = SecurityHeadersConfig::Enabled(true);
        assert!(is_host_allowed(Some(&cfg), None, "evil.com"));
        assert!(is_host_allowed(Some(&cfg), None, "api.example.com"));
        assert!(is_host_allowed(None, None, "evil.com"));
    }

    #[test]
    fn allowed_hosts_rejects_unknown() {
        use crate::config::schema::SecurityHeadersOptions;
        let opts = SecurityHeadersOptions {
            allowed_hosts: Some(vec!["api.example.com".to_owned()]),
            ..Default::default()
        };
        let cfg = SecurityHeadersConfig::Options(opts);
        assert!(is_host_allowed(Some(&cfg), None, "api.example.com"));
        assert!(!is_host_allowed(Some(&cfg), None, "evil.com"));
    }

    #[test]
    fn allowed_hosts_strips_port() {
        use crate::config::schema::SecurityHeadersOptions;
        let opts = SecurityHeadersOptions {
            allowed_hosts: Some(vec!["api.example.com".to_owned()]),
            ..Default::default()
        };
        let cfg = SecurityHeadersConfig::Options(opts);
        // Host header often includes port: "api.example.com:8080"
        assert!(is_host_allowed(Some(&cfg), None, "api.example.com:8080"));
    }

    #[test]
    fn wildcard_allows_all() {
        use crate::config::schema::SecurityHeadersOptions;
        let opts = SecurityHeadersOptions {
            allowed_hosts: Some(vec!["*".to_owned()]),
            ..Default::default()
        };
        let cfg = SecurityHeadersConfig::Options(opts);
        assert!(is_host_allowed(Some(&cfg), None, "anything.example.com"));
    }

    #[test]
    fn site_host_fallback_rejects_mismatch_when_allowed_hosts_unset() {
        // No `securityHeaders` at all — a site declaring `host: example.com`
        // must still reject a forged Host header by default (#password-reset
        // link poisoning / Host header injection).
        assert!(is_host_allowed(None, Some("example.com"), "example.com"));
        assert!(!is_host_allowed(None, Some("example.com"), "evil.com"));
    }

    #[test]
    fn site_host_fallback_is_case_insensitive_and_strips_port() {
        assert!(is_host_allowed(
            None,
            Some("example.com"),
            "EXAMPLE.com:8080"
        ));
    }

    #[test]
    fn site_host_fallback_applies_even_with_bool_security_headers() {
        let cfg = SecurityHeadersConfig::Enabled(true);
        assert!(!is_host_allowed(Some(&cfg), Some("example.com"), "evil.com"));
    }

    #[test]
    fn catch_all_site_host_allows_everything() {
        assert!(is_host_allowed(None, Some("*"), "evil.com"));
        assert!(is_host_allowed(None, None, "evil.com"));
    }

    #[test]
    fn explicit_allowed_hosts_takes_precedence_over_site_host() {
        use crate::config::schema::SecurityHeadersOptions;
        let opts = SecurityHeadersOptions {
            allowed_hosts: Some(vec!["api.example.com".to_owned()]),
            ..Default::default()
        };
        let cfg = SecurityHeadersConfig::Options(opts);
        // site_host says "example.com" but allowedHosts explicitly allows a
        // different host — the explicit list wins.
        assert!(is_host_allowed(
            Some(&cfg),
            Some("example.com"),
            "api.example.com"
        ));
    }

    // ── HSTS edge cases ───────────────────────────────────────────────────────

    #[test]
    fn hsts_without_subdomains() {
        use crate::config::schema::SecurityHeadersOptions;
        let opts = SecurityHeadersOptions {
            hsts_max_age_secs: Some(31536000),
            hsts_include_subdomains: Some(false), // explicitly disabled
            hsts_preload: None,
            ..Default::default()
        };
        let cfg = SecurityHeadersConfig::Options(opts);
        let h = header_entries(&cfg);
        let hsts = h.iter().find(|(k, _)| k == "Strict-Transport-Security");
        assert!(hsts.is_some(), "HSTS header must be present");
        assert!(
            !hsts.unwrap().1.contains("includeSubDomains"),
            "hsts_include_subdomains=false must omit directive"
        );
    }

    #[test]
    fn hsts_with_preload() {
        use crate::config::schema::SecurityHeadersOptions;
        let opts = SecurityHeadersOptions {
            hsts_max_age_secs: Some(31536000),
            hsts_preload: Some(true),
            ..Default::default()
        };
        let cfg = SecurityHeadersConfig::Options(opts);
        let h = header_entries(&cfg);
        let hsts = h.iter().find(|(k, _)| k == "Strict-Transport-Security");
        assert!(hsts.is_some());
        assert!(
            hsts.unwrap().1.contains("preload"),
            "preload=true must include 'preload'"
        );
    }
}
