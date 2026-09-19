//! Upstream request transforms: `upstream_request_filter` (the
//! [`pingora_proxy::ProxyHttp::upstream_request_filter`] trait-method
//! body), forwarded-header injection, path strip/rewrite, header transform
//! (with JWT template substitution), traffic mirroring, and URI rebuilding.
//!
//! Split out of the former monolithic `request_phase.rs` (issue #144 prep,
//! PR 1 of 2) -- pure code relocation, no behavioral change.
//!
//! **Feature gating (issue #144, PR 3):** the forwarding-only steps --
//! per-upstream selection stats, path strip/rewrite, and traffic mirroring
//! -- exist only with the root `proxy` feature (they act on
//! `UpstreamTarget::Proxy` / `proxy_upstream_url`, which only the proxy
//! routing resolvers produce). What every build keeps: the upstream-timing
//! start mark, forwarded/`Via` headers, the upload loopback header, and the
//! request header transform. Each gated step is a two-variant function, not
//! a `#[cfg]` inside a body (see the #341/#342 lesson in `CLAUDE.md`).

#[cfg(feature = "proxy")]
use dashmap::DashMap;
use pingora_core::Result;
use pingora_http::RequestHeader;
use pingora_proxy::Session;

use crate::proxy::ctx::{RequestCtx, UpstreamTarget};
use crate::proxy::service::ConduitProxy;

/// Body of [`pingora_proxy::ProxyHttp::upstream_request_filter`].
pub(crate) async fn upstream_request_filter(
    proxy: &ConduitProxy,
    session: &mut Session,
    upstream_request: &mut RequestHeader,
    ctx: &mut Option<RequestCtx>,
) -> Result<()> {
    // Record the moment we start forwarding to the upstream so that
    // `logging()` can compute upstream_response_time_ms.
    // Also increment the per-upstream active-connections gauge.
    if let Some(req_ctx) = ctx.as_mut() {
        req_ctx.proxy.upstream_start = Some(std::time::Instant::now());
        record_upstream_selection(proxy, req_ctx);
    }

    append_forwarded_headers(session, upstream_request, &proxy.state, ctx)?;
    apply_upstream_path_transforms(upstream_request, ctx)?;

    // Request header transformation with optional JWT template substitution.
    {
        let config = proxy.state.config.load();
        if let Some(req_ctx) = ctx.as_ref() {
            let site = config.sites.get(req_ctx.site_idx);
            if let Some(transform) = site.and_then(|s| s.request_transform.as_ref()) {
                apply_header_transform_request_with_claims(
                    upstream_request,
                    transform,
                    req_ctx.jwt_claims(),
                )?;
            }
        }
    }

    // Traffic mirroring: fire-and-forget copy to the mirror backend.
    maybe_fire_mirror(session, upstream_request, ctx);

    Ok(())
}

/// Bump the per-upstream active-connections gauge and the selection counters
/// (#40) for the peer this request was routed to -- the `proxy` variant.
/// No-ops when no upstream URL is tracked (e.g. an `upload` target).
#[cfg(feature = "proxy")]
fn record_upstream_selection(proxy: &ConduitProxy, req_ctx: &RequestCtx) {
    if let Some(url) = req_ctx.proxy.proxy_upstream_url.as_deref() {
        proxy
            .state
            .metrics
            .upstream_active_connections
            .with_label_values(&[url])
            .inc();
        // Per-upstream selection counters (#40).
        crate::proxy::health::record_upstream_selected(&proxy.state.upstream_health, url);
    }
}

/// No-`proxy` variant of [`record_upstream_selection`]: `proxy_upstream_url`
/// is only ever set by the proxy routing resolvers, so there is never an
/// upstream to attribute the selection to.
#[cfg(not(feature = "proxy"))]
fn record_upstream_selection(_proxy: &ConduitProxy, _req_ctx: &RequestCtx) {}

