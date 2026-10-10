//! Unit tests of the Admin API handlers (moved from `src/admin/api.rs`, issue #146).

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
#[cfg(feature = "cache")]
use axum::Json;
use serde_json::Value;
use std::sync::Arc;

use conduit_runtime::proxy::service::AppState;

use super::*;

// ── cache purge (issue #144, PR 4b) ──────────────────────────────────────

/// `NotImplemented` is a 501 with the usual `{"status":"error","message":…}`
/// body -- unconditional, so it is exercised in every build.
#[tokio::test]
async fn not_implemented_is_501_with_error_body() {
    let resp = AdminError::NotImplemented("nope".to_owned()).into_response();
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["status"], "error");
    assert_eq!(v["message"], "nope");
}

/// Without the `cache` feature the route stays registered but must answer
/// 501 -- not `{"purged":false}`, which would claim there was a store to
/// purge and that the entry simply did not exist.
#[cfg(not(feature = "cache"))]
#[tokio::test]
async fn cache_purge_without_cache_feature_answers_501() {
    let err = cache_purge_handler(
        State(app_state_for_ip_deny()),
        Query(CachePurgeParams {
            url: "http://example.com/x".to_owned(),
        }),
    )
    .await
    .expect_err("there is no response cache to purge in this build");
    assert_eq!(err.into_response().status(), StatusCode::NOT_IMPLEMENTED);
}

/// With the feature, purging an URL that was never cached is an ordinary
/// success that reports `purged: false`.
#[cfg(feature = "cache")]
#[tokio::test]
async fn cache_purge_with_cache_feature_reports_not_purged_for_unknown_url() {
    let Json(v) = cache_purge_handler(
        State(app_state_for_ip_deny()),
        Query(CachePurgeParams {
            url: "http://example.com/never-cached".to_owned(),
        }),
    )
    .await
    .expect("a well-formed http URL is accepted");
    assert_eq!(v["status"], "ok");
    assert_eq!(v["purged"], false);
}

/// Issues #444 and #482: the purge must target the keys the request path stores. That path keys on
/// the `Host` header *without its port* plus the **listener port** the site serves on, so a URL's
/// own port is only a way to pick the listener, never part of the host.
#[cfg(feature = "cache")]
#[test]
fn purge_keys_of_a_url_are_the_keys_the_request_path_stores() {
    let keys = |url: &str, ports: &[u16]| -> Vec<_> {
        purge_cache_keys(url, ports)
            .expect("valid url")
            .iter()
            .map(|k| k.to_compact().primary)
            .collect()
    };
    let stored = |host: &str, port: u16, path: &str, query: Option<&str>| {
        conduit_runtime::proxy::cache::build_cache_key(host, port, "http", path, query, None, None)
            .to_compact()
            .primary
    };
    // an explicit URL port picks that one listener, whatever the configured ports are
    assert_eq!(
        keys("http://example.com:8080/x?a=1", &[80, 9090]),
        vec![stored("example.com", 8080, "/x", Some("a=1"))]
    );
    // no port in the URL: every configured listener port (a port mapping hides the real one)
    assert_eq!(
        keys("http://example.com/x?a=1", &[8080, 9090]),
        vec![
            stored("example.com", 8080, "/x", Some("a=1")),
            stored("example.com", 9090, "/x", Some("a=1")),
        ]
    );
    // the `url` crate lowercases the host, and the stored key is lowercase for a lowercase Host
    assert_eq!(
        keys("http://EXAMPLE.com:8080/x", &[]),
        vec![stored("example.com", 8080, "/x", None)]
    );
    // a bracketed IPv6 literal keeps its brackets and loses its port, like the request side
    assert_eq!(
        keys("http://[::1]:8080/x", &[]),
        vec![stored("[::1]", 8080, "/x", None)]
    );
}

