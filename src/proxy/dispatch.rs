//! Local-path/site dispatch helpers (issue #143) — moved verbatim out of
//! `router.rs` in PR A2 (issue #419) to keep it under the
//! 400-production-line soft limit. These decide which special local path
//! (health/metrics/hot-reload/ACME) or which `SiteConfig` a request maps
//! to — orthogonal to proxy-target resolution (`conduit_proxy_http::resolve`/
//! `groups`/`routes_resolve`), so they get their own file rather than being
//! folded into one of those.
//!
//! Deliberately did NOT move into `conduit-proxy-http` in PR B (issue #143
//! itself), unlike the rest of this file's PR-A2 siblings: every function
//! here takes `&AppConfig`/`Option<&SiteConfig>` directly, and those are
//! still root-crate-only types (a separate, not-yet-started
//! config-schema-decomposition track — issues #314/#315/#316/#222). Moving
//! this file would have created a genuine circular dependency (this
//! function set needing types defined in the root crate, which depends on
//! `conduit-proxy-http`). See `crates/conduit-proxy-http/src/lib.rs`'s own
//! doc comment ("`dispatch.rs` deliberately did NOT move here") for the
//! full reasoning — confirmed via grep before this decision that none of
//! the files that DID move into that crate ever called any function here.

use crate::config::schema::{AppConfig, SiteConfig};

/// Returns `Some(token)` when `path` matches the configured metrics endpoint.
/// `token` is `None` when the endpoint has no auth token.
pub(crate) fn metrics_token(site: Option<&SiteConfig>, path: &str) -> Option<Option<String>> {
    let site = site?;
    let metrics = site.metrics.as_ref()?;
    let bare = path.split('?').next().unwrap_or(path);
    let metrics_path = metrics.path.as_deref().unwrap_or("/__metrics__");
    if bare == metrics_path {
        Some(metrics.token.clone())
    } else {
        None
    }
}

pub(crate) fn is_health_path(site: Option<&SiteConfig>, path: &str) -> bool {
    let bare = path.split('?').next().unwrap_or(path);
    let default_path = "/__health__";
    if let Some(site) = site {
        if let Some(hc) = &site.health_check {
            use crate::config::schema::HealthCheckConfig;
            match hc {
                HealthCheckConfig::Enabled(false) => return false,
                HealthCheckConfig::Enabled(true) => return bare == default_path,
                HealthCheckConfig::Options(opts) => {
                    let p = opts.path.as_deref().unwrap_or(default_path);
                    return bare == p;
                }
            }
        }
    }
    bare == default_path
}

/// Returns `true` when the path targets the SSE hot-reload endpoint
/// (`/__hot-reload__`) and the site has `hotReload` enabled.
///
/// Only matches when compiled with `--features hotreload` — without it, no
/// hot-reload handler exists to serve this path, so it must not win routing
/// precedence over the site's own `fallback`/`static`/`proxy` config (same
/// bug class as issue #341's ACME-challenge fix: previously matched
/// unconditionally whenever `hotReload` was configured, regardless of the
/// compiled feature — `HandlerKind::HotReloadSse`'s handler being `None`
/// without `hotreload` meant every request to this path would have fallen
/// through to Pingora's proxy path with no real upstream to select).
#[cfg(feature = "hotreload")]
pub(crate) fn is_hot_reload_sse_path(site: Option<&SiteConfig>, path: &str) -> bool {
    use crate::config::schema::HotReloadConfig;
    let Some(site) = site else { return false };
    let Some(hr) = &site.hot_reload else {
        return false;
    };
    if matches!(hr, HotReloadConfig::Enabled(false)) {
        return false;
    }
    let bare = path.split('?').next().unwrap_or(path);
    bare == "/__hot-reload__"
}

#[cfg(not(feature = "hotreload"))]
pub(crate) fn is_hot_reload_sse_path(_site: Option<&SiteConfig>, _path: &str) -> bool {
    false
}

/// Returns `true` when the path targets the hot-reload client JS file
/// (`/__hot-reload__/client.js`) and the site has `hotReload` enabled.
///
/// Only matches when compiled with `--features hotreload` — see
/// `is_hot_reload_sse_path`'s doc comment.
#[cfg(feature = "hotreload")]
pub(crate) fn is_hot_reload_js_path(site: Option<&SiteConfig>, path: &str) -> bool {
    use crate::config::schema::HotReloadConfig;
    let Some(site) = site else { return false };
    let Some(hr) = &site.hot_reload else {
        return false;
    };
    if matches!(hr, HotReloadConfig::Enabled(false)) {
        return false;
    }
    let bare = path.split('?').next().unwrap_or(path);
    bare == "/__hot-reload__/client.js"
}