/// Fire the traffic-mirror copy when the routed target has one -- the
/// `proxy` variant.
#[cfg(feature = "proxy")]
fn maybe_fire_mirror(
    session: &Session,
    upstream_request: &RequestHeader,
    ctx: &Option<RequestCtx>,
) {
    if let Some(req_ctx) = ctx.as_ref() {
        if let UpstreamTarget::Proxy {
            mirror_url: Some(ref mirror),
            ..
        } = req_ctx.upstream
        {
            fire_mirror_request(mirror, session, upstream_request);
        }
    }
}

/// No-`proxy` variant of [`maybe_fire_mirror`]: mirroring is a forwarding
/// feature, and `mirror_url` only exists on `UpstreamTarget::Proxy`.
#[cfg(not(feature = "proxy"))]
fn maybe_fire_mirror(
    _session: &Session,
    _upstream_request: &RequestHeader,
    _ctx: &Option<RequestCtx>,
) {
}

pub(super) fn extract_host(session: &Session) -> String {
    session
        .req_header()
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).to_owned())
        .unwrap_or_default()
}

/// Append `X-Forwarded-For` and `X-Forwarded-Proto` headers to the upstream request.
pub(super) fn append_forwarded_headers(
    session: &Session,
    upstream_request: &mut RequestHeader,
    state: &crate::proxy::service::AppState,
    ctx: &Option<RequestCtx>,
) -> Result<()> {
    // X-Forwarded-For: chain or start a new entry.
    if let Some(ip) = session
        .client_addr()
        .and_then(|a| a.as_inet())
        .map(|a| a.ip().to_string())
    {
        let xff = match upstream_request
            .headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
        {
            Some(existing) => format!("{existing}, {ip}"),
            None => ip,
        };
        upstream_request.insert_header("x-forwarded-for", xff)?;
    }

    // X-Forwarded-Proto: derive from whether the matched site has TLS.
    let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
    let proto = if state
        .config
        .load()
        .sites
        .get(site_idx)
        .and_then(|s| s.tls.as_ref())
        .is_some()
    {
        "https"
    } else {
        "http"
    };
    upstream_request.insert_header("x-forwarded-proto", proto)?;

    // X-Forwarded-Host: original Host header so the upstream can reconstruct URLs.
    if let Some(host) = session
        .req_header()
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
    {
        upstream_request.insert_header("x-forwarded-host", host)?;
    }

    // Via: RFC 7230 §5.7 — identify the proxy hop.
    // Append to any existing Via header rather than replacing it.
    let via_value = match upstream_request
        .headers
        .get("via")
        .and_then(|v| v.to_str().ok())
    {
        Some(existing) => format!("{existing}, 1.1 conduit"),
        None => "1.1 conduit".to_owned(),
    };
    upstream_request.insert_header("via", via_value)?;

    Ok(())
}

/// Apply strip-prefix and path-rewrite transforms for proxy and upload targets.
pub(super) fn apply_upstream_path_transforms(
    upstream_request: &mut RequestHeader,
    ctx: &Option<RequestCtx>,
) -> Result<()> {
    let Some(ctx_ref) = ctx.as_ref() else {
        return Ok(());
    };
    match &ctx_ref.upstream {
        UpstreamTarget::Proxy {
            strip_prefix,
            rewrite,
            ..
        } => {
            apply_proxy_path_transforms(
                upstream_request,
                strip_prefix.as_deref(),
                rewrite.as_deref(),
            )?;
        }
        #[cfg(feature = "upload")]
        UpstreamTarget::Upload { .. } => {
            upstream_request.insert_header("x-conduit-site-idx", ctx_ref.site_idx.to_string())?;
        }
        _ => {}
    }
    Ok(())
}

/// Apply a proxy target's strip-prefix and rewrite rules to the upstream
/// request URI -- the `proxy` variant.
#[cfg(feature = "proxy")]
fn apply_proxy_path_transforms(
    upstream_request: &mut RequestHeader,
    strip_prefix: Option<&str>,
    rewrite: Option<&[crate::config::schema::RewriteRule]>,
) -> Result<()> {
    let original = upstream_request.uri.path();
    let path = apply_path_strip(original, strip_prefix);
    let path = apply_path_rewrites(&path, rewrite);
    if path != upstream_request.uri.path() {
        let new_uri = rebuild_uri(&upstream_request.uri, &path)?;
        upstream_request.set_uri(new_uri);
    }
    Ok(())
}