/// The listener ports a purge fans out over follow `classify_ports`: `site.port`, else 443 with
/// `tls` and 80 without, 8080 for an empty site list, TCP-proxy sites excluded, duplicates folded.
#[cfg(feature = "cache")]
#[test]
fn listener_ports_follow_the_servers_defaults() {
    use conduit_config::schema::SiteConfig;
    let site = |port: Option<u16>| SiteConfig {
        port,
        ..Default::default()
    };
    assert_eq!(listener_ports(&[]), vec![8080]);
    assert_eq!(
        listener_ports(&[
            site(Some(9090)),
            site(None),
            site(Some(9090)),
            site(Some(8080))
        ]),
        vec![80, 8080, 9090]
    );
}

/// With the feature, a non-http(s) scheme is rejected as a bad request
/// before the cache is touched.
#[cfg(feature = "cache")]
#[tokio::test]
async fn cache_purge_with_cache_feature_rejects_non_http_scheme() {
    let err = cache_purge_handler(
        State(app_state_for_ip_deny()),
        Query(CachePurgeParams {
            url: "ftp://example.com/x".to_owned(),
        }),
    )
    .await
    .expect_err("ftp is not a cacheable scheme");
    assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
}

// ── validate_cidr ────────────────────────────────────────────────────────

#[test]
fn valid_ipv4_cidr() {
    assert!(validate_cidr("192.168.1.0/24"));
    assert!(validate_cidr("10.0.0.0/8"));
    assert!(validate_cidr("0.0.0.0/0"));
    assert!(validate_cidr("255.255.255.255/32"));
}

#[test]
fn valid_ipv6_cidr() {
    assert!(validate_cidr("2001:db8::/32"));
    assert!(validate_cidr("::/0"));
    assert!(validate_cidr("::1/128"));
}

#[test]
fn valid_single_ips() {
    assert!(validate_cidr("192.168.1.1"));
    assert!(validate_cidr("::1"));
    assert!(validate_cidr("10.0.0.1"));
}

#[test]
fn invalid_cidrs() {
    assert!(!validate_cidr("not-a-cidr"));
    assert!(!validate_cidr("999.999.999.999"));
    assert!(!validate_cidr("192.168.1.0/99")); // prefix > 32 for IPv4
    assert!(!validate_cidr("192.168.1.0/abc")); // non-numeric prefix
    assert!(!validate_cidr(""));
}

// ── ip_deny_add_handler (direct call, no HTTP server needed) ─────────────

fn app_state_for_ip_deny() -> Arc<AppState> {
    Arc::new(AppState::new(
        conduit_config::schema::AppConfig::default(),
        std::path::PathBuf::from("."),
        None,
    ))
}

#[tokio::test]
async fn ip_deny_add_handler_valid_cidr_adds_to_list() {
    let state = app_state_for_ip_deny();
    let result = ip_deny_add_handler(
        State(state.clone()),
        axum::Json(IpDenyBody {
            cidr: "203.0.113.0/24".to_owned(),
        }),
    )
    .await;
    let body = result.expect("valid CIDR must be accepted").0;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["action"], "added");
    assert_eq!(body["cidr"], "203.0.113.0/24");
    assert_eq!(
        state.dynamic_deny.read().unwrap().as_slice(),
        ["203.0.113.0/24"]
    );
}

#[tokio::test]
async fn ip_deny_add_handler_invalid_cidr_returns_bad_request() {
    let state = app_state_for_ip_deny();
    let result = ip_deny_add_handler(
        State(state.clone()),
        axum::Json(IpDenyBody {
            cidr: "not-an-ip".to_owned(),
        }),
    )
    .await;
    assert!(
        matches!(result, Err(AdminError::BadRequest(_))),
        "an invalid CIDR must be rejected as a typed AdminError::BadRequest"
    );
    assert!(
        state.dynamic_deny.read().unwrap().is_empty(),
        "the rejected entry must not have been added"
    );
}

#[tokio::test]
async fn ip_deny_add_handler_duplicate_cidr_not_added_twice() {
    let state = app_state_for_ip_deny();
    for _ in 0..2 {
        let _ = ip_deny_add_handler(
            State(state.clone()),
            axum::Json(IpDenyBody {
                cidr: "10.0.0.0/8".to_owned(),
            }),
        )
        .await
        .expect("valid CIDR must be accepted");
    }
    assert_eq!(state.dynamic_deny.read().unwrap().len(), 1);
}

