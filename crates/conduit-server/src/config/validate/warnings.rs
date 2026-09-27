//! `feature_warnings()`: advisory messages for configuration that a compile-time feature would
//! act on but that this build ignores, plus the secret/metrics/unknown-key warnings.

use conduit_config_core::scheme::is_redis_url;

use crate::config::schema::{AppConfig, SiteConfig};

// The sixteen `conduit_x::warnings::COMPILED == cfg!(feature = "x")` compile-time asserts that used to sit here
// (checking each feature-owning crate's `COMPILED` const against this crate's own `cfg!()`) now live in the ROOT
// crate's `src/config/validate/mod.rs` instead, unchanged — see this crate's own top-level doc comment
// (`crates/conduit-server/src/lib.rs`) for why: the functions below never read their own crate's `cfg!()`, they
// only call into each leaf crate's `warnings::feature_warning()`/`COMPILED`, so moving this file doesn't change
// what the asserts need to check — only where `cfg!()` needs to point, which is the root's own Cargo features,
// not this crate's (this crate has no reason to depend on `otlp`/`wasm`/`rhai`/... otherwise).

/// Maps a top-level `SiteConfig` JSON/YAML key to the Cargo feature that
/// owns it. Used by `check_extra_key_warnings` below to turn a key that
/// lands in `SiteConfig.extra` (unrecognized by any named field) into an
/// actionable warning instead of a silent no-op.
///
/// This table is ahead of need (#124, part of the Conduit 2.0 workspace
/// migration, #114): today every field on `SiteConfig` is always present
/// regardless of compiled features, so a well-known key like `jwtAuth`
/// always lands in its named field, never in `extra` — this table only
/// fires for genuine typos right now. Once the migration starts
/// `#[cfg]`-gating these fields per feature crate, the same keys will start
/// landing in `extra` whenever their feature is compiled out, and this
/// table (already wired into `feature_warnings()`) picks them up with no
/// further changes needed — preserving today's warning UX instead of
/// regressing to a hard deserialize error or a silent drop.
const DISABLED_KEY_OWNING_FEATURE: &[(&str, &str)] = &[
    ("jwtAuth", "jwt"),
    ("forwardAuth", "forward-auth"),
    ("consumers", "consumers"),
    ("tcp", "tcp"),
    ("upload", "upload"),
    ("faultInjection", "fault-injection"),
    ("compression", "compression"),
    ("static", "static"),
    ("fallback", "static"),
    ("hotReload", "hotreload"),
];

/// Warn about unrecognized top-level site config keys (`SiteConfig.extra`):
/// a specific "recompile with --features X" message when the key is known
/// to belong to an optional feature, or a generic typo warning otherwise.
///
/// The key name itself is config-controlled (an arbitrary JSON/YAML object
/// key) and is interpolated into the warning message below — routed through
/// `sanitize_for_log()` for the same reason `check_proxy_loop_warnings`'s
/// `target` is (issue #185, CWE-117-style log injection).
pub(super) fn check_extra_key_warnings(config: &AppConfig, warnings: &mut Vec<String>) {
    for (i, site) in config.sites.iter().enumerate() {
        for key in site.extra.keys() {
            let key = sanitize_for_log(key);
            match DISABLED_KEY_OWNING_FEATURE.iter().find(|(k, _)| **k == key) {
                Some((_, feature)) => warnings.push(format!(
                    "sites[{i}].{key} is configured but Conduit was compiled without the \
                     `{feature}` feature — this configuration will be ignored. \
                     Recompile with `--features {feature}` to enable."
                )),
                None => warnings.push(format!(
                    "sites[{i}].{key} is not a recognized configuration key and will be \
                     ignored — check for a typo."
                )),
            }
        }
    }
}