#[cfg(not(feature = "hotreload"))]
pub(crate) fn is_hot_reload_js_path(_site: Option<&SiteConfig>, _path: &str) -> bool {
    false
}

/// If `path` starts with the ACME HTTP-01 challenge prefix, return the token
/// portion.  E.g. `/.well-known/acme-challenge/abc123` → `Some("abc123")`.
///
/// Only matches when compiled with `--features acme` — without it, no ACME
/// challenge handler exists to serve this path, so it must not win routing
/// precedence over the site's own `fallback`/`static`/`proxy` config
/// (issue #341: previously matched unconditionally regardless of the
/// feature, and `HandlerKind::AcmeChallenge`'s handler being `None` without
/// `acme` meant every request to this path fell through to Pingora's proxy
/// path with no real upstream to select, surfacing as a 502 instead of
/// whatever the site would otherwise have served).
#[cfg(feature = "acme")]
pub(crate) fn acme_challenge_token(path: &str) -> Option<&str> {
    path.strip_prefix("/.well-known/acme-challenge/")
}

#[cfg(not(feature = "acme"))]
pub(crate) fn acme_challenge_token(_path: &str) -> Option<&str> {
    None
}

pub(crate) fn find_site_idx(config: &AppConfig, host: &str, server_port: u16) -> Option<usize> {
    if config.sites.is_empty() {
        return None;
    }
    // 1st pass: sites with an explicit matching host.
    if let Some(idx) = find_host_match(&config.sites, host, server_port) {
        return Some(idx);
    }
    // 2nd pass: catch-all sites (no host configured, or host == "*").
    if let Some(idx) = find_wildcard_match(&config.sites, server_port) {
        return Some(idx);
    }
    Some(0)
}

/// Find the first site whose `host` equals `host`, preferring an exact port match.
fn find_host_match(sites: &[SiteConfig], host: &str, server_port: u16) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, site) in sites.iter().enumerate() {
        if site.host.as_deref() == Some(host) {
            if site.port == Some(server_port) {
                return Some(i); // exact host+port match
            }
            best.get_or_insert(i);
        }
    }
    best
}

/// Find the first catch-all site (no host or `host == "*"`), preferring port match.
fn find_wildcard_match(sites: &[SiteConfig], server_port: u16) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, site) in sites.iter().enumerate() {
        if matches!(site.host.as_deref(), None | Some("*")) {
            if site.port == Some(server_port) {
                return Some(i);
            }
            best.get_or_insert(i);
        }
    }
    best
}