#[tokio::test]
async fn ip_deny_remove_handler_removes_from_list() {
    let state = app_state_for_ip_deny();
    state
        .dynamic_deny
        .write()
        .unwrap()
        .push("192.0.2.0/24".to_owned());
    let result = ip_deny_remove_handler(
        State(state.clone()),
        axum::Json(IpDenyBody {
            cidr: "192.0.2.0/24".to_owned(),
        }),
    )
    .await;
    assert_eq!(result.0["status"], "ok");
    assert_eq!(result.0["action"], "removed");
    assert!(state.dynamic_deny.read().unwrap().is_empty());
}

// ── rate_limits_handler (#303/#304) ───────────────────────────────────────

fn bucket_with_counts(
    passed: u64,
    rejected: u64,
) -> conduit_runtime::filter::rate_limit::TokenBucket {
    let mut b = conduit_runtime::filter::rate_limit::TokenBucket::new(100, 0, 60);
    b.passed = passed;
    b.rejected = rejected;
    b
}

#[tokio::test]
async fn rate_limits_handler_parses_site_and_route_namespaces() {
    let state = app_state_for_ip_deny();
    state.rate_limiter.insert(
        conduit_runtime::filter::rate_limit::site_key("app.example.com:8080", "1.2.3.4"),
        bucket_with_counts(10, 1),
    );
    state.rate_limiter.insert(
        conduit_runtime::filter::rate_limit::route_key("app.example.com:8080", "/api", "1.2.3.4"),
        bucket_with_counts(20, 2),
    );

    let result = rate_limits_handler(State(state)).await;
    let site = &result.0["app.example.com:8080"];
    assert_eq!(site["*"]["passed"], 10);
    assert_eq!(site["*"]["rejected"], 1);
    assert_eq!(site["/api"]["passed"], 20);
    assert_eq!(site["/api"]["rejected"], 2);
}

#[tokio::test]
async fn rate_limits_handler_excludes_consumer_buckets() {
    let state = app_state_for_ip_deny();
    state.rate_limiter.insert(
        conduit_runtime::filter::rate_limit::consumer_key("alice"),
        bucket_with_counts(5, 0),
    );

    let result = rate_limits_handler(State(state)).await;
    assert_eq!(
        result.0,
        serde_json::json!({}),
        "consumer buckets are global, not attributable to a site — must not appear here"
    );
}

#[tokio::test]
async fn rate_limits_handler_aggregates_multiple_clients_into_one_site_route_total() {
    // Two different clients hitting the same site's same route — bucket
    // keys differ (per-client), but the handler must report one summed
    // total for the (site, route) pair, not two separate entries.
    let state = app_state_for_ip_deny();
    state.rate_limiter.insert(
        conduit_runtime::filter::rate_limit::route_key("a.example.com:80", "/x", "1.1.1.1"),
        bucket_with_counts(3, 1),
    );
    state.rate_limiter.insert(
        conduit_runtime::filter::rate_limit::route_key("a.example.com:80", "/x", "2.2.2.2"),
        bucket_with_counts(4, 0),
    );

    let result = rate_limits_handler(State(state)).await;
    let route = &result.0["a.example.com:80"]["/x"];
    assert_eq!(route["passed"], 7);
    assert_eq!(route["rejected"], 1);
}

#[tokio::test]
async fn rate_limits_handler_keeps_two_sites_separate_regression_304() {
    // The bug #304 was filed against: two sites sharing a client key
    // must not collide into one bucket. Since the fix scopes the key by
    // site_label, they naturally land as two distinct handler entries.
    let state = app_state_for_ip_deny();
    state.rate_limiter.insert(
        conduit_runtime::filter::rate_limit::site_key("site-a.example.com:80", "9.9.9.9"),
        bucket_with_counts(1, 0),
    );
    state.rate_limiter.insert(
        conduit_runtime::filter::rate_limit::site_key("site-b.example.com:80", "9.9.9.9"),
        bucket_with_counts(2, 0),
    );

    let result = rate_limits_handler(State(state)).await;
    assert_eq!(result.0["site-a.example.com:80"]["*"]["passed"], 1);
    assert_eq!(result.0["site-b.example.com:80"]["*"]["passed"], 2);
}

