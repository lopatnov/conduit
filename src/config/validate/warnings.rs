//! `feature_warnings()`: advisory messages for configuration that a compile-time feature would
//! act on but that this build ignores, plus the secret/metrics/unknown-key warnings.

use crate::config::schema::{AppConfig, SiteConfig};

// The feature-off texts live in the crate that owns each feature (#316); each crate reports through its `COMPILED` whether *its*
// feature is on in this build. The root's feature of the same name must agree: if a crate's feature were on while the root's is
// off (or the reverse), a warning would silently vanish or appear where it should not. Turn any such drift into a build error.
const _: () = assert!(
    conduit_otlp::warnings::COMPILED == cfg!(feature = "otlp"),
    "`conduit-otlp`'s `otlp` feature and the root crate's `otlp` feature must be enabled together"
);
const _: () = assert!(
    conduit_middleware::warnings::WASM_COMPILED == cfg!(feature = "wasm"),
    "`conduit-middleware`'s `wasm` feature and the root crate's `wasm` feature must be enabled together"
);
const _: () = assert!(
    conduit_middleware::warnings::RHAI_COMPILED == cfg!(feature = "rhai"),
    "`conduit-middleware`'s `rhai` feature and the root crate's `rhai` feature must be enabled together"
);
const _: () = assert!(
    conduit_proxy_http::warnings::COMPILED == cfg!(feature = "proxy"),
    "`conduit-proxy-http`'s `proxy` feature and the root crate's `proxy` feature must be enabled together"
);
const _: () = assert!(
    conduit_auth_jwt::warnings::COMPILED == cfg!(feature = "jwt"),
    "`conduit-auth-jwt`'s `jwt` feature and the root crate's `jwt` feature must be enabled together"
);
const _: () = assert!(
    conduit_auth_forward::warnings::COMPILED == cfg!(feature = "forward-auth"),
    "`conduit-auth-forward`'s `forward-auth` feature and the root crate's `forward-auth` feature must be enabled together"
);
const _: () = assert!(
    conduit_acme::warnings::COMPILED == cfg!(feature = "acme"),
    "`conduit-acme`'s `acme` feature and the root crate's `acme` feature must be enabled together"
);
const _: () = assert!(
    conduit_tcp::warnings::COMPILED == cfg!(feature = "tcp"),
    "`conduit-tcp`'s `tcp` feature and the root crate's `tcp` feature must be enabled together"
);
const _: () = assert!(
    conduit_ratelimit::warnings::COMPILED == cfg!(feature = "redis"),
    "`conduit-ratelimit`'s `redis` feature and the root crate's `redis` feature must be enabled together"
);
const _: () = assert!(
    conduit_cache::warnings::COMPILED == cfg!(feature = "cache"),
    "`conduit-cache`'s `cache` feature and the root crate's `cache` feature must be enabled together"
);
const _: () = assert!(
    conduit_upload::warnings::COMPILED == cfg!(feature = "upload"),
    "`conduit-upload`'s `upload` feature and the root crate's `upload` feature must be enabled together"
);
const _: () = assert!(
    conduit_faults::warnings::COMPILED == cfg!(feature = "fault-injection"),
    "`conduit-faults`'s `fault-injection` feature and the root crate's `fault-injection` feature must be enabled together"
);
const _: () = assert!(
    conduit_auth_consumers::warnings::COMPILED == cfg!(feature = "consumers"),
    "`conduit-auth-consumers`'s `consumers` feature and the root crate's `consumers` feature must be enabled together"
);
const _: () = assert!(
    conduit_auth_consumers::warnings::JWT_COMPILED == cfg!(feature = "jwt"),
    "`conduit-auth-consumers`'s `jwt` feature and the root crate's `jwt` feature must be enabled together"
);
const _: () = assert!(
    conduit_compression::warnings::COMPILED == cfg!(feature = "compression"),
    "`conduit-compression`'s `compression` feature and the root crate's `compression` feature must be enabled together"
);
const _: () = assert!(
    conduit_static::warnings::COMPILED == cfg!(feature = "static"),
    "`conduit-static`'s `static` feature and the root crate's `static` feature must be enabled together"
);
const _: () = assert!(
    conduit_hotreload::warnings::COMPILED == cfg!(feature = "hotreload"),
    "`conduit-hotreload`'s `hotreload` feature and the root crate's `hotreload` feature must be enabled together"
);

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
pub(super) fn sanitize_for_log(s: &str) -> String {
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
    fn is_redis_store(store: &str) -> bool {
        store.starts_with("redis://") || store.starts_with("rediss://")
    }

    crate::config::rate_limit_scan::iter_rate_limit_configs(site)
        .filter_map(|rl| rl.store.as_deref())
        .any(is_redis_store)
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