/// Parse an RFC 9218 `Priority:` header value and convert to Conduit's 0–100 scale.
///
/// RFC 9218 format: `u=<urgency>[,i]` where urgency is 0 (highest) to 7 (lowest).
/// Mapping: `priority = 100 - urgency * 14`
///
/// Returns `None` if the header is absent, malformed, or the urgency is out of range.
///
/// ```
/// # use conduit::proxy::router::parse_rfc9218_priority;
/// assert_eq!(parse_rfc9218_priority("u=0"), Some(100)); // highest urgency
/// assert_eq!(parse_rfc9218_priority("u=3"), Some(58));  // default urgency
/// assert_eq!(parse_rfc9218_priority("u=7"), Some(2));   // lowest urgency
/// assert_eq!(parse_rfc9218_priority("u=7,i"), Some(2)); // incremental flag ignored
/// ```
pub fn parse_rfc9218_priority(header: &str) -> Option<u8> {
    // Find the `u=<N>` token; other directives (e.g. `i`) are ignored.
    for token in header.split(',') {
        let token = token.trim();
        if let Some(rest) = token.strip_prefix("u=") {
            // Strip any structured-field parameters (e.g. `u=3;foo=bar` → "3").
            let val_str = rest.split(';').next().unwrap_or("").trim();
            if let Ok(urgency) = val_str.parse::<u8>() {
                if urgency <= 7 {
                    return Some(100u8.saturating_sub(urgency * 14));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{HealthCheckConfig, HealthCheckOptions, MetricsConfig};

    // ── is_health_path ────────────────────────────────────────────────────────

    #[test]
    fn health_path_default_matches() {
        assert!(is_health_path(None, "/__health__"));
    }

    #[test]
    fn health_path_non_health_does_not_match() {
        assert!(!is_health_path(None, "/other"));
    }

    #[test]
    fn health_path_disabled_via_false() {
        let site = SiteConfig {
            health_check: Some(HealthCheckConfig::Enabled(false)),
            ..Default::default()
        };
        assert!(!is_health_path(Some(&site), "/__health__"));
    }

    #[test]
    fn health_path_enabled_via_true() {
        let site = SiteConfig {
            health_check: Some(HealthCheckConfig::Enabled(true)),
            ..Default::default()
        };
        assert!(is_health_path(Some(&site), "/__health__"));
    }

    #[test]
    fn health_path_custom_path_configured() {
        let site = SiteConfig {
            health_check: Some(HealthCheckConfig::Options(HealthCheckOptions {
                path: Some("/health".to_string()),
                ..Default::default()
            })),
            ..Default::default()
        };
        assert!(is_health_path(Some(&site), "/health"));
        assert!(!is_health_path(Some(&site), "/__health__"));
    }

    // ── metrics_token ─────────────────────────────────────────────────────────

    #[test]
    fn metrics_token_no_site_returns_none() {
        assert!(metrics_token(None, "/__metrics__").is_none());
    }

    #[test]
    fn metrics_token_default_path_no_auth() {
        let site = SiteConfig {
            metrics: Some(MetricsConfig {
                path: None,
                token: None,
            }),
            ..Default::default()
        };
        assert_eq!(metrics_token(Some(&site), "/__metrics__"), Some(None));
    }

    #[test]
    fn metrics_token_custom_path_with_token() {
        let site = SiteConfig {
            metrics: Some(MetricsConfig {
                path: Some("/m".to_string()),
                token: Some("secret".to_string()),
            }),
            ..Default::default()
        };
        assert_eq!(
            metrics_token(Some(&site), "/m"),
            Some(Some("secret".to_string()))
        );
        assert!(metrics_token(Some(&site), "/__metrics__").is_none());
    }

    // ── find_site_idx ─────────────────────────────────────────────────────────

    #[test]
    fn site_idx_empty_config_returns_none() {
        assert!(find_site_idx(&AppConfig::default(), "example.com", 80).is_none());
    }

    #[test]
    fn site_idx_exact_host_match() {
        let config = AppConfig {
            sites: vec![
                SiteConfig {
                    host: Some("other.com".to_string()),
                    ..Default::default()
                },
                SiteConfig {
                    host: Some("example.com".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(find_site_idx(&config, "example.com", 80), Some(1));
    }

    #[test]
    fn site_idx_wildcard_fallback() {
        let config = AppConfig {
            sites: vec![
                SiteConfig {
                    host: Some("example.com".to_string()),
                    ..Default::default()
                },
                SiteConfig {
                    host: Some("*".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(find_site_idx(&config, "other.com", 80), Some(1));
    }

    // ── parse_rfc9218_priority ────────────────────────────────────────────────

    #[test]
    fn rfc9218_highest_urgency_maps_to_100() {
        assert_eq!(parse_rfc9218_priority("u=0"), Some(100));
    }

    #[test]
    fn rfc9218_default_urgency_maps_to_58() {
        assert_eq!(parse_rfc9218_priority("u=3"), Some(58));
    }

    #[test]
    fn rfc9218_lowest_urgency_maps_to_2() {
        assert_eq!(parse_rfc9218_priority("u=7"), Some(2));
    }

    #[test]
    fn rfc9218_incremental_flag_ignored() {
        assert_eq!(parse_rfc9218_priority("u=7,i"), Some(2));
        assert_eq!(parse_rfc9218_priority("u=1, i"), Some(86));
    }

    #[test]
    fn rfc9218_out_of_range_returns_none() {
        assert_eq!(parse_rfc9218_priority("u=8"), None);
        assert_eq!(parse_rfc9218_priority("u=255"), None);
    }

    #[test]
    fn rfc9218_malformed_returns_none() {
        assert_eq!(parse_rfc9218_priority(""), None);
        assert_eq!(parse_rfc9218_priority("i"), None);
        assert_eq!(parse_rfc9218_priority("u=abc"), None);
    }

    // ── acme_challenge_token ──────────────────────────────────────────────────

    #[test]
    #[cfg(feature = "acme")]
    fn acme_challenge_token_extracts_token() {
        assert_eq!(
            acme_challenge_token("/.well-known/acme-challenge/abc123"),
            Some("abc123")
        );
    }

    #[test]
    fn acme_challenge_token_none_for_other_paths() {
        assert!(acme_challenge_token("/").is_none());
        assert!(acme_challenge_token("/__health__").is_none());
        assert!(acme_challenge_token("/.well-known/other").is_none());
    }

    #[test]
    #[cfg(feature = "acme")]
    fn acme_challenge_token_empty_token() {
        // Edge case: empty token after the challenge prefix.
        let result = acme_challenge_token("/.well-known/acme-challenge/");
        assert_eq!(result, Some(""));
    }

    #[test]
    #[cfg(not(feature = "acme"))]
    fn acme_challenge_token_always_none_without_feature() {
        // Issue #341: without `acme`, this path must never win routing
        // precedence — no matter how well-formed the challenge path is.
        assert!(acme_challenge_token("/.well-known/acme-challenge/abc123").is_none());
        assert!(acme_challenge_token("/.well-known/acme-challenge/").is_none());
    }

    // ── is_hot_reload_sse_path and is_hot_reload_js_path ─────────────────────

    #[test]
    #[cfg(feature = "hotreload")]
    fn hot_reload_sse_path_when_enabled() {
        let site = SiteConfig {
            hot_reload: Some(crate::config::schema::HotReloadConfig::Enabled(true)),
            ..Default::default()
        };
        assert!(is_hot_reload_sse_path(Some(&site), "/__hot-reload__"));
        assert!(!is_hot_reload_sse_path(
            Some(&site),
            "/__hot-reload__/client.js"
        ));
        assert!(!is_hot_reload_sse_path(Some(&site), "/other"));
    }

    #[test]
    #[cfg(feature = "hotreload")]
    fn hot_reload_sse_path_when_disabled() {
        let site = SiteConfig {
            hot_reload: Some(crate::config::schema::HotReloadConfig::Enabled(false)),
            ..Default::default()
        };
        assert!(!is_hot_reload_sse_path(Some(&site), "/__hot-reload__"));
    }

    #[test]
    #[cfg(feature = "hotreload")]
    fn hot_reload_sse_path_no_site_returns_false() {
        assert!(!is_hot_reload_sse_path(None, "/__hot-reload__"));
    }

    #[test]
    #[cfg(feature = "hotreload")]
    fn hot_reload_js_path_when_enabled() {
        let site = SiteConfig {
            hot_reload: Some(crate::config::schema::HotReloadConfig::Enabled(true)),
            ..Default::default()
        };
        assert!(is_hot_reload_js_path(
            Some(&site),
            "/__hot-reload__/client.js"
        ));
        assert!(!is_hot_reload_js_path(Some(&site), "/__hot-reload__"));
    }

    #[test]
    #[cfg(feature = "hotreload")]
    fn hot_reload_js_path_when_disabled() {
        let site = SiteConfig {
            hot_reload: Some(crate::config::schema::HotReloadConfig::Enabled(false)),
            ..Default::default()
        };
        assert!(!is_hot_reload_js_path(
            Some(&site),
            "/__hot-reload__/client.js"
        ));
    }

    #[test]
    #[cfg(feature = "hotreload")]
    fn hot_reload_js_path_with_query_string() {
        let site = SiteConfig {
            hot_reload: Some(crate::config::schema::HotReloadConfig::Enabled(true)),
            ..Default::default()
        };
        // Query string should be stripped before comparison.
        assert!(is_hot_reload_js_path(
            Some(&site),
            "/__hot-reload__/client.js?v=123"
        ));
    }

    #[test]
    #[cfg(not(feature = "hotreload"))]
    fn hot_reload_paths_always_false_without_feature() {
        // Issue #341's fix class, applied here: without `hotreload`, these
        // paths must never win routing precedence — no matter how the site
        // configures `hotReload`.
        let site = SiteConfig {
            hot_reload: Some(crate::config::schema::HotReloadConfig::Enabled(true)),
            ..Default::default()
        };
        assert!(!is_hot_reload_sse_path(Some(&site), "/__hot-reload__"));
        assert!(!is_hot_reload_js_path(
            Some(&site),
            "/__hot-reload__/client.js"
        ));
    }
}