/// No-`proxy` variant of [`apply_proxy_path_transforms`]: the strip/rewrite
/// machinery is compiled out, and no `UpstreamTarget::Proxy` exists to carry
/// such rules in the first place.
#[cfg(not(feature = "proxy"))]
fn apply_proxy_path_transforms(
    _upstream_request: &mut RequestHeader,
    _strip_prefix: Option<&str>,
    _rewrite: Option<&[crate::config::schema::RewriteRule]>,
) -> Result<()> {
    Ok(())
}

/// Strip `prefix` from `path`, returning `"/"` when stripping leaves an empty string.
#[cfg(feature = "proxy")]
pub(super) fn apply_path_strip(path: &str, prefix: Option<&str>) -> String {
    let Some(pfx) = prefix else {
        return path.to_owned();
    };
    let stripped = path.strip_prefix(pfx).unwrap_or("/");
    if stripped.is_empty() {
        "/".to_owned()
    } else {
        stripped.to_owned()
    }
}

/// Apply the first matching rewrite rule to `path` and return the (possibly unchanged) result.
#[cfg(feature = "proxy")]
pub(super) fn apply_path_rewrites(
    path: &str,
    rules: Option<&[crate::config::schema::RewriteRule]>,
) -> String {
    let Some(rules) = rules else {
        return path.to_owned();
    };
    let mut out = path.to_owned();
    for rule in rules {
        match get_rewrite_regex(&rule.from) {
            Some(re) if re.is_match(&out) => {
                out = re.replacen(&out, 1, rule.to.as_str()).into_owned();
                break;
            }
            None => {
                tracing::warn!(
                    pattern = %rule.from,
                    "rewrite rule regex error: invalid pattern (skipped)"
                );
            }
            _ => {}
        }
    }
    out
}

/// Apply request header transform with optional JWT template substitution.
///
/// Supports `{{ jwt.<claim> }}` syntax in header values — replaced with the
/// corresponding claim from the decoded JWT payload.  Unknown claims resolve
/// to an empty string.  Static values (no `{{`) are passed through unchanged.
///
/// Called from `upstream_request_filter` after claims are extracted by
/// `do_request_filter`.
pub(super) fn apply_header_transform_request_with_claims(
    req: &mut RequestHeader,
    transform: &crate::config::schema::HeaderTransformConfig,
    jwt_claims: &Option<std::collections::HashMap<String, serde_json::Value>>,
) -> pingora_core::Result<()> {
    if let Some(remove) = &transform.remove_headers {
        for name in remove {
            req.headers.remove(name.as_str());
        }
    }
    if let Some(set) = &transform.set_headers {
        for (name, value) in set {
            let resolved = if value.contains("{{") {
                crate::util::jwt_template::expand_jwt_templates(value, jwt_claims)
            } else {
                value.clone()
            };
            req.insert_header(name.clone(), resolved)?;
        }
    }
    Ok(())
}

/// Fire-and-forget a copy of the current request to a mirror backend.
///
/// The mirror task is detached (spawned with `tokio::spawn`); its response is
/// discarded and any error is silently logged at DEBUG level.  The primary
/// request processing is unaffected by mirror success or failure.
///
/// **V1 limitation:** only the method, path, query, and request headers are
/// mirrored.  The request body is not buffered and is therefore not mirrored.
#[cfg(feature = "proxy")]
pub(super) fn fire_mirror_request(
    mirror_url: &str,
    session: &Session,
    upstream_request: &RequestHeader,
) {
    // Build the mirror URL: base URL + path + query from the upstream request.
    let path_and_query = upstream_request
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or_else(|| upstream_request.uri.path());

    let target_url = {
        let base = mirror_url.trim_end_matches('/');
        format!("{base}{path_and_query}")
    };

    // Collect request headers (skip hop-by-hop and host).
    let method = upstream_request.method.clone();
    let mut headers = Vec::new();
    for (name, value) in upstream_request.headers.iter() {
        let n = name.as_str().to_ascii_lowercase();
        if matches!(
            n.as_str(),
            "connection"
                | "keep-alive"
                | "transfer-encoding"
                | "te"
                | "trailer"
                | "upgrade"
                | "proxy-authorization"
                | "proxy-authenticate"
                | "host"
        ) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            headers.push((n, v.to_owned()));
        }
    }
    // Add X-Mirrored-From so the mirror can distinguish shadow traffic.
    let primary_host = session
        .req_header()
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_owned();
    headers.push(("x-mirrored-from".to_owned(), primary_host));

    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, "mirror: failed to build client");
                return;
            }
        };

        let mut req = client.request(
            reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET),
            &target_url,
        );
        for (name, value) in &headers {
            if let Ok(header_name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) {
                if let Ok(header_value) = reqwest::header::HeaderValue::from_str(value) {
                    req = req.header(header_name, header_value);
                }
            }
        }

        match req.send().await {
            Ok(resp) => {
                tracing::debug!(
                    url = %target_url,
                    status = resp.status().as_u16(),
                    "mirror: response received (discarded)"
                );
            }
            Err(e) => {
                tracing::debug!(url = %target_url, error = %e, "mirror: request failed");
            }
        }
    });
}