// ── AdminError ───────────────────────────────────────────────────────────

#[test]
fn bad_request_produces_error_status_json() {
    use axum::response::IntoResponse;
    let err = AdminError::BadRequest("test error".to_owned());
    let resp = err.into_response();
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
}

#[test]
fn server_error_produces_500_status() {
    use axum::response::IntoResponse;
    let err = AdminError::ServerError("internal".to_owned());
    let resp = err.into_response();
    assert_eq!(resp.status(), axum::http::StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn cold_fields_changed_produces_400() {
    use axum::response::IntoResponse;
    let err = AdminError::ColdFieldsChanged {
        message: "restart required: sites[0].port".to_owned(),
        fields: vec!["sites[0].port".to_owned()],
    };
    let resp = err.into_response();
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
}

// ── make_site_label ──────────────────────────────────────────────────────

#[test]
fn site_label_with_host_and_port() {
    let label = make_site_label(&Some("api.example.com".to_owned()), Some(443));
    assert!(label.contains("api.example.com"));
    assert!(label.contains("443"));
}

#[test]
fn site_label_without_host() {
    let label = make_site_label(&None, Some(8080));
    assert!(label.contains("8080"));
}

#[test]
fn site_label_both_none() {
    let label = make_site_label(&None, None);
    // Must not panic; some placeholder is returned.
    assert!(!label.is_empty());
}

// ── validate_cidr — additional edge cases ─────────────────────────────────

#[test]
fn validate_cidr_ipv4_prefix_32_valid() {
    assert!(validate_cidr("192.168.1.1/32"), "/32 is valid for IPv4");
}

#[test]
fn validate_cidr_ipv4_prefix_33_invalid() {
    assert!(!validate_cidr("10.0.0.0/33"), "/33 is invalid for IPv4");
}

#[test]
fn validate_cidr_ipv6_prefix_128_valid() {
    assert!(validate_cidr("::1/128"), "/128 is valid for IPv6");
}

#[test]
fn validate_cidr_ipv6_prefix_129_invalid() {
    assert!(!validate_cidr("::1/129"), "/129 is invalid for IPv6");
}

#[test]
fn validate_cidr_whitespace_invalid() {
    assert!(
        !validate_cidr(" 192.168.1.0/24"),
        "leading space is invalid"
    );
    assert!(
        !validate_cidr("192.168.1.0/24 "),
        "trailing space is invalid"
    );
}

// ── subtle_eq ─────────────────────────────────────────────────────────────

#[test]
fn subtle_eq_equal_slices() {
    assert!(subtle_eq(b"secret", b"secret"));
}

#[test]
fn subtle_eq_different_slices() {
    assert!(!subtle_eq(b"secret", b"wrong!"));
}

#[test]
fn subtle_eq_different_lengths() {
    assert!(!subtle_eq(b"short", b"longer-value"));
}

#[test]
fn subtle_eq_empty_slices() {
    assert!(subtle_eq(b"", b""));
}

// ── strategy_label ────────────────────────────────────────────────────────

#[test]
fn strategy_label_all_variants() {
    use conduit_config::schema::LoadBalanceStrategy as S;
    assert_eq!(strategy_label(&S::RoundRobin), "round-robin");
    assert_eq!(
        strategy_label(&S::WeightedRoundRobin),
        "weighted-round-robin"
    );
    assert_eq!(strategy_label(&S::Random), "random");
    assert_eq!(strategy_label(&S::LeastConn), "least-conn");
    assert_eq!(strategy_label(&S::LeastResponseTime), "least-response-time");
    assert_eq!(strategy_label(&S::IpHash), "ip-hash");
    assert_eq!(strategy_label(&S::ConsistentHash), "consistent-hash");
    assert_eq!(strategy_label(&S::P2c), "p2c");
}

// ── proxy_target_url_weight ───────────────────────────────────────────────

#[test]
fn proxy_target_simple_has_weight_one() {
    use conduit_config::schema::ProxyTarget;
    let t = ProxyTarget::Simple("http://backend:4000".to_owned());
    let (url, weight) = proxy_target_url_weight(&t);
    assert_eq!(url, "http://backend:4000");
    assert_eq!(weight, 1);
}

#[test]
fn proxy_target_weighted_uses_configured_weight() {
    use conduit_config::schema::{ProxyTarget, WeightedTarget};
    let t = ProxyTarget::Weighted(WeightedTarget {
        url: "http://backend:4000".to_owned(),
        weight: 5,
    });
    let (url, weight) = proxy_target_url_weight(&t);
    assert_eq!(url, "http://backend:4000");
    assert_eq!(weight, 5);
}

// ── atomic_write ──────────────────────────────────────────────────────────

#[test]
fn atomic_write_creates_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("output.txt");
    atomic_write(path.to_str().unwrap(), b"hello atomic").expect("atomic_write must succeed");
    let content = std::fs::read_to_string(&path).expect("file must exist");
    assert_eq!(content, "hello atomic");
}

#[test]
fn atomic_write_no_tmp_file_left_on_success() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("output.txt");
    atomic_write(path.to_str().unwrap(), b"data").expect("must succeed");
    let tmp = dir.path().join("output.txt.tmp");
    assert!(!tmp.exists(), ".tmp file must be removed after rename");
}

