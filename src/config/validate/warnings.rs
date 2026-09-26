//! `feature_warnings()`: advisory messages for configuration that a compile-time feature would
//! act on but that this build ignores, plus the secret/metrics/unknown-key warnings.

use crate::config::schema::{AppConfig, SiteConfig};

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
pub(super) fn check_per_site_feature_warnings(config: &AppConfig, warnings: &mut Vec<String>) {
    for (i, site) in config.sites.iter().enumerate() {
        check_site_middleware_feature_warnings(i, site, warnings);
        check_site_simple_feature_warnings(i, site, warnings);
        check_site_proxy_feature_warnings(i, site, warnings);
    }
}

/// Warn about `proxy` configuration a build without the `proxy` feature cannot
/// honour (issue #144): the router ignores `sites[].proxy` entirely and ends a
/// `routes[]` entry with a `proxy` action in the site's fallback response, so
/// without this the operator would just see 404s and no explanation.
///
/// Two cfg'd variants rather than a `#[cfg]` inside one body, so the
/// proxy-enabled build carries no unused-variable noise (same shape as
/// `resolve_site_proxy` in `router.rs`).
#[cfg(not(feature = "proxy"))]
fn check_site_proxy_feature_warnings(i: usize, site: &SiteConfig, warnings: &mut Vec<String>) {
    if site.proxy.is_some() {
        warnings.push(format!(
            "sites[{i}].proxy is configured but Conduit was compiled without the `proxy` \
             feature — reverse proxying is disabled and this configuration will be ignored \
             (requests fall through to `static`/`fallback`). \
             Recompile with `--features proxy` to enable."
        ));
    }
    for (j, route) in site.routes.iter().flatten().enumerate() {
        if route.proxy.is_some() {
            warnings.push(format!(
                "sites[{i}].routes[{j}].proxy is configured but Conduit was compiled without \
                 the `proxy` feature — requests matching this route are answered by the \
                 site's `fallback` response (its `static` action, if any, is not served \
                 either). Recompile with `--features proxy` to enable."
            ));
        }
    }
}

#[cfg(feature = "proxy")]
fn check_site_proxy_feature_warnings(_i: usize, _site: &SiteConfig, _warnings: &mut Vec<String>) {}

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
    //
    // Checks site, route, and consumer levels alike (issue #322 gave the
    // latter two real effect when `redis` *is* compiled — before that, a
    // route/consumer `store: "redis://..."` was always a no-op regardless of
    // this feature, so warning about it there would have been misleading).
    #[cfg(not(feature = "redis"))]
    {
        if site_uses_redis_store(site) {
            warnings.push(format!(
                "sites[{i}].rateLimit.store (site, route, or consumer level) uses Redis but \
                 Conduit was compiled without the `redis` feature — falling back to in-memory \
                 rate limiting everywhere. Recompile with `--features redis` to enable."
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

    // ── Consumers (feature: consumers) ────────────────────────────────────────
    #[cfg(not(feature = "consumers"))]
    if site.consumers.is_some() {
        warnings.push(format!(
            "sites[{i}].consumers is configured but Conduit was compiled without the \
             `consumers` feature — consumer authentication will be disabled and every \
             request will bypass it. \
             Recompile with `--features consumers` to enable."
        ));
    }

    // ── Compression (feature: compression) ───────────────────────────────────
    // Unlike every other feature checked here, `compression` is default-on
    // at the root crate (issue #114/#138) — this warning only fires for a
    // deliberate `--no-default-features` build (or one that otherwise
    // excludes `compression`), same mechanism as the rest of this function.
    #[cfg(not(feature = "compression"))]
    if site.compression.is_some() {
        warnings.push(format!(
            "sites[{i}].compression is configured but Conduit was compiled without the \
             `compression` feature — response compression will be disabled. \
             Recompile with `--features compression` to enable."
        ));
    }

    // ── Static files (feature: static) ────────────────────────────────────────
    // Unlike every other feature checked here besides `compression`, `static`
    // is default-on at the root crate (issue #114/#139) — this warning only
    // fires for a deliberate `--no-default-features` build (or one that
    // otherwise excludes `static`), same mechanism as the rest of this
    // function.
    #[cfg(not(feature = "static"))]
    if site.static_files.is_some() {
        warnings.push(format!(
            "sites[{i}].static is configured but Conduit was compiled without the `static` \
             feature — static file serving will be disabled. \
             Recompile with `--features static` to enable."
        ));
    }

    // ── Fallback responses (feature: static) ──────────────────────────────────
    // Same default-on caveat as `static` above — `fallback` is served by the
    // same crate/feature (crates/conduit-static, issue #114/#139).
    #[cfg(not(feature = "static"))]
    if site.fallback.is_some() {
        warnings.push(format!(
            "sites[{i}].fallback is configured but Conduit was compiled without the `static` \
             feature — fallback responses (including the site's default 404) will be \
             disabled. \
             Recompile with `--features static` to enable."
        ));
    }

    // ── Hot reload (feature: hotreload) ───────────────────────────────────────
    // Unlike every other feature checked here besides `compression`/`static`,
    // `hotreload` is default-on at the root crate (issue #114/#140) — this
    // warning only fires for a deliberate `--no-default-features` build (or
    // one that otherwise excludes `hotreload`), same mechanism as the rest of
    // this function. Previously had no `feature_warnings()` case at all
    // (found during #140's extraction) — `hotReload` was never gated behind
    // any feature pre-extraction, so there was nothing to warn about yet.
    #[cfg(not(feature = "hotreload"))]
    if site.hot_reload.is_some() {
        warnings.push(format!(
            "sites[{i}].hotReload is configured but Conduit was compiled without the \
             `hotreload` feature — browser hot-reload (the SSE stream and file watcher) will \
             be disabled. \
             Recompile with `--features hotreload` to enable."
        ));
    }

    // ── Consumer JWT / sharedJwt without the `jwt` feature ────────────────────
    // `consumers` alone doesn't imply `jwt` — a consumer whose only credential
    // is `jwt` (V2) or a `consumers.sharedJwt` block (V3) is silently
    // unreachable without it (see `check_consumer_credentials`/
    // `identify_consumer` in `crates/conduit-auth-consumers/src/identify.rs`,
    // both `jwt`-gated).
    #[cfg(all(feature = "consumers", not(feature = "jwt")))]
    if let Some(ref consumers_cfg) = site.consumers {
        let has_shared_jwt = consumers_cfg.shared_jwt.is_some();
        let any_consumer_jwt = consumers_cfg.consumers.iter().any(|c| c.jwt.is_some());
        if has_shared_jwt || any_consumer_jwt {
            warnings.push(format!(
                "sites[{i}].consumers uses `sharedJwt` or a consumer `jwt` credential but \
                 Conduit was compiled without the `jwt` feature — those consumers will be \
                 permanently unreachable. \
                 Recompile with `--features jwt` to enable."
            ));
        }
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
        feature = "fault-injection",
        feature = "consumers",
        feature = "compression",
        feature = "static",
        feature = "hotreload"
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

/// Return `true` when `site`'s site-, route- (`proxy` map or `routes[]`,
/// issue #360), or consumer-level `rateLimit` configures a `redis://`/
/// `rediss://` store — mirrors `src/server/builder.rs::
/// find_redis_rate_limit_store`'s scan (issue #322), but only needs a yes/no
/// answer here rather than the actual URL. Delegates to the shared
/// [`crate::config::rate_limit_scan::iter_rate_limit_configs`] walk.
#[cfg(not(feature = "redis"))]
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
pub(super) fn check_metrics_auth_warnings(config: &AppConfig, warnings: &mut Vec<String>) {
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