/// Escapes control characters (newlines, carriage returns, and other
/// non-printable bytes) in a config-controlled string before it's
/// interpolated into a `feature_warnings()` message.
///
/// Issue #185 (CWE-117-style log injection): these warnings are written via
/// `tracing::warn!("{w}")` at startup/hot-reload (`main.rs`) and returned
/// verbatim in the Admin API `/reload` response's `warnings: [...]` field
/// (`admin/api.rs`). A config value containing a literal newline — e.g. a
/// proxy target URL, which is operator-supplied and not otherwise validated
/// for control characters before this point — could forge additional fake
/// log lines in the tracing output. Applied once centrally here rather than
/// patched ad hoc per call site, so any future `check_*_warnings` function
/// that interpolates a config-controlled string gets the same treatment by
/// routing through this helper instead of re-deriving the fix.
///
/// Escaping (not stripping) preserves the operator's ability to see what the
/// offending value actually contained, just rendered as a single log line.
///
/// `pub`, not `pub(super)`: the root crate's `src/config/validate/tests.rs` (which stays in the root — see this
/// crate's own top-level doc comment) needs it too, re-exported at `crate::config::validate::sanitize_for_log`
/// (`mod.rs`).
pub fn sanitize_for_log(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<char>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// Check warnings for global-level feature flags (e.g. OTLP).
pub(super) fn check_global_feature_warnings(config: &AppConfig, warnings: &mut Vec<String>) {
    warnings.extend(conduit_otlp::warnings::feature_warning(
        config.global.as_ref().and_then(|g| g.otlp.as_ref()),
    ));
}

/// Check per-site feature-gated option warnings.
pub(super) fn check_per_site_feature_warnings(config: &AppConfig, warnings: &mut Vec<String>) {
    for (i, site) in config.sites.iter().enumerate() {
        check_site_middleware_feature_warnings(i, site, warnings);
        check_site_simple_feature_warnings(i, site, warnings);
        check_site_proxy_feature_warnings(i, site, warnings);
    }
}

/// `proxy` / `routes[].proxy` in a build without the `proxy` feature (issue #144).
fn check_site_proxy_feature_warnings(i: usize, site: &SiteConfig, warnings: &mut Vec<String>) {
    conduit_proxy_http::warnings::feature_warnings(
        i,
        site.proxy.as_ref(),
        site.routes.as_deref(),
        warnings,
    );
}

/// Middleware-level feature warnings (wasm, rhai) for a single site.
fn check_site_middleware_feature_warnings(i: usize, site: &SiteConfig, warnings: &mut Vec<String>) {
    if let Some(middleware) = &site.middleware {
        conduit_middleware::warnings::feature_warnings(i, middleware, warnings);
    }
}

/// Simple (non-middleware) per-site feature warnings, in a fixed order that the golden tests pin.
///
/// `redis` and `cache` need the whole site to decide whether the feature would matter (`site_uses_redis_store`,
/// `site_has_cache_config`), so the root evaluates those two predicates and hands the crate a `bool`.
/// `consumers` appears twice on purpose: the plain feature-off warning is 9th and the consumers-without-`jwt` one 14th, and
/// the warnings between them keep their positions.
fn check_site_simple_feature_warnings(i: usize, site: &SiteConfig, warnings: &mut Vec<String>) {
    warnings.extend(conduit_auth_jwt::warnings::feature_warning(
        i,
        site.jwt_auth.as_ref(),
    ));
    warnings.extend(conduit_auth_forward::warnings::feature_warning(
        i,
        site.forward_auth.as_ref(),
    ));
    warnings.extend(conduit_acme::warnings::feature_warning(
        i,
        site.tls.as_ref().and_then(|t| t.acme.as_ref()),
    ));
    warnings.extend(conduit_tcp::warnings::feature_warning(i, site.tcp.as_ref()));
    warnings.extend(conduit_ratelimit::warnings::feature_warning(
        i,
        site_uses_redis_store(site),
    ));
    warnings.extend(conduit_cache::warnings::feature_warning(
        i,
        site_has_cache_config(site),
    ));
    warnings.extend(conduit_upload::warnings::feature_warning(
        i,
        site.upload.as_ref(),
    ));
    warnings.extend(conduit_faults::warnings::feature_warning(
        i,
        site.fault_injection.as_ref(),
    ));
    warnings.extend(conduit_auth_consumers::warnings::feature_warning(
        i,
        site.consumers.as_ref(),
    ));
    warnings.extend(conduit_compression::warnings::feature_warning(
        i,
        site.compression.as_ref(),
    ));
    warnings.extend(conduit_static::warnings::static_feature_warning(
        i,
        site.static_files.as_ref(),
    ));
    warnings.extend(conduit_static::warnings::fallback_feature_warning(
        i,
        site.fallback.as_ref(),
    ));
    warnings.extend(conduit_hotreload::warnings::feature_warning(
        i,
        site.hot_reload.as_ref(),
    ));
    warnings.extend(conduit_auth_consumers::warnings::jwt_feature_warning(
        i,
        site.consumers.as_ref(),
    ));
}

/// Return `true` when any proxy route in the site has a `cache` config block.
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

/// Return `true` when `site`'s site-, route- (`proxy` map or `routes[]`,
/// issue #360), or consumer-level `rateLimit` configures a `redis://`/
/// `rediss://` store — mirrors `src/server/builder.rs::
/// find_redis_rate_limit_store`'s scan (issue #322), but only needs a yes/no
/// answer here rather than the actual URL. Delegates to the shared
/// [`crate::config::rate_limit_scan::iter_rate_limit_configs`] walk.
fn site_uses_redis_store(site: &SiteConfig) -> bool {
    crate::config::rate_limit_scan::iter_rate_limit_configs(site)
        .filter_map(|rl| rl.store.as_deref())
        .any(is_redis_url)
}

/// Warn when JWT HMAC secrets are shorter than the 32-byte minimum.
pub(super) fn check_jwt_secret_warnings(config: &AppConfig, warnings: &mut Vec<String>) {
    for (i, site) in config.sites.iter().enumerate() {
        if let Some(jwt) = &site.jwt_auth {
            warnings.extend(conduit_auth_jwt::warnings::secret_warning(i, jwt));
        }
        check_consumer_jwt_secret_warnings(i, site, warnings);
    }
}

/// Warn when consumer-level JWT secrets are too short.
fn check_consumer_jwt_secret_warnings(i: usize, site: &SiteConfig, warnings: &mut Vec<String>) {
    if let Some(consumers_cfg) = &site.consumers {
        conduit_auth_consumers::warnings::secret_warnings(i, consumers_cfg, warnings);
    }
}

/// Warn when a metrics endpoint has no auth token configured.
pub(super) fn check_metrics_auth_warnings(config: &AppConfig, warnings: &mut Vec<String>) {
    for (i, site) in config.sites.iter().enumerate() {
        if let Some(metrics) = &site.metrics {
            warnings.extend(conduit_metrics::warnings::token_warning(i, metrics));
        }
    }
}