#[test]
fn atomic_write_overwrites_existing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.txt");
    atomic_write(path.to_str().unwrap(), b"v1").unwrap();
    atomic_write(path.to_str().unwrap(), b"v2").unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "v2", "second write must overwrite first");
}

// ── url_health_entry ──────────────────────────────────────────────────────

#[test]
fn url_health_entry_unknown_url_returns_null_health() {
    let reg = conduit_runtime::proxy::health::UpstreamRegistry::new();
    let entry = url_health_entry(&reg, "http://unknown:4000", 1, None);
    assert_eq!(entry["url"], "http://unknown:4000");
    assert_eq!(entry["weight"], 1);
    assert!(
        entry["healthy"].is_null(),
        "unknown URL must have null health"
    );
}

#[test]
fn url_health_entry_known_url_includes_health_data() {
    let reg = conduit_runtime::proxy::health::UpstreamRegistry::new();
    {
        let mut e = reg
            .statuses
            .entry("http://backend:4000".to_owned())
            .or_default();
        e.healthy = true;
        e.latency_ms = Some(15);
        e.consecutive_failures = 0;
        e.consecutive_successes = 5;
    }
    let entry = url_health_entry(&reg, "http://backend:4000", 2, None);
    assert_eq!(entry["healthy"], true);
    assert_eq!(entry["latency_ms"], 15);
    assert_eq!(entry["consecutive_failures"], 0);
    assert_eq!(entry["consecutive_successes"], 5);
    assert_eq!(entry["weight"], 2);
}

#[test]
fn url_health_entry_with_group_includes_group_field() {
    let reg = conduit_runtime::proxy::health::UpstreamRegistry::new();
    let entry = url_health_entry(&reg, "http://a:4000", 1, Some("primary"));
    assert_eq!(entry["group"], "primary");
}

// ── format_proxy_route_targets ────────────────────────────────────────────

#[test]
fn format_proxy_route_targets_url_variant() {
    use conduit_config::schema::ProxyRouteTarget;
    use conduit_runtime::proxy::health::UpstreamRegistry;
    let reg = UpstreamRegistry::new();
    let rt = ProxyRouteTarget::Url("http://a:4000".to_owned());
    let (strategy, targets) = format_proxy_route_targets(&rt, &reg);
    assert_eq!(strategy, "round-robin");
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0]["url"], "http://a:4000");
}

#[test]
fn format_proxy_route_targets_round_robin_variant() {
    use conduit_config::schema::ProxyRouteTarget;
    use conduit_runtime::proxy::health::UpstreamRegistry;
    let reg = UpstreamRegistry::new();
    let rt =
        ProxyRouteTarget::RoundRobin(vec!["http://a:4000".to_owned(), "http://b:4000".to_owned()]);
    let (strategy, targets) = format_proxy_route_targets(&rt, &reg);
    assert_eq!(strategy, "round-robin");
    assert_eq!(targets.len(), 2);
}