/// Return a compiled [`regex::Regex`] for `pattern`, using a process-wide cache
/// to avoid recompiling the same pattern on every request.
///
/// Rewrite patterns are plain (un-anchored) regexes so that `replacen` can
/// match anywhere in the path.  Invalid patterns are not stored; the caller
/// should log the error and skip the rule.
#[cfg(feature = "proxy")]
pub(super) fn get_rewrite_regex(pattern: &str) -> Option<regex::Regex> {
    static CACHE: std::sync::OnceLock<DashMap<String, regex::Regex>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(DashMap::new);
    if let Some(re) = cache.get(pattern) {
        return Some(re.clone());
    }
    let re = regex::Regex::new(pattern).ok()?;
    cache.insert(pattern.to_owned(), re.clone());
    Some(re)
}

#[cfg(feature = "proxy")]
pub(super) fn rebuild_uri(original: &http::Uri, new_path: &str) -> Result<http::Uri> {
    let pq = match original.query() {
        Some(q) => format!("{new_path}?{q}"),
        None => new_path.to_string(),
    };
    let mut parts = http::uri::Parts::default();
    parts.scheme = original.scheme().cloned();
    parts.authority = original.authority().cloned();
    parts.path_and_query = Some(pq.parse().map_err(|_| {
        pingora_core::Error::explain(
            pingora_core::ErrorType::InternalError,
            "failed to build upstream URI",
        )
    })?);
    http::Uri::from_parts(parts).map_err(|_| {
        pingora_core::Error::explain(
            pingora_core::ErrorType::InternalError,
            "failed to build upstream URI",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::proxy::ctx::ProxyReqState;

    // Duplicated across this file, `handlers.rs`, `peer.rs`, and `retry.rs`'s
    // test modules -- `peer.rs` holds the "canonical" original location.
    fn make_ctx(upstream: UpstreamTarget) -> RequestCtx {
        RequestCtx::new(0, upstream, ProxyReqState::default(), None)
    }

    // ── apply_upstream_path_transforms ───────────────────────────────────────

    #[test]
    fn apply_upstream_path_transforms_none_ctx_noop() {
        use pingora_http::RequestHeader;
        let mut req = RequestHeader::build("GET", b"/original", None).unwrap();
        apply_upstream_path_transforms(&mut req, &None).unwrap();
        assert_eq!(req.uri.path(), "/original");
    }

    /// Without `proxy` there is no `UpstreamTarget::Proxy` in practice, and the
    /// strip/rewrite transforms are compiled out entirely. Pins that gate: the
    /// exact input of `apply_upstream_path_transforms_strips_prefix` (which
    /// rewrites `/api/v1/users` to `/v1/users` with `proxy` on) must leave the
    /// path untouched here, not half-apply a transform whose code is gone.
    #[cfg(not(feature = "proxy"))]
    #[test]
    fn apply_upstream_path_transforms_leaves_path_alone_without_proxy() {
        use pingora_http::RequestHeader;
        let mut req = RequestHeader::build("GET", b"/api/v1/users", None).unwrap();
        let ctx = Some(make_ctx(UpstreamTarget::Proxy {
            addr: "backend:4000".to_owned(),
            tls: false,
            sni: String::new(),
            strip_prefix: Some("/api".to_owned()),
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        }));
        apply_upstream_path_transforms(&mut req, &ctx).unwrap();
        assert_eq!(req.uri.path(), "/api/v1/users");
    }

    // ── apply_header_transform_request_with_claims ────────────────────────────

    #[test]
    fn header_transform_sets_header() {
        use pingora_http::RequestHeader;
        let mut req = RequestHeader::build("GET", b"/api", None).unwrap();
        let transform = crate::config::schema::HeaderTransformConfig {
            set_headers: Some(
                [("x-env".to_owned(), "production".to_owned())]
                    .iter()
                    .cloned()
                    .collect(),
            ),
            remove_headers: None,
        };
        apply_header_transform_request_with_claims(&mut req, &transform, &None).unwrap();
        assert_eq!(req.headers.get("x-env").unwrap(), "production");
    }

    #[test]
    fn header_transform_removes_header() {
        use pingora_http::RequestHeader;
        let mut req = RequestHeader::build("GET", b"/api", None).unwrap();
        req.insert_header("x-remove", "bye").unwrap();
        let transform = crate::config::schema::HeaderTransformConfig {
            set_headers: None,
            remove_headers: Some(vec!["x-remove".to_owned()]),
        };
        apply_header_transform_request_with_claims(&mut req, &transform, &None).unwrap();
        assert!(
            req.headers.get("x-remove").is_none(),
            "header must be removed"
        );
    }

    #[test]
    fn header_transform_with_jwt_template_substitution() {
        use pingora_http::RequestHeader;
        use std::collections::HashMap;
        let mut req = RequestHeader::build("GET", b"/api", None).unwrap();
        let transform = crate::config::schema::HeaderTransformConfig {
            set_headers: Some(
                [("x-user".to_owned(), "{{ jwt.sub }}".to_owned())]
                    .iter()
                    .cloned()
                    .collect(),
            ),
            remove_headers: None,
        };
        let mut claims = HashMap::new();
        claims.insert("sub".to_owned(), serde_json::json!("alice"));
        apply_header_transform_request_with_claims(&mut req, &transform, &Some(claims)).unwrap();
        assert_eq!(req.headers.get("x-user").unwrap(), "alice");
    }

    // Everything below exercises path strip/rewrite/URI helpers that only
    // exist with the root `proxy` feature.
    #[cfg(feature = "proxy")]
    mod proxy_only {
        use super::*;

        // ── apply_path_strip ──────────────────────────────────────────────────────

        #[test]
        fn apply_path_strip_removes_prefix() {
            assert_eq!(apply_path_strip("/api/v1/users", Some("/api")), "/v1/users");
        }

        #[test]
        fn apply_path_strip_no_prefix_returns_unchanged() {
            assert_eq!(apply_path_strip("/api/v1", None), "/api/v1");
        }

        #[test]
        fn apply_path_strip_exact_match_returns_root() {
            // Stripping the exact path leaves an empty string → normalize to "/".
            assert_eq!(apply_path_strip("/api", Some("/api")), "/");
        }

        #[test]
        fn apply_path_strip_no_match_returns_root() {
            // Prefix doesn't match → `strip_prefix` returns None → "/" returned.
            assert_eq!(apply_path_strip("/other", Some("/api")), "/");
        }

        // ── apply_path_rewrites ───────────────────────────────────────────────────

        #[test]
        fn apply_path_rewrites_no_rules_returns_unchanged() {
            assert_eq!(apply_path_rewrites("/v1/users", None), "/v1/users");
        }

        #[test]
        fn apply_path_rewrites_no_match_returns_unchanged() {
            let rules = vec![crate::config::schema::RewriteRule {
                from: "^/api/(.*)".to_owned(),
                to: "/v2/$1".to_owned(),
            }];
            assert_eq!(
                apply_path_rewrites("/other/path", Some(&rules)),
                "/other/path"
            );
        }

        #[test]
        fn apply_path_rewrites_matching_rule_transforms_path() {
            let rules = vec![crate::config::schema::RewriteRule {
                from: "^/v1/(.*)".to_owned(),
                to: "/v2/$1".to_owned(),
            }];
            assert_eq!(apply_path_rewrites("/v1/users", Some(&rules)), "/v2/users");
        }

        #[test]
        fn apply_path_rewrites_first_match_wins() {
            let rules = vec![
                crate::config::schema::RewriteRule {
                    from: "^/v1/(.*)".to_owned(),
                    to: "/first/$1".to_owned(),
                },
                crate::config::schema::RewriteRule {
                    from: "^/v1/(.*)".to_owned(),
                    to: "/second/$1".to_owned(),
                },
            ];
            assert_eq!(
                apply_path_rewrites("/v1/users", Some(&rules)),
                "/first/users"
            );
        }

        // ── get_rewrite_regex ─────────────────────────────────────────────────────

        #[test]
        fn get_rewrite_regex_compiles_valid_pattern() {
            let re = get_rewrite_regex("^/v1/(.*)");
            assert!(re.is_some(), "valid regex must compile");
            let re = re.unwrap();
            assert!(re.is_match("/v1/users"));
            assert!(!re.is_match("/v2/users"));
        }

        #[test]
        fn get_rewrite_regex_returns_none_for_invalid() {
            let re = get_rewrite_regex("[invalid");
            assert!(re.is_none(), "invalid regex must return None");
        }

        #[test]
        fn get_rewrite_regex_caches_compiled_pattern() {
            // Second call for the same pattern must return a cached copy.
            let r1 = get_rewrite_regex("^/api/(.*)");
            let r2 = get_rewrite_regex("^/api/(.*)");
            assert!(r1.is_some() && r2.is_some(), "both calls must succeed");
        }

        // ── apply_upstream_path_transforms ───────────────────────────────────────

        #[test]
        fn apply_upstream_path_transforms_strips_prefix() {
            use pingora_http::RequestHeader;
            let mut req = RequestHeader::build("GET", b"/api/v1/users", None).unwrap();
            let ctx = Some(make_ctx(UpstreamTarget::Proxy {
                addr: "backend:4000".to_owned(),
                tls: false,
                sni: String::new(),
                strip_prefix: Some("/api".to_owned()),
                rewrite: None,
                mirror_url: None,
                upstream_tls: None,
            }));
            apply_upstream_path_transforms(&mut req, &ctx).unwrap();
            assert_eq!(req.uri.path(), "/v1/users");
        }

        #[test]
        fn apply_upstream_path_transforms_no_prefix_unchanged() {
            use pingora_http::RequestHeader;
            let mut req = RequestHeader::build("GET", b"/api/v1/users", None).unwrap();
            let ctx = Some(make_ctx(UpstreamTarget::Proxy {
                addr: "backend:4000".to_owned(),
                tls: false,
                sni: String::new(),
                strip_prefix: None,
                rewrite: None,
                mirror_url: None,
                upstream_tls: None,
            }));
            apply_upstream_path_transforms(&mut req, &ctx).unwrap();
            assert_eq!(req.uri.path(), "/api/v1/users");
        }

        // ── rebuild_uri ───────────────────────────────────────────────────────────

        #[test]
        fn rebuild_uri_replaces_path() {
            let original: http::Uri = "/old/path".parse().unwrap();
            let new_uri = rebuild_uri(&original, "/new/path").unwrap();
            assert_eq!(new_uri.path(), "/new/path");
        }

        #[test]
        fn rebuild_uri_preserves_query() {
            let original: http::Uri = "/old/path?foo=bar".parse().unwrap();
            let new_uri = rebuild_uri(&original, "/new/path").unwrap();
            assert_eq!(new_uri.path(), "/new/path");
            assert_eq!(new_uri.query(), Some("foo=bar"));
        }

        #[test]
        fn rebuild_uri_no_query_keeps_no_query() {
            let original: http::Uri = "/path".parse().unwrap();
            let new_uri = rebuild_uri(&original, "/v2").unwrap();
            assert_eq!(new_uri.path(), "/v2");
            assert!(new_uri.query().is_none());
        }
    }
}
