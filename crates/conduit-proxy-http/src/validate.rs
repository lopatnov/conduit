//! Config validation for `proxy`/`routes[]`: proxy targets, route configs, caches, upstream groups and rewrites. Called by the root
//! crate's `config::validate`.

use conduit_cache::validate::validate_cache_config;
use conduit_config_core::validation::ValidationError;
use conduit_ratelimit::validate::validate_rate_limit;
use conduit_upstream::{LoadBalanceStrategy, ProxyTarget, UpstreamGroup};

use crate::config::{ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, RewriteRule};

pub fn validate_proxy(proxy: &ProxyConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
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
                warn_route_key_is_not_a_path(route, &route_prefix);
                validate_proxy_route_target(target, &route_prefix, errors);
            }
        }
    }
}

/// Issue #706: the keys of a `proxy` map are path prefixes, and a request path always starts with `/`. A key that
/// does not can never match, so its upstreams are never used. The usual cause is `proxy: { targets: [...] }`, which
/// reads like "these are the targets" but is one route named `targets`: every request then gets the site's fallback
/// (a 404), and nothing said so. A key that is empty once its trailing `/` is trimmed (`""`, `/`, `//`) is the
/// catch-all: `find_route` (resolve.rs) matches every path with it, so it is not warned about.
fn warn_route_key_is_not_a_path(route: &str, prefix: &str) {
    if route.starts_with('/') || route.trim_end_matches('/').is_empty() {
        return;
    }
    tracing::warn!(
        "{prefix}: the key '{route}' is not a path (it does not start with '/'), so it never matches a request and its upstreams are never used; to send every request to one upstream write `proxy: \"http://host:port\"` or `proxy: {{ \"/\": [...] }}`"
    );
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

pub fn validate_route_config(
    cfg: &ProxyRouteConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
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
    // A probe timeout of 0 would fail every probe (it is read as 1 ms), taking the upstream out for good (#652).
    if cfg
        .health_check
        .as_ref()
        .and_then(|h| h.timeout_ms)
        .is_some_and(|ms| ms == 0)
    {
        errors.push(ValidationError::new(
            format!("{prefix}.healthCheck.timeoutMs"),
            "timeoutMs must be at least 1 (milliseconds)",
        ));
    }
    warn_slow_start_ignored(cfg, prefix);
    warn_group_strategy_cannot_see_load(cfg, prefix);
    if let Some(cache) = &cfg.cache {
        validate_cache_config(cache, &format!("{prefix}.cache"), errors);
    }
    // Per-route rateLimit was never validated at all before issue #310 (found
    // independently by security-engineer and CodeRabbit reviewing #309) — a
    // malformed keyBy/zero windowSecs/unknown algorithm/invalid store all
    // passed silently. Now shares the same validation as site/consumer level.
    if let Some(rate_limit) = &cfg.rate_limit {
        validate_rate_limit(rate_limit, prefix, errors);
        // `RateLimitConfig` is shared with the site and consumer levels, which honour `dryRun`; the per-route check
        // never reads it, so accepting it here would silently enforce the limit (#521). The JSON Schema already omits it.
        if rate_limit.dry_run.is_some() {
            errors.push(ValidationError::new(
                format!("{prefix}.rateLimit.dryRun"),
                "dryRun is not supported on a route-level rateLimit (only site and consumer rate limits support it)",
            ));
        }
    }
}

/// Warn (never an error) when `healthCheck.slowStartSecs` is set on a route where it has no effect.
///
/// Slow start (#157) is deliberately not applied to hash-based strategies
/// or sticky sessions -- a client's hash/pin must map to a fixed upstream
/// for the strategy's own consistency guarantee to hold, which a
/// probabilistic ramp gate would break. Warn rather than silently ignore,
/// so an operator isn't left believing a recovered upstream on this route
/// is being ramped when it isn't. Per-route, not sitewide: other routes on
/// the same site ramp normally.
///
/// Split out of `validate_route_config` (Sonar `rust:S3776`); it only logs, so the order of the
/// validation errors is unchanged.
fn warn_slow_start_ignored(cfg: &ProxyRouteConfig, prefix: &str) {
    let Some(window) = cfg.health_check.as_ref().and_then(|h| h.slow_start_secs) else {
        return;
    };
    if window == 0 {
        return;
    }
    // A route with `groups` never reads its own `strategy` or `sticky` (`resolve_grouped` picks the
    // group by `groupStrategy` and the target by each group's strategy; groups have no sticky
    // sessions), so judging it by them would warn about a leftover field that changes nothing
    // (issue #483). The groups are judged below.
    let has_groups = cfg.groups.as_ref().is_some_and(|g| !g.is_empty());
    let hash_based = matches!(
        cfg.strategy,
        Some(LoadBalanceStrategy::IpHash | LoadBalanceStrategy::ConsistentHash)
    );
    if !has_groups && (hash_based || cfg.sticky.is_some()) {
        tracing::warn!(
            "{prefix}.healthCheck.slowStartSecs is ignored on this route: hash-based \
             strategies and sticky sessions map each client to a fixed upstream, so a \
             recovered upstream receives full traffic immediately (see docs/configuration.md, \
             'Slow start')"
        );
    }
    // `groups` selects an *inner* strategy per group (resolve_grouped in
    // router.rs) independent of the route-level `strategy` above -- a
    // hash-based group strategy hits pick_bounded's same early-return and
    // silently bypasses the ramp, with no warning otherwise (found by
    // Gitar reviewing #157/PR #365).
    for group in cfg.groups.iter().flatten() {
        if matches!(
            group.strategy,
            Some(LoadBalanceStrategy::IpHash | LoadBalanceStrategy::ConsistentHash)
        ) {
            tracing::warn!(
                "{prefix}.healthCheck.slowStartSecs is ignored for group '{}': its \
                 hash-based strategy maps each client to a fixed upstream, so a \
                 recovered upstream receives full traffic immediately (see \
                 docs/configuration.md, 'Slow start')",
                group.name
            );
        }
    }
}

/// The strategies that choose by the load, latency or traffic of the thing they pick, which for the outer
/// pick of a route with `groups` is a group *name*: nothing is ever in flight on a name, no latency is
/// ever measured for it and no bytes move through it.
fn group_strategy_cannot_see_load(strategy: Option<&LoadBalanceStrategy>) -> bool {
    matches!(
        strategy,
        Some(
            LoadBalanceStrategy::LeastConn
                | LoadBalanceStrategy::P2c
                | LoadBalanceStrategy::LeastBandwidth
                | LoadBalanceStrategy::LeastResponseTime
        )
    )
}

/// Warn that a load-aware `groupStrategy` does not balance by load (issue #632). It only logs, so a
/// config that worked keeps working: the strategy still picks a group, just not the least loaded one.
fn warn_group_strategy_cannot_see_load(cfg: &ProxyRouteConfig, prefix: &str) {
    let has_groups = cfg.groups.as_ref().is_some_and(|g| !g.is_empty());
    if has_groups && group_strategy_cannot_see_load(cfg.group_strategy.as_ref()) {
        tracing::warn!(
            "{prefix}.groupStrategy is load-aware but a group has no load of its own, so it does \
             not choose the least loaded group (least-conn, p2c and least-bandwidth choose at \
             random, least-response-time in turn). Use round-robin, random, ip-hash or \
             consistent-hash for the outer pick and put the load-aware strategy on each group's \
             own `strategy` (see docs/configuration.md, 'Upstream groups')"
        );
    }
}

/// Validate upstream groups: non-empty targets and WRR strategy requirements.
fn validate_groups_config(
    groups: &[UpstreamGroup],
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
    use std::io;
    use std::sync::{Arc, Mutex};

    use conduit_upstream::config::UpstreamHealthCheck;
    use tracing_subscriber::fmt::MakeWriter;

    use super::*;
    use crate::config::StickyConfig;

    /// A `tracing` writer that keeps everything it is given.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Captured {
        type Writer = Captured;
        fn make_writer(&'a self) -> Captured {
            self.clone()
        }
    }

    /// Everything `validate_route_config` logs at warn level for `cfg`, one line per warning.
    fn warnings_for(cfg: &ProxyRouteConfig) -> Vec<String> {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut errors = Vec::new();
            validate_route_config(cfg, "sites[0].proxy", &mut errors);
        });
        let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        text.lines().map(str::to_owned).collect()
    }

    /// Everything `validate_proxy` logs at warn level for `proxy`.
    fn proxy_warnings_for(proxy: &ProxyConfig) -> Vec<String> {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut errors = Vec::new();
            validate_proxy(proxy, "sites[0].proxy", &mut errors);
        });
        let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        text.lines().map(str::to_owned).collect()
    }

    fn routes_map(key: &str) -> ProxyConfig {
        let mut routes = indexmap::IndexMap::new();
        routes.insert(
            key.to_owned(),
            ProxyRouteTarget::RoundRobin(vec!["http://api:4000".to_owned()]),
        );
        ProxyConfig::Routes(routes)
    }

    /// Issue #706: `proxy: { targets: [...] }` is a route named `targets`, which no request path can match.
    #[test]
    fn a_proxy_route_key_that_is_not_a_path_is_warned_about() {
        let lines = proxy_warnings_for(&routes_map("targets"));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("'targets'") && l.contains("never matches")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_proxy_route_key_that_is_a_path_gives_no_warning() {
        // "" and "//" are the catch-all too (`find_route` trims the trailing slashes: an empty key matches everything)
        for key in ["/", "", "//", "/api", "/api/**"] {
            let lines = proxy_warnings_for(&routes_map(key));
            assert!(lines.is_empty(), "{key}: {lines:?}");
        }
        // the string shorthand has no key at all
        assert!(proxy_warnings_for(&ProxyConfig::Single("http://api:4000".to_owned())).is_empty());
    }

    fn route(
        strategy: Option<LoadBalanceStrategy>,
        slow_start_secs: Option<u64>,
    ) -> ProxyRouteConfig {
        ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://a:1".to_owned())],
            strategy,
            health_check: slow_start_secs.map(|s| UpstreamHealthCheck {
                slow_start_secs: Some(s),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Issue #652: `healthCheck.timeoutMs` of 0 is an error naming the field; a positive value and no value pass.
    #[test]
    fn a_zero_probe_timeout_is_refused() {
        let with = |timeout_ms: Option<u64>| ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://a:1".to_owned())],
            health_check: Some(UpstreamHealthCheck {
                timeout_ms,
                ..Default::default()
            }),
            ..Default::default()
        };
        let errors_of = |cfg: &ProxyRouteConfig| {
            let mut errors = Vec::new();
            validate_route_config(cfg, "sites[0].proxy", &mut errors);
            errors
        };
        let errors = errors_of(&with(Some(0)));
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].path, "sites[0].proxy.healthCheck.timeoutMs");
        assert!(errors_of(&with(Some(1))).is_empty());
        assert!(errors_of(&with(Some(2500))).is_empty());
        assert!(errors_of(&with(None)).is_empty());
    }

    fn grouped_route(group_strategy: Option<LoadBalanceStrategy>) -> ProxyRouteConfig {
        let group = |name: &str, url: &str| conduit_upstream::config::UpstreamGroup {
            name: name.to_owned(),
            targets: vec![ProxyTarget::Simple(url.to_owned())],
            strategy: Some(LoadBalanceStrategy::LeastConn),
        };
        ProxyRouteConfig {
            groups: Some(vec![group("g1", "http://a:1"), group("g2", "http://b:1")]),
            group_strategy,
            ..Default::default()
        }
    }

    /// Issue #632: the outer pick of a route with `groups` runs over group names, which have no load.
    #[test]
    fn a_load_aware_group_strategy_is_warned_about() {
        for strategy in [
            LoadBalanceStrategy::LeastConn,
            LoadBalanceStrategy::P2c,
            LoadBalanceStrategy::LeastBandwidth,
            LoadBalanceStrategy::LeastResponseTime,
        ] {
            let lines = warnings_for(&grouped_route(Some(strategy.clone())));
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains("sites[0].proxy.groupStrategy is load-aware")),
                "{strategy:?}: {lines:?}"
            );
        }
    }

    /// Strategies that need no load of what they pick, and a flat route with a leftover `groupStrategy`, give no warning.
    #[test]
    fn a_group_strategy_that_needs_no_load_is_not_warned_about() {
        for strategy in [
            None,
            Some(LoadBalanceStrategy::RoundRobin),
            Some(LoadBalanceStrategy::Random),
            Some(LoadBalanceStrategy::IpHash),
            Some(LoadBalanceStrategy::ConsistentHash),
        ] {
            let lines = warnings_for(&grouped_route(strategy.clone()));
            assert!(
                !lines
                    .iter()
                    .any(|l| l.contains("groupStrategy is load-aware")),
                "{strategy:?}: {lines:?}"
            );
        }
        // A flat route has no outer pick, whatever its `groupStrategy` field says.
        let flat = ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://a:1".to_owned())],
            group_strategy: Some(LoadBalanceStrategy::P2c),
            ..Default::default()
        };
        assert!(!warnings_for(&flat)
            .iter()
            .any(|l| l.contains("groupStrategy is load-aware")));
    }

    fn route_dry_run_errors(dry_run: Option<bool>) -> Vec<String> {
        let cfg = ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://a:1".to_owned())],
            rate_limit: Some(conduit_ratelimit::RateLimitConfig {
                window_secs: 60,
                limit: 10,
                burst: None,
                algorithm: None,
                key_by: None,
                skip_paths: None,
                store: None,
                dry_run,
            }),
            ..Default::default()
        };
        let mut errors = Vec::new();
        validate_route_config(&cfg, "sites[0].proxy", &mut errors);
        errors.into_iter().map(|e| e.path).collect()
    }

    #[test]
    fn route_level_dry_run_is_rejected_not_silently_ignored() {
        for dry_run in [Some(true), Some(false)] {
            let errors = route_dry_run_errors(dry_run);
            assert_eq!(errors.len(), 1, "{errors:?}");
            assert_eq!(errors[0], "sites[0].proxy.rateLimit.dryRun");
        }
        assert!(route_dry_run_errors(None).is_empty());
    }

    fn group(name: &str, strategy: Option<LoadBalanceStrategy>) -> UpstreamGroup {
        UpstreamGroup {
            name: name.to_owned(),
            targets: vec![ProxyTarget::Simple("http://a:1".to_owned())],
            strategy,
        }
    }

    #[test]
    fn slow_start_on_a_hash_based_route_warns_once() {
        for strategy in [
            LoadBalanceStrategy::IpHash,
            LoadBalanceStrategy::ConsistentHash,
        ] {
            let warnings = warnings_for(&route(Some(strategy), Some(30)));
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert!(
                warnings[0]
                    .contains("sites[0].proxy.healthCheck.slowStartSecs is ignored on this route"),
                "{warnings:?}"
            );
        }
    }

    #[test]
    fn slow_start_on_a_sticky_route_warns_once() {
        let mut cfg = route(Some(LoadBalanceStrategy::RoundRobin), Some(30));
        cfg.sticky = Some(StickyConfig {
            cookie: "sid".to_owned(),
            secret: None,
            strict: None,
        });
        let warnings = warnings_for(&cfg);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("is ignored on this route"),
            "{warnings:?}"
        );
    }

    #[test]
    fn slow_start_on_a_hash_based_group_warns_for_that_group_only() {
        let mut cfg = route(Some(LoadBalanceStrategy::RoundRobin), Some(30));
        cfg.groups = Some(vec![
            group("plain", Some(LoadBalanceStrategy::RoundRobin)),
            group("pinned", Some(LoadBalanceStrategy::IpHash)),
            group("default", None),
        ]);
        let warnings = warnings_for(&cfg);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("is ignored for group 'pinned'"),
            "{warnings:?}"
        );
    }

    /// Issue #483: with `groups`, the route-level `strategy` / `sticky` are not used, so only the
    /// groups are judged — a leftover `strategy: ip-hash` must not produce a false route warning.
    #[test]
    fn a_route_with_groups_is_judged_by_its_groups_not_its_own_strategy() {
        let mut cfg = route(Some(LoadBalanceStrategy::IpHash), Some(5));
        cfg.sticky = Some(StickyConfig {
            cookie: "sid".to_owned(),
            secret: None,
            strict: None,
        });
        cfg.groups = Some(vec![group("plain", Some(LoadBalanceStrategy::RoundRobin))]);
        assert!(warnings_for(&cfg).is_empty());

        cfg.groups = Some(vec![group(
            "pinned",
            Some(LoadBalanceStrategy::ConsistentHash),
        )]);
        cfg.group_strategy = Some(LoadBalanceStrategy::IpHash);
        let warnings = warnings_for(&cfg);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("is ignored for group 'pinned'"),
            "{warnings:?}"
        );
    }

    #[test]
    fn no_slow_start_warning_when_it_is_off_or_applies() {
        // window 0, and no health check at all: nothing is ignored.
        assert!(warnings_for(&route(Some(LoadBalanceStrategy::IpHash), Some(0))).is_empty());
        assert!(warnings_for(&route(Some(LoadBalanceStrategy::IpHash), None)).is_empty());
        // a strategy the ramp does apply to.
        assert!(warnings_for(&route(Some(LoadBalanceStrategy::RoundRobin), Some(30))).is_empty());
        assert!(warnings_for(&route(None, Some(30))).is_empty());
        // a hash-based group under window 0 stays quiet too.
        let mut cfg = route(None, Some(0));
        cfg.groups = Some(vec![group("pinned", Some(LoadBalanceStrategy::IpHash))]);
        assert!(warnings_for(&cfg).is_empty());
    }
}
