//! Checks that span more than one site: port conflicts, duplicate `host:port`, the HTTP-redirect
//! port collision and the multiple-Redis-URLs warning.

use std::collections::HashMap;

use super::ValidationError;

#[cfg(feature = "redis")]
use super::warnings::sanitize_for_log;
#[cfg(feature = "redis")]
use conduit_config_core::redact::redact_url;
#[cfg(feature = "redis")]
use conduit_config_core::scheme::is_redis_url;

use crate::config::defaults::DEFAULT_ADMIN_BIND;
use crate::config::schema::{AppConfig, SiteConfig};

/// Warn (advisory, not fatal) when more than one distinct `redis://`/`rediss://`
/// URL is configured across every site/route/consumer `rateLimit.store` in the
/// whole config (issue #357).
///
/// Only one Redis connection is ever established per process
/// (`connect_redis_rate_limiter_if_configured` in `src/server/builder.rs`
/// connects to the first URL found scanning site → route → consumer, across
/// sites in order) — every level that names a *different* URL silently shares
/// that one connection instead of getting its own. This can't be a hard error
/// (a config with mismatched-but-harmless URLs, e.g. a typo'd port an operator
/// hasn't noticed yet, must still start), so it's `Severity::Warning` like the
/// near-expiry-cert check (#191/#253) — logged, not fatal.
#[cfg(feature = "redis")]
pub(super) fn check_redis_store_consistency(config: &AppConfig, errors: &mut Vec<ValidationError>) {
    let mut seen: Vec<String> = Vec::new();
    for site in &config.sites {
        collect_redis_stores(site, &mut seen);
    }

    let mut distinct: Vec<&str> = Vec::new();
    for url in &seen {
        if !distinct.contains(&url.as_str()) {
            distinct.push(url);
        }
    }

    if distinct.len() > 1 {
        // `validate_rate_limit` only checks the `redis://`/`rediss://` prefix
        // on `store` — it doesn't reject embedded control characters, so an
        // operator-supplied URL could carry a raw newline this far.
        // `sanitize_for_log` (in `warnings.rs`, also used for other config-derived
        // values reaching a warning/error message) escapes those before they
        // can forge a fake log line in this warning's output.
        let redacted: Vec<String> = distinct
            .iter()
            .map(|url| sanitize_for_log(&redact_url(url)))
            .collect();
        errors.push(ValidationError::warning(
            "rateLimit.store",
            format!(
                "multiple distinct Redis URLs configured across site/route/consumer \
                 rateLimit.store fields ({}). Only one Redis connection is ever \
                 established per process — Conduit will connect to '{}' and every \
                 other level configuring a different URL will silently share that \
                 same connection instead of getting its own. Point every level at \
                 the same Redis instance to avoid surprises.",
                redacted.join(", "),
                redacted[0]
            ),
        ));
    }
}

/// Collect every `redis://`/`rediss://` `rate_limit.store` value configured on
/// `site` — site-level, then per-route (`proxy` map AND `routes[]`, issue
/// #360), then per-consumer — in the same scan order as
/// `src/server/builder.rs::find_redis_rate_limit_store`, appending to `out`
/// (not deduped; the caller dedupes across all sites). Delegates to the
/// shared [`crate::config::rate_limit_scan::iter_rate_limit_configs`] walk.
#[cfg(feature = "redis")]
fn collect_redis_stores(site: &SiteConfig, out: &mut Vec<String>) {
    out.extend(
        crate::config::rate_limit_scan::iter_rate_limit_configs(site)
            .filter_map(|rl| rl.store.as_deref())
            .filter(|s| is_redis_url(s))
            .map(str::to_owned),
    );
}

pub(super) fn effective_port(site: &SiteConfig) -> u16 {
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

pub(super) fn validate_no_duplicate_host_port(
    config: &AppConfig,
    errors: &mut Vec<ValidationError>,
) {
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

/// The port the Admin API listens on: the port of `global.admin.bind` when it parses, else the
/// documented default ([`DEFAULT_ADMIN_BIND`]). A forwardAuth URL must not point at it (#447).
pub(super) fn admin_port(config: &AppConfig) -> u16 {
    fn port_of(bind: &str) -> Option<u16> {
        bind.rsplit(':').next()?.parse().ok()
    }
    let configured = config
        .global
        .as_ref()
        .and_then(|g| g.admin.as_ref())
        .and_then(|a| a.bind.as_deref())
        .and_then(port_of);
    // The default is a constant with a port in it (`defaults.rs` pins that).
    configured
        .or_else(|| port_of(DEFAULT_ADMIN_BIND))
        .unwrap_or(2019)
}

/// `global.workers: 0` used to be silently inert (issue #226 — the field was
/// parsed but never applied), so a typo'd `0` had no real effect. Now that it
/// actually reaches Pingora's `ServerConf.threads`, `0` would mean the server
/// spawns no worker threads at all — reject it at validate-time rather than
/// let it reach `Server::new_with_opt_and_conf` (found by CodeRabbit/Gitar
/// review on the #226 fix itself).
pub(super) fn validate_global(config: &AppConfig, errors: &mut Vec<ValidationError>) {
    if let Some(workers) = config.global.as_ref().and_then(|g| g.workers) {
        if workers == 0 {
            errors.push(ValidationError::new(
                "global.workers",
                "must be greater than 0 (0 would run the server with no worker threads)",
            ));
        }
    }
}

pub(super) fn validate_http_redirect_ports(config: &AppConfig, errors: &mut Vec<ValidationError>) {
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