#[test]
fn format_full_config_targets_no_groups() {
    use conduit_config::schema::{ProxyRouteConfig, ProxyTarget};
    use conduit_runtime::proxy::health::UpstreamRegistry;
    let reg = UpstreamRegistry::new();
    let cfg = ProxyRouteConfig {
        targets: vec![
            ProxyTarget::Simple("http://a:4000".to_owned()),
            ProxyTarget::Simple("http://b:4000".to_owned()),
        ],
        ..Default::default()
    };
    let result = format_full_config_targets(&cfg, &reg);
    assert_eq!(result.len(), 2);
    let urls: Vec<&str> = result.iter().filter_map(|e| e["url"].as_str()).collect();
    assert!(urls.contains(&"http://a:4000"));
    assert!(urls.contains(&"http://b:4000"));
}

// ── collect_site_proxy_entries ────────────────────────────────────────────

#[test]
fn collect_site_proxy_entries_empty_site() {
    use conduit_config::schema::SiteConfig;
    let site = SiteConfig::default();
    let entries = collect_site_proxy_entries(&site);
    assert!(entries.is_empty(), "empty site must yield empty entries");
}

#[test]
fn collect_site_proxy_entries_from_routes_map() {
    use conduit_config::schema::{ProxyConfig, ProxyRouteTarget, SiteConfig};
    use indexmap::IndexMap;
    let mut routes = IndexMap::new();
    routes.insert(
        "/api".to_string(),
        ProxyRouteTarget::Url("http://backend:4000".to_string()),
    );
    let site = SiteConfig {
        proxy: Some(ProxyConfig::Routes(routes)),
        ..Default::default()
    };
    let entries = collect_site_proxy_entries(&site);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, "/api");
}

// ── build_flat_upstream_list ──────────────────────────────────────────────

#[test]
fn build_flat_upstream_list_empty_registry() {
    let reg = conduit_runtime::proxy::health::UpstreamRegistry::new();
    let list = build_flat_upstream_list(&reg);
    assert!(list.is_empty(), "empty registry must return empty list");
}

#[test]
fn build_flat_upstream_list_includes_known_urls() {
    use conduit_runtime::proxy::health::UpstreamRegistry;
    let reg = UpstreamRegistry::new();
    reg.statuses.entry("http://a:4000".to_owned()).or_default();
    reg.statuses.entry("http://b:4000".to_owned()).or_default();
    let list = build_flat_upstream_list(&reg);
    assert_eq!(list.len(), 2);
    let urls: Vec<&str> = list.iter().filter_map(|e| e["url"].as_str()).collect();
    assert!(urls.contains(&"http://a:4000"));
    assert!(urls.contains(&"http://b:4000"));
}

#[test]
fn build_flat_upstream_list_sorted_by_url() {
    use conduit_runtime::proxy::health::UpstreamRegistry;
    let reg = UpstreamRegistry::new();
    reg.statuses.entry("http://z:4000".to_owned()).or_default();
    reg.statuses.entry("http://a:4000".to_owned()).or_default();
    let list = build_flat_upstream_list(&reg);
    assert_eq!(list[0]["url"], "http://a:4000");
    assert_eq!(list[1]["url"], "http://z:4000");
}

// ── resolve_runtime_targets ───────────────────────────────────────────────

#[test]
fn resolve_runtime_targets_no_overrides_returns_config() {
    use conduit_runtime::proxy::health::UpstreamRegistry;
    let reg = UpstreamRegistry::new();
    let config_targets = vec![serde_json::json!({"url": "http://config:4000"})];
    let result = resolve_runtime_targets(&reg, "*", "/api", config_targets.clone());
    assert_eq!(
        result, config_targets,
        "no overrides → config targets returned"
    );
}

#[test]
fn resolve_runtime_targets_with_overrides_returns_overrides() {
    use conduit_runtime::proxy::health::UpstreamRegistry;
    let reg = UpstreamRegistry::new();
    reg.add_upstream("*", "/api", "http://override:4000", 2);
    let config_targets = vec![serde_json::json!({"url": "http://config:4000"})];
    let result = resolve_runtime_targets(&reg, "*", "/api", config_targets);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0]["url"], "http://override:4000");
    assert_eq!(result[0]["runtime"], true);
}
