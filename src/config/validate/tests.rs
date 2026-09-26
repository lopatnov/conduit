use super::warnings::sanitize_for_log;
use super::{feature_warnings, partition_by_severity, validate, Severity, ValidationError};
use crate::config::from_str;
use crate::config::schema::AppConfig;

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

// ── `proxy` feature warnings (#144 PR 2) ─────────────────────────────────

/// One config with both a legacy `proxy` and a `routes[]` proxy action.
const PROXY_CONFIG: &str = r#"{
        "port": 8080,
        "proxy": "http://backend:4000",
        "routes": [
            { "match": { "path": "/a/**" }, "static": "./dist" },
            { "match": { "path": "/b/**" }, "proxy": "http://other:4000" }
        ]
    }"#;

/// Without `proxy` the operator must be told BOTH shapes are inert —
/// pointing at the exact `routes[1]`, not the static-only `routes[0]`.
#[cfg(not(feature = "proxy"))]
#[test]
fn proxy_config_without_feature_generates_warnings() {
    let warnings = feature_warnings(&parse(PROXY_CONFIG));
    let proxy_warnings: Vec<&String> = warnings
        .iter()
        .filter(|w| w.contains("`proxy` feature"))
        .collect();
    assert_eq!(proxy_warnings.len(), 2, "got: {warnings:?}");
    assert!(
        proxy_warnings
            .iter()
            .any(|w| w.starts_with("sites[0].proxy ")),
        "missing the legacy `proxy` warning: {warnings:?}"
    );
    assert!(
        proxy_warnings
            .iter()
            .any(|w| w.starts_with("sites[0].routes[1].proxy ")),
        "must name the routes[] entry that carries the proxy action: {warnings:?}"
    );
}

/// A config with no proxying at all stays silent even without the feature
/// — the warning is about configuration that can't be honoured, not about
/// the build.
#[cfg(not(feature = "proxy"))]
#[test]
fn static_only_config_without_proxy_feature_is_silent() {
    let warnings = feature_warnings(&parse(
        r#"{ "port": 8080, "routes": [{ "match": { "path": "/a/**" }, "static": "./dist" }] }"#,
    ));
    assert!(
        !warnings.iter().any(|w| w.contains("`proxy` feature")),
        "got: {warnings:?}"
    );
}

/// With the feature on, the same config produces no `proxy` warning.
#[cfg(feature = "proxy")]
#[test]
fn proxy_config_with_feature_generates_no_proxy_warning() {
    let warnings = feature_warnings(&parse(PROXY_CONFIG));
    assert!(
        !warnings.iter().any(|w| w.contains("`proxy` feature")),
        "got: {warnings:?}"
    );
}

#[test]
fn duplicate_host_port_detected() {
    let e = errs(r#"[{ "port": 8080 }, { "port": 8080 }]"#);
    assert_eq!(e.len(), 1);
    assert!(e[0].message.contains("Duplicate"), "got: {}", e[0].message);
}

/// Regression test for the #226 fix: `global.workers: 0` used to be
/// silently inert, but now reaches `ServerConf.threads` directly — must
/// be rejected at validate-time rather than let the server start with
/// zero worker threads.
#[test]
fn global_workers_zero_is_rejected() {
    let e = errs(r#"{ "global": { "workers": 0 }, "sites": [{ "port": 8080 }] }"#);
    assert_eq!(e.len(), 1, "got: {e:?}");
    assert_eq!(e[0].path, "global.workers");
}

#[test]
fn global_workers_positive_is_accepted() {
    assert!(errs(r#"{ "global": { "workers": 4 }, "sites": [{ "port": 8080 }] }"#).is_empty());
}

#[test]
fn global_workers_absent_is_accepted() {
    assert!(errs(r#"{ "sites": [{ "port": 8080 }] }"#).is_empty());
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
    let e =
        errs(r#"{ "tls": { "cert": "a.pem", "key": "a.key", "acme": { "email": "a@b.com" } } }"#);
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

// ── tls.versions / tls.ciphers (issue #189: never enforced by Pingora) ─────

#[test]
fn tls_versions_is_rejected() {
    let e = errs(r#"{ "tls": { "cert": "a.pem", "key": "a.key", "versions": ["TLSv1.2"] } }"#);
    assert!(!e.is_empty(), "tls.versions must be rejected: {e:?}");
    assert!(
        e.iter()
            .any(|x| x.path.ends_with(".versions") && x.message.contains("not currently enforced")),
        "got: {e:?}"
    );
    assert!(
        e.iter().all(|x| x.severity == Severity::Error),
        "must be a hard error, not a warning — silently accepting this field lets an \
             operator believe TLS versions are actually restricted: {e:?}"
    );
}

#[test]
fn tls_ciphers_is_rejected() {
    let e = errs(
        r#"{ "tls": { "cert": "a.pem", "key": "a.key", "ciphers": ["TLS13_AES_256_GCM_SHA384"] } }"#,
    );
    assert!(!e.is_empty(), "tls.ciphers must be rejected: {e:?}");
    assert!(
        e.iter()
            .any(|x| x.path.ends_with(".ciphers") && x.message.contains("not currently enforced")),
        "got: {e:?}"
    );
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
    assert!(errs(r#"{ "redirects": [{ "from": "/a", "to": "/b", "status": 301 }] }"#).is_empty());
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

// ── cors ───────────────────────────────────────────────────────────────

#[test]
fn cors_credentials_without_origins_invalid() {
    let e = errs(r#"{ "cors": { "credentials": true } }"#);
    assert!(
        !e.is_empty(),
        "credentials: true with no origins allowlist must be rejected"
    );
    assert!(
        e[0].message.contains("credentials"),
        "got: {}",
        e[0].message
    );
}

#[test]
fn cors_credentials_with_wildcard_origin_invalid() {
    let e = errs(r#"{ "cors": { "credentials": true, "origins": ["*"] } }"#);
    assert!(
        !e.is_empty(),
        "credentials: true with a wildcard origin must be rejected"
    );
}

#[test]
fn cors_credentials_with_empty_origins_invalid() {
    let e = errs(r#"{ "cors": { "credentials": true, "origins": [] } }"#);
    assert!(
        !e.is_empty(),
        "credentials: true with an empty allowlist admits no real origin — still invalid"
    );
}

#[test]
fn cors_credentials_with_explicit_origins_valid() {
    assert!(
        errs(r#"{ "cors": { "credentials": true, "origins": ["https://app.example.com"] } }"#)
            .is_empty()
    );
}

#[test]
fn cors_without_credentials_valid_regardless_of_origins() {
    assert!(errs(r#"{ "cors": true }"#).is_empty());
    assert!(errs(r#"{ "cors": { "origins": ["*"] } }"#).is_empty());
    assert!(errs(r#"{ "cors": {} }"#).is_empty());
}

#[test]
fn cors_disabled_valid() {
    assert!(errs(r#"{ "cors": false }"#).is_empty());
}

#[test]
fn tls_missing_cert() {
    let e = errs(r#"{ "tls": { "key": "a.key" } }"#);
    assert_eq!(e.len(), 1);
    assert!(e[0].message.contains("cert"), "got: {}", e[0].message);
}

#[test]
fn groups_empty_targets_in_group_invalid() {
    let e = errs(
        r#"{
                "proxy": {
                    "/api": {
                        "groups": [
                            { "name": "a", "targets": [] },
                            { "name": "b", "targets": ["http://b1:4000"] }
                        ]
                    }
                }
            }"#,
    );
    assert!(!e.is_empty(), "empty group targets must be rejected");
    assert!(
        e[0].message.contains("target") || e[0].message.contains("group"),
        "got: {}",
        e[0].message
    );
}

#[test]
fn groups_weighted_rr_with_simple_targets_invalid() {
    let e = errs(
        r#"{
                "proxy": {
                    "/api": {
                        "groups": [
                            {
                                "name": "a",
                                "targets": ["http://b1:4000", "http://b2:4000"],
                                "strategy": "weighted-round-robin"
                            }
                        ]
                    }
                }
            }"#,
    );
    assert!(!e.is_empty());
    assert!(e[0].message.contains("weighted"), "got: {}", e[0].message);
}

#[test]
fn groups_no_top_level_targets_valid() {
    assert!(
        errs(
            r#"{
                    "proxy": {
                        "/api": {
                            "groups": [
                                { "name": "a", "targets": ["http://b1:4000"] },
                                { "name": "b", "targets": ["http://b2:4000"] }
                            ]
                        }
                    }
                }"#
        )
        .is_empty(),
        "groups without top-level targets must be valid"
    );
}

#[test]
fn invalid_rewrite_regex_detected() {
    let e = errs(
        r#"{
                "proxy": {
                    "/api": {
                        "targets": ["http://b:4000"],
                        "rewrite": [{ "from": "(unclosed", "to": "/" }]
                    }
                }
            }"#,
    );
    assert!(!e.is_empty(), "invalid regex must be caught");
    assert!(
        e[0].path.contains("rewrite"),
        "error path must reference rewrite field, got: {}",
        e[0].path
    );
}

#[test]
fn valid_rewrite_rules_no_errors() {
    assert!(errs(
        r#"{
                    "proxy": {
                        "/api": {
                            "targets": ["http://b:4000"],
                            "rewrite": [
                                { "from": "^/v[0-9]+/(.+)$", "to": "/$1" }
                            ]
                        }
                    }
                }"#
    )
    .is_empty());
}

#[test]
fn invalid_upstream_url_single_proxy() {
    let e = errs(r#"{ "proxy": "not-a-url" }"#);
    assert!(!e.is_empty(), "non-HTTP URL must be rejected");
    assert!(e[0].message.contains("http://") || e[0].message.contains("https://"));
}

#[test]
fn invalid_upstream_url_in_roundrobin_array() {
    let e = errs(r#"{ "proxy": { "/api": ["http://ok:4000", "ftp://bad:4000"] } }"#);
    assert!(!e.is_empty());
    assert!(e[0].message.contains("ftp://bad"));
}

#[test]
fn valid_upstream_urls_no_errors() {
    assert!(errs(r#"{ "proxy": "http://localhost:4000" }"#).is_empty());
    assert!(errs(r#"{ "proxy": "https://api.example.com" }"#).is_empty());
    assert!(errs(
        r#"{ "proxy": { "/api": { "targets": ["http://b1:4000", "http://b2:4000"] } } }"#
    )
    .is_empty());
}

#[test]
fn ip_filter_invalid_cidr_detected() {
    let e = errs(r#"{ "ipFilter": { "deny": ["999.999.0.0/8", "not-an-ip"] } }"#);
    assert_eq!(e.len(), 2, "both invalid entries must be flagged");
    assert!(e.iter().all(|e| e.path.contains("ipFilter")));
}

#[test]
fn ip_filter_valid_entries_no_errors() {
    assert!(errs(
        r#"{ "ipFilter": { "allow": ["10.0.0.0/8", "192.168.1.1", "::1", "2001:db8::/32"] } }"#
    )
    .is_empty());
}

#[test]
fn ip_filter_prefix_too_large_invalid() {
    let e = errs(r#"{ "ipFilter": { "deny": ["10.0.0.0/33"] } }"#);
    assert!(!e.is_empty(), "/33 is invalid for IPv4");
}

#[test]
fn upload_path_without_leading_slash_invalid() {
    let e = errs(r#"{ "upload": { "path": "upload", "dir": "./uploads" } }"#);
    assert!(
        !e.is_empty(),
        "upload path without leading slash must be rejected"
    );
    assert!(e[0].path.contains("upload.path"), "got: {}", e[0].path);
}

#[test]
fn upload_valid_config_no_errors() {
    assert!(errs(r#"{ "upload": { "path": "/upload", "dir": "./uploads" } }"#).is_empty());
}

#[test]
fn metrics_path_without_leading_slash_invalid() {
    let e = errs(r#"{ "metrics": { "path": "metrics" } }"#);
    assert!(
        !e.is_empty(),
        "metrics path without leading slash must be rejected"
    );
    assert!(e[0].path.contains("metrics.path"), "got: {}", e[0].path);
}

#[test]
fn metrics_valid_path_no_errors() {
    assert!(errs(r#"{ "metrics": { "path": "/__metrics__" } }"#).is_empty());
}

#[test]
fn routes_array_rewrite_regex_validated() {
    let e = errs(
        r#"{
                "routes": [
                    {
                        "match": { "path": "/api/**" },
                        "proxy": {
                            "targets": ["http://b:4000"],
                            "rewrite": [{ "from": "[bad", "to": "/" }]
                        }
                    }
                ]
            }"#,
    );
    assert!(
        !e.is_empty(),
        "invalid regex in routes array must be caught"
    );
}

// ── rateLimit.store validation ────────────────────────────────────────────

#[test]
fn rate_limit_memory_store_valid() {
    assert!(
        errs(r#"{ "rateLimit": { "windowSecs": 60, "limit": 100, "store": "memory" } }"#)
            .is_empty()
    );
}

#[test]
fn rate_limit_redis_store_valid() {
    assert!(errs(
        r#"{ "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://127.0.0.1:6379" } }"#
    )
    .is_empty());
}

// ── check_redis_store_consistency (issue #357) ──────────────────────────

#[cfg(feature = "redis")]
#[test]
fn redis_store_single_url_no_warning() {
    let e = errs(
        r#"{ "port": 8080, "proxy": { "/api": { "targets": ["http://127.0.0.1:9"],
                 "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://a:6379" } } },
                 "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://a:6379" } }"#,
    );
    assert!(
        !e.iter().any(|err| err.path == "rateLimit.store"),
        "identical URLs at every level must not warn: {e:?}"
    );
}

#[cfg(feature = "redis")]
#[test]
fn redis_store_mismatched_site_and_route_urls_warns() {
    let e = errs(
        r#"{ "port": 8080, "proxy": { "/api": { "targets": ["http://127.0.0.1:9"],
                 "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://route-host:6379" } } },
                 "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://site-host:6379" } }"#,
    );
    let warning = e
        .iter()
        .find(|err| err.path == "rateLimit.store")
        .unwrap_or_else(|| panic!("missing mismatch warning: {e:?}"));
    assert_eq!(warning.severity, Severity::Warning);
    assert!(warning.message.contains("redis://site-host:6379"));
    assert!(warning.message.contains("redis://route-host:6379"));
}

#[cfg(feature = "redis")]
#[test]
fn redis_store_mismatched_urls_across_sites_warns() {
    let e = errs(
        r#"[{ "port": 8080, "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://a:6379" } },
                 { "port": 8081, "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://b:6379" } }]"#,
    );
    assert!(
        e.iter().any(|err| err.path == "rateLimit.store"),
        "two sites with different Redis URLs must warn: {e:?}"
    );
}

#[cfg(all(feature = "redis", feature = "consumers"))]
#[test]
fn redis_store_mismatched_consumer_url_warns() {
    let e = errs(
        r#"{ "port": 8080, "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://site-host:6379" },
                 "consumers": { "consumers": [{ "username": "alice",
                   "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://consumer-host:6379" } }] } }"#,
    );
    assert!(
        e.iter().any(|err| err.path == "rateLimit.store"),
        "consumer-level URL differing from site-level must warn: {e:?}"
    );
}

#[cfg(feature = "redis")]
#[test]
fn redis_store_memory_alongside_redis_does_not_count_as_a_second_url() {
    // "memory" is a valid `store` value but not a Redis URL — it must not
    // be treated as a second distinct URL competing with the real one.
    let e = errs(
        r#"{ "port": 8080, "proxy": { "/api": { "targets": ["http://127.0.0.1:9"],
                 "rateLimit": { "windowSecs": 60, "limit": 100, "store": "memory" } } },
                 "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://a:6379" } }"#,
    );
    assert!(
        !e.iter().any(|err| err.path == "rateLimit.store"),
        "a lone Redis URL alongside a memory store must not warn: {e:?}"
    );
}

#[cfg(feature = "redis")]
#[test]
fn redis_store_mismatch_warning_redacts_credentials() {
    // Regression for the security-engineer HOLD on PR #359: the mismatch
    // warning interpolates the raw configured URLs into a message that
    // reaches tracing::warn! verbatim (via load_and_validate/file_provider/
    // the /reload handler) -- a credential-bearing URL must never survive
    // into that log line unredacted.
    let e = errs(
        r#"{ "port": 8080, "proxy": { "/api": { "targets": ["http://127.0.0.1:9"],
                 "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://route-host:6379" } } },
                 "rateLimit": { "windowSecs": 60, "limit": 100,
                   "store": "redis://alice:s3cret@site-host:6379" } }"#,
    );
    let warning = e
        .iter()
        .find(|err| err.path == "rateLimit.store")
        .unwrap_or_else(|| panic!("missing mismatch warning: {e:?}"));
    assert!(
        !warning.message.contains("s3cret") && !warning.message.contains("alice"),
        "credentials must not leak into the warning message: {}",
        warning.message
    );
    assert!(
        warning.message.contains("redis://***@site-host:6379"),
        "redacted URL must still be identifiable: {}",
        warning.message
    );
}

#[cfg(feature = "redis")]
#[test]
fn redis_store_mismatch_warning_escapes_embedded_control_chars() {
    // CodeRabbit finding on PR #359: validate_rate_limit only checks the
    // redis://\rediss:// prefix on `store`, not for embedded control
    // characters -- a raw newline could otherwise forge a fake log line
    // in this warning's tracing::warn! output. sanitize_for_log must
    // escape it before it reaches the message.
    let e = errs(
        r#"{ "port": 8080, "proxy": { "/api": { "targets": ["http://127.0.0.1:9"],
                 "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://route-host:6379" } } },
                 "rateLimit": { "windowSecs": 60, "limit": 100,
                   "store": "redis://a\nfake log line: [ERROR] pwned:6379" } }"#,
    );
    let warning = e
        .iter()
        .find(|err| err.path == "rateLimit.store")
        .unwrap_or_else(|| panic!("missing mismatch warning: {e:?}"));
    assert!(
        !warning.message.contains('\n'),
        "an embedded newline must not survive into the warning message: {}",
        warning.message
    );
    assert!(
        warning.message.contains("\\nfake log line"),
        "the newline must be visibly escaped, not silently dropped: {}",
        warning.message
    );
}

#[test]
fn rate_limit_token_bucket_algorithm_valid() {
    assert!(errs(
        r#"{ "rateLimit": { "windowSecs": 60, "limit": 100, "algorithm": "token-bucket" } }"#
    )
    .is_empty());
}

#[test]
fn rate_limit_unknown_algorithm_rejected() {
    let e =
        errs(r#"{ "rateLimit": { "windowSecs": 60, "limit": 100, "algorithm": "leaky-bucket" } }"#);
    assert!(!e.is_empty(), "unknown algorithm must be rejected");
    assert!(e[0].path.contains("algorithm"), "got: {}", e[0].path);
}

#[test]
fn rate_limit_key_by_ip_valid() {
    assert!(
        errs(r#"{ "rateLimit": { "windowSecs": 60, "limit": 100, "keyBy": "ip" } }"#).is_empty()
    );
}

#[test]
fn rate_limit_key_by_valid_header_name_valid() {
    assert!(errs(
        r#"{ "rateLimit": { "windowSecs": 60, "limit": 100, "keyBy": "header:X-User-ID" } }"#
    )
    .is_empty());
}

#[test]
fn rate_limit_key_by_header_with_space_rejected() {
    let e =
        errs(r#"{ "rateLimit": { "windowSecs": 60, "limit": 100, "keyBy": "header:bad name" } }"#);
    assert!(!e.is_empty(), "a header name with a space must be rejected");
    assert!(e[0].path.contains("keyBy"), "got: {}", e[0].path);
}

#[test]
fn rate_limit_key_by_bare_header_rejected() {
    // "header" with no ":<name>" suffix was previously schema-valid but
    // silently fell through to IP-based limiting at runtime — now rejected
    // outright (Gitar finding on PR #302's review).
    let e = errs(r#"{ "rateLimit": { "windowSecs": 60, "limit": 100, "keyBy": "header" } }"#);
    assert!(
        !e.is_empty(),
        "bare \"header\" without a name must be rejected"
    );
    assert!(e[0].path.contains("keyBy"), "got: {}", e[0].path);
}

#[test]
fn rate_limit_key_by_unknown_value_rejected() {
    let e = errs(r#"{ "rateLimit": { "windowSecs": 60, "limit": 100, "keyBy": "cookie" } }"#);
    assert!(
        !e.is_empty(),
        "an unrecognized keyBy value must be rejected"
    );
    assert!(e[0].path.contains("keyBy"), "got: {}", e[0].path);
}

#[test]
fn rate_limit_invalid_store_rejected() {
    let e = errs(
        r#"{ "rateLimit": { "windowSecs": 60, "limit": 100, "store": "memcached://localhost" } }"#,
    );
    assert!(!e.is_empty(), "invalid store must be rejected");
    assert!(e[0].path.contains("store"), "got: {}", e[0].path);
}

// ── proxy cache store validation ─────────────────────────────────────────

fn proxy_with_cache(store: &str) -> Vec<ValidationError> {
    errs(&format!(
        r#"{{ "proxy": {{ "/api": {{ "targets": ["http://b:4000"], "cache": {{ "store": "{store}", "ttlSecs": 60 }} }} }} }}"#
    ))
}

#[test]
fn cache_store_memory_valid() {
    assert!(proxy_with_cache("memory").is_empty());
}

#[test]
fn cache_store_redis_url_valid() {
    assert!(proxy_with_cache("redis://localhost:6379").is_empty());
}

#[test]
fn cache_store_rediss_tls_valid() {
    assert!(proxy_with_cache("rediss://redis.example.com:6380").is_empty());
}

#[test]
fn cache_store_disk_valid() {
    assert!(proxy_with_cache("disk:/var/cache/conduit").is_empty());
}

#[test]
fn cache_store_invalid_rejected() {
    let e = proxy_with_cache("memcached://localhost");
    assert!(!e.is_empty(), "invalid cache store must be rejected");
    assert!(e[0].path.contains("store"), "got: {}", e[0].path);
}

// ── TCP proxy validation ──────────────────────────────────────────────────

#[test]
fn tcp_proxy_valid() {
    let e = errs(r#"{ "port": 3306, "tcp": { "targets": ["mysql:3306"] } }"#);
    assert!(e.is_empty(), "valid TCP site must pass: {e:?}");
}

#[test]
fn tcp_proxy_no_targets_rejected() {
    let e = errs(r#"{ "port": 3306, "tcp": { "targets": [] } }"#);
    assert!(!e.is_empty());
    assert!(e[0].path.contains("targets"), "got: {}", e[0].path);
}

#[test]
fn tcp_proxy_http_prefix_rejected() {
    let e = errs(r#"{ "port": 3306, "tcp": { "targets": ["http://mysql:3306"] } }"#);
    assert!(
        !e.is_empty(),
        "http:// prefix must be rejected for TCP targets"
    );
}

#[test]
fn tcp_proxy_missing_port_rejected() {
    let e = errs(r#"{ "port": 3306, "tcp": { "targets": ["just-a-host"] } }"#);
    assert!(!e.is_empty(), "target without port must be rejected");
}

#[test]
fn tcp_proxy_combined_with_proxy_rejected() {
    let e =
        errs(r#"{ "port": 3306, "tcp": { "targets": ["mysql:3306"] }, "proxy": "http://b:4000" }"#);
    assert!(!e.is_empty(), "tcp + proxy must be rejected");
}

// ── middleware validation ─────────────────────────────────────────────────

#[test]
fn middleware_script_without_path_is_invalid() {
    let e = errs(r#"{ "middleware": [{ "type": "script" }] }"#);
    assert!(!e.is_empty(), "script entry without path must be rejected");
    assert!(
        e[0].message.contains("path"),
        "error must mention missing path, got: {}",
        e[0].message
    );
}

#[test]
fn middleware_script_with_path_is_valid() {
    assert!(errs(r#"{ "middleware": [{ "type": "script", "path": "./my.rhai" }] }"#).is_empty());
}

#[test]
fn middleware_builtin_type_is_valid() {
    assert!(errs(r#"{ "middleware": [{ "type": "ipFilter" }] }"#).is_empty());
    assert!(errs(r#"{ "middleware": [{ "type": "rateLimit" }] }"#).is_empty());
    assert!(errs(r#"{ "middleware": [{ "type": "auth" }] }"#).is_empty());
    assert!(errs(r#"{ "middleware": [{ "type": "headers" }] }"#).is_empty());
}

#[test]
fn middleware_unknown_type_is_invalid() {
    let e = errs(r#"{ "middleware": [{ "type": "magic" }] }"#);
    assert!(!e.is_empty(), "unknown middleware type must be rejected");
    assert!(
        e[0].message.contains("unknown middleware type"),
        "got: {}",
        e[0].message
    );
}

#[test]
fn middleware_mixed_entries_validated() {
    // Script with path + script without path: one error expected.
    let e = errs(
        r#"{ "middleware": [
                { "type": "script", "path": "./ok.rhai" },
                { "type": "script" }
            ] }"#,
    );
    assert_eq!(e.len(), 1, "exactly one script entry is missing path");
}

#[test]
fn middleware_phase_typo_is_invalid() {
    let e = errs(
        r#"{ "middleware": [
                { "type": "script", "path": "./ok.rhai", "phase": "resposne" }
            ] }"#,
    );
    assert!(!e.is_empty(), "misspelled phase must be rejected");
    assert!(
        e.iter().any(|err| err.path.contains("phase")),
        "error path must mention phase: {:?}",
        e
    );
}

#[test]
fn middleware_phase_request_and_response_are_valid() {
    assert!(errs(
        r#"{ "middleware": [{ "type": "script", "path": "./ok.rhai", "phase": "request" }] }"#
    )
    .is_empty());
    assert!(errs(
        r#"{ "middleware": [{ "type": "script", "path": "./ok.rhai", "phase": "response" }] }"#
    )
    .is_empty());
}

#[test]
fn middleware_phase_absent_is_valid() {
    assert!(errs(r#"{ "middleware": [{ "type": "script", "path": "./ok.rhai" }] }"#).is_empty());
}

#[test]
fn rate_limit_limit_zero_returns_error() {
    let e = errs(r#"{ "rateLimit": { "windowSecs": 60, "limit": 0 } }"#);
    assert!(!e.is_empty(), "limit 0 must be rejected");
    assert!(
        e.iter().any(|err| err.path.contains("limit")),
        "error path must mention limit: {:?}",
        e
    );
}

#[test]
fn upload_empty_dir_invalid() {
    let e = errs(r#"{ "upload": { "path": "/upload", "dir": "" } }"#);
    assert!(!e.is_empty(), "empty upload dir must be rejected");
    assert!(
        e.iter().any(|err| err.path.contains("upload")),
        "error path must mention upload: {:?}",
        e
    );
}

#[test]
fn cidr_with_non_numeric_mask_is_invalid() {
    // "10.0.0.0/abc" — the prefix length is not a valid u32.
    let e = errs(r#"{ "ipFilter": { "deny": ["10.0.0.0/abc"] } }"#);
    assert!(!e.is_empty(), "CIDR with non-numeric mask must be rejected");
    assert!(
        e.iter().any(|err| err.path.contains("ipFilter")),
        "error path must mention ipFilter: {:?}",
        e
    );
}

// ── TLS cert expiry ───────────────────────────────────────────────────────

/// Helper: write `content` to a temp file and return the (dir, path_string).
fn write_temp_file(content: &[u8], name: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, content).unwrap();
    let path_str = path.to_str().unwrap().replace('\\', "/");
    (dir, path_str)
}

#[test]
fn cert_expiry_invalid_pem_silently_ignored() {
    // File exists but contains garbage — check_cert_expiry should return without error.
    let (_dir, cert_path) = write_temp_file(b"this is not valid PEM content", "bad.pem");
    let json = format!(r#"{{"tls": {{"cert": "{cert_path}", "key": "k.key"}}}}"#);
    let e = errs(&json);
    // The only possible error is "missing key" or none. No cert-expiry error.
    assert!(
        e.iter()
            .all(|err| !err.message.contains("expired") && !err.message.contains("WARNING")),
        "invalid PEM must not produce cert-expiry errors: {:?}",
        e
    );
}

#[test]
fn cert_expiry_expired_cert_returns_error() {
    // Generate a cert with not_after in the past (expired ~5 years ago).
    let mut params = rcgen::CertificateParams::new(vec!["example.com".to_string()]).unwrap();
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2020, 12, 31);
    let key_pair = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key_pair).unwrap();
    let pem = cert.pem();

    let (_dir, cert_path) = write_temp_file(pem.as_bytes(), "expired.pem");
    let json = format!(r#"{{"tls": {{"cert": "{cert_path}", "key": "k.key"}}}}"#);
    let e = errs(&json);
    assert!(
        e.iter().any(|err| err.message.contains("expired")),
        "expired cert must produce an expiry error: {:?}",
        e
    );
    // Regression test for #191: an actually-expired cert must stay a
    // hard error (Severity::Error) — only the "still valid, but soon"
    // case (see cert_expiry_soon_returns_warning below) becomes advisory.
    assert!(
        e.iter()
            .any(|err| err.message.contains("expired") && err.severity == Severity::Error),
        "expired cert must be Severity::Error, not just Severity::Warning: {:?}",
        e
    );
}

#[test]
fn cert_expiry_soon_returns_warning() {
    // Generate a cert expiring 15 days from now — within the 30-day warning window.
    use time::{Duration, OffsetDateTime};
    let soon = OffsetDateTime::now_utc() + Duration::days(15);
    let mut params = rcgen::CertificateParams::new(vec!["example.com".to_string()]).unwrap();
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = soon;
    let key_pair = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key_pair).unwrap();
    let pem = cert.pem();

    let (_dir, cert_path) = write_temp_file(pem.as_bytes(), "soon.pem");
    let json = format!(r#"{{"tls": {{"cert": "{cert_path}", "key": "k.key"}}}}"#);
    let e = errs(&json);
    assert!(
        e.iter().any(|err| err.message.contains("WARNING")),
        "soon-expiring cert must produce a WARNING: {:?}",
        e
    );
    // Regression test for #191: a still-valid-but-soon-expiring cert must
    // be Severity::Warning (advisory — must not block startup/reload),
    // not Severity::Error (which would hard-fail the server).
    assert!(
        e.iter()
            .any(|err| err.message.contains("WARNING") && err.severity == Severity::Warning),
        "soon-expiring cert must be Severity::Warning, not Severity::Error: {:?}",
        e
    );
}

#[test]
fn partition_by_severity_splits_warnings_from_hard_errors() {
    let errors = vec![
        ValidationError::new("a", "hard 1"),
        ValidationError::warning("b", "soft 1"),
        ValidationError::new("c", "hard 2"),
        ValidationError::warning("d", "soft 2"),
    ];
    let (warnings, hard_errors) = partition_by_severity(errors);
    assert_eq!(warnings.len(), 2, "got: {warnings:?}");
    assert!(warnings.iter().all(|w| w.severity == Severity::Warning));
    assert_eq!(hard_errors.len(), 2, "got: {hard_errors:?}");
    assert!(hard_errors.iter().all(|e| e.severity == Severity::Error));
}

#[test]
fn partition_by_severity_all_warnings_yields_no_hard_errors() {
    let errors = vec![
        ValidationError::warning("a", "soft 1"),
        ValidationError::warning("b", "soft 2"),
    ];
    let (warnings, hard_errors) = partition_by_severity(errors);
    assert_eq!(warnings.len(), 2);
    assert!(
        hard_errors.is_empty(),
        "an all-warnings input must never produce a hard error: {hard_errors:?}"
    );
}

#[test]
fn proxy_route_url_variant_invalid_scheme_rejected() {
    // `{ "/api": "ftp://bad" }` parses as ProxyRouteTarget::Url("ftp://bad").
    // Exercises the Url arm of `proxy::validate_proxy_route_target`.
    let e = errs(r#"{ "proxy": { "/api": "ftp://bad" } }"#);
    assert!(!e.is_empty(), "non-HTTP proxy URL must be rejected");
    assert!(
        e.iter()
            .any(|err| err.message.contains("Invalid upstream URL")),
        "error must mention invalid URL: {:?}",
        e
    );
}

#[test]
fn proxy_route_full_invalid_target_url_rejected() {
    // Full form with an ftp:// target URL exercises `proxy::validate_target_urls`,
    // which `proxy::validate_route_config` calls.
    let e = errs(r#"{ "proxy": { "/api": { "targets": ["ftp://bad:4000"] } } }"#);
    assert!(!e.is_empty(), "non-HTTP target URL must be rejected");
    assert!(
        e.iter()
            .any(|err| err.message.contains("Invalid upstream URL")),
        "error must mention invalid URL: {:?}",
        e
    );
}

// ── feature_warnings ─────────────────────────────────────────────────────

fn warns(json: &str) -> Vec<String> {
    feature_warnings(&parse(json))
}

#[test]
fn no_warnings_for_plain_config() {
    // A config with no feature-gated options produces no warnings. `proxy`
    // is itself feature-gated now (#144), so it is not part of the "plain"
    // config — see the `cfg(proxy)` sibling below for the old coverage.
    assert!(warns(r#"{ "port": 8080 }"#).is_empty());
}

#[test]
#[cfg(feature = "proxy")]
fn no_warnings_for_a_proxy_config_when_proxy_is_compiled_in() {
    assert!(warns(r#"{ "port": 8080, "proxy": "http://up:4000" }"#).is_empty());
}

#[test]
#[cfg(feature = "wasm")]
fn no_warning_when_wasm_feature_enabled() {
    // When the wasm feature IS compiled in, no warning is emitted.
    let w = warns(
        r#"{ "port": 8080,
                 "middleware": [{ "type": "wasm", "path": "p.wasm" }] }"#,
    );
    assert!(w.is_empty(), "wasm feature active → no warning: {w:?}");
}

#[test]
#[cfg(not(feature = "wasm"))]
fn warning_for_wasm_middleware_without_feature() {
    let w = warns(
        r#"{ "port": 8080,
                 "middleware": [{ "type": "wasm", "path": "plugin.wasm" }] }"#,
    );
    assert_eq!(w.len(), 1, "expected exactly one warning: {w:?}");
    assert!(
        w[0].contains("wasm"),
        "warning must mention 'wasm': {}",
        w[0]
    );
    assert!(
        w[0].contains("--features wasm"),
        "warning must mention compile flag: {}",
        w[0]
    );
}

// ── extra key warnings (#124) ────────────────────────────────────────────

#[test]
fn unrecognized_key_is_captured_in_extra_not_an_error() {
    // Today, since every field is always present, an unrecognized key
    // must parse successfully (captured in `extra`) rather than erroring.
    let cfg = parse(r#"{ "port": 8080, "totallyMadeUpKey": true }"#);
    assert_eq!(
        cfg.sites[0].extra.get("totallyMadeUpKey"),
        Some(&serde_json::json!(true))
    );
}

#[test]
fn unrecognized_key_produces_typo_warning() {
    let w = warns(r#"{ "port": 8080, "jwtAuht": { "secret": "x" } }"#);
    assert_eq!(w.len(), 1, "expected exactly one warning: {w:?}");
    assert!(
        w[0].contains("jwtAuht") && w[0].contains("typo"),
        "warning must name the key and mention a typo: {}",
        w[0]
    );
}

#[test]
fn known_field_key_never_lands_in_extra() {
    // Well-known keys always deserialize into their named field today —
    // this is the "ahead of need" property the whole mechanism relies on.
    let cfg = parse(r#"{ "port": 8080, "jwtAuth": { "secret": "x" } }"#);
    assert!(
        cfg.sites[0].extra.is_empty(),
        "known key must not land in extra: {:?}",
        cfg.sites[0].extra
    );
}

#[test]
fn disabled_key_owning_feature_table_produces_recompile_warning() {
    // Simulates the future state (once #114 gates fields per-crate) by
    // manually placing a known disabled-feature key into `extra` —
    // proving the table lookup + warning wording work correctly now,
    // ahead of any field actually being removed from the struct.
    let mut config = parse(r#"{ "port": 8080 }"#);
    config.sites[0]
        .extra
        .insert("jwtAuth".to_string(), serde_json::json!({}));
    let w = feature_warnings(&config);
    assert_eq!(w.len(), 1, "expected exactly one warning: {w:?}");
    assert!(
        w[0].contains("jwtAuth") && w[0].contains("--features jwt"),
        "warning must name the key and the owning feature: {}",
        w[0]
    );
    assert!(
        !w[0].contains("typo"),
        "a known disabled-feature key must not be reported as a typo: {}",
        w[0]
    );
}

#[test]
#[cfg(not(feature = "wasm"))]
fn warning_per_wasm_entry() {
    // Two wasm + one script entry → warnings depend on which features are off.
    let w = warns(
        r#"{ "port": 8080,
                 "middleware": [
                     { "type": "wasm", "path": "a.wasm" },
                     { "type": "script", "path": "b.rhai" },
                     { "type": "wasm", "path": "c.wasm" }
                 ] }"#,
    );
    // At least 2 warnings for the two WASM entries.
    assert!(w.len() >= 2, "at least two wasm warnings expected: {w:?}");
    let wasm_warns = w.iter().filter(|m| m.contains("wasm")).count();
    assert_eq!(wasm_warns, 2, "exactly two wasm warnings: {w:?}");
}

#[test]
#[cfg(not(feature = "otlp"))]
fn warning_for_otlp_without_feature() {
    let w = warns(
        r#"{ "global": { "otlp": { "endpoint": "http://otel:4317" } },
                 "sites": [{ "port": 8080 }] }"#,
    );
    assert_eq!(w.len(), 1, "expected exactly one otlp warning: {w:?}");
    assert!(
        w[0].contains("otlp"),
        "warning must mention 'otlp': {}",
        w[0]
    );
    assert!(
        w[0].contains("--features otlp"),
        "warning must mention compile flag: {}",
        w[0]
    );
}

#[test]
#[cfg(feature = "otlp")]
fn no_warning_when_otlp_feature_enabled() {
    let w = warns(
        r#"{ "global": { "otlp": { "endpoint": "http://otel:4317" } },
                 "sites": [{ "port": 8080 }] }"#,
    );
    assert!(w.is_empty(), "otlp feature active → no warning: {w:?}");
}

// ── proxy loop detection ──────────────────────────────────────────────────

#[cfg(feature = "proxy")]
#[test]
fn proxy_loop_on_own_port_warns() {
    // Port 8080 listens AND is the proxy target → loop.
    let w = warns(r#"{ "port": 8080, "proxy": "http://127.0.0.1:8080" }"#);
    assert!(!w.is_empty(), "self-referencing target must warn: {w:?}");
    assert!(
        w.iter().any(|m| m.contains("loop")),
        "warning must mention loop: {w:?}"
    );
}

#[cfg(feature = "proxy")]
#[test]
fn proxy_to_different_port_no_warn() {
    let w = warns(r#"{ "port": 8080, "proxy": "http://127.0.0.1:4000" }"#);
    assert!(
        w.iter().all(|m| !m.contains("loop")),
        "proxy to different port must not warn about loop: {w:?}"
    );
}

#[cfg(feature = "proxy")]
#[test]
fn proxy_to_external_host_no_warn() {
    let w = warns(r#"{ "port": 8080, "proxy": "http://api.example.com:8080" }"#);
    assert!(
        w.iter().all(|m| !m.contains("loop")),
        "proxy to external host must not warn: {w:?}"
    );
}

// ── weak JWT secret ───────────────────────────────────────────────────────

#[test]
#[cfg(feature = "jwt")] // secret-length warning only fires when jwt feature is enabled
fn short_jwt_secret_warns() {
    let w = warns(r#"{ "port": 8080, "jwtAuth": { "secret": "short" } }"#);
    assert!(!w.is_empty(), "short secret must warn");
    let has_32_bytes_warn = w.iter().any(|m| m.contains("32 bytes"));
    assert!(has_32_bytes_warn, "warning must mention 32 bytes: {w:?}");
}

#[test]
#[cfg(not(feature = "jwt"))] // when jwt feature is off, generates a different warning
fn jwt_without_feature_warns() {
    let w = warns(r#"{ "port": 8080, "jwtAuth": { "secret": "short" } }"#);
    assert!(!w.is_empty(), "jwtAuth without jwt feature must warn");
    assert!(
        w.iter().any(|m| m.contains("jwt")),
        "warning must mention jwt: {w:?}"
    );
}

#[test]
fn adequate_jwt_secret_no_warn() {
    let secret = "a".repeat(32);
    let w = warns(&format!(
        r#"{{ "port": 8080, "jwtAuth": {{ "secret": "{secret}" }} }}"#
    ));
    assert!(
        w.iter().all(|m| !m.contains("secret")),
        "32-byte secret must not warn: {w:?}"
    );
}

// ── metrics without auth ─────────────────────────────────────────────────

#[test]
fn metrics_without_token_warns() {
    let w = warns(r#"{ "port": 8080, "metrics": {} }"#);
    assert!(
        w.iter().any(|m| m.contains("metrics")),
        "metrics without token must warn: {w:?}"
    );
}

#[test]
fn metrics_with_token_no_warn() {
    let w = warns(r#"{ "port": 8080, "metrics": { "token": "secret" } }"#);
    assert!(
        w.iter().all(|m| !m.contains("publicly")),
        "metrics with token must not warn about access: {w:?}"
    );
}

#[test]
fn metrics_empty_string_token_is_error() {
    // `token: ""` is not the same as omitting it — the handler's
    // constant-time comparison treats an empty configured token as
    // matching a request with no Authorization header at all,
    // authenticating everyone. Must be a hard validation error, not
    // silently normalized to None (which would pick the unauthenticated
    // path the operator likely didn't intend).
    let e = errs(r#"{ "port": 8080, "metrics": { "token": "" } }"#);
    assert!(
        e.iter().any(|err| err.path.contains("metrics.token")),
        "empty metrics.token must be a validation error: {e:?}"
    );
}

// ── forwardAuth SSRF to admin API ─────────────────────────────────────────

#[cfg(feature = "forward-auth")]
#[test]
fn forward_auth_to_admin_api_port_is_error() {
    let e = errs(r#"{ "port": 8080, "forwardAuth": { "url": "http://127.0.0.1:2019/auth" } }"#);
    assert!(
        e.iter().any(|err| err.message.contains("Admin API")),
        "forwardAuth pointing to admin port must be an error: {e:?}"
    );
}

/// Without `forward-auth` the whole `forwardAuth` block is ignored (and
/// `feature_warnings()` says so), so the Admin-API-target rule has nothing
/// to guard: it is scoped to builds that enforce forwardAuth, together with
/// the `url` parsing it needs. Pinned so the scoping cannot drift silently.
#[cfg(not(feature = "forward-auth"))]
#[test]
fn forward_auth_to_admin_api_port_is_not_an_error_without_the_feature() {
    let e = errs(r#"{ "port": 8080, "forwardAuth": { "url": "http://127.0.0.1:2019/auth" } }"#);
    assert!(
        e.iter().all(|err| !err.message.contains("Admin API")),
        "the Admin-API rule is scoped to builds that enforce forwardAuth: {e:?}"
    );
}

#[test]
fn forward_auth_to_normal_service_ok() {
    assert!(
        errs(r#"{ "port": 8080, "forwardAuth": { "url": "http://auth-service:4000/verify" } }"#)
            .iter()
            .all(|e| !e.message.contains("Admin API")),
        "forwardAuth to external service must not warn about admin API"
    );
}

// ── empty API key ─────────────────────────────────────────────────────────

#[test]
fn empty_api_key_is_rejected() {
    // An empty key creates a bypass: clients without the header have provided=""
    // which matches the empty key.
    let e = errs(r#"{ "port": 8080, "apiKey": { "keys": [""] } }"#);
    assert!(!e.is_empty(), "empty API key must be rejected");
    assert!(
        e.iter().any(|err| err.message.contains("empty")),
        "error must mention empty key: {e:?}"
    );
}

#[test]
fn non_empty_api_key_is_valid() {
    assert!(
        errs(r#"{ "port": 8080, "apiKey": { "keys": ["secret-key-123"] } }"#).is_empty(),
        "non-empty API key must pass validation"
    );
}

#[test]
fn empty_keys_list_is_rejected() {
    let e = errs(r#"{ "port": 8080, "apiKey": { "keys": [] } }"#);
    assert!(!e.is_empty(), "empty keys list must be rejected");
}

#[test]
fn consumer_empty_api_key_is_rejected() {
    let e = errs(
        r#"{
            "port": 8080,
            "consumers": { "consumers": [{ "username": "bob", "apiKey": "" }] }
        }"#,
    );
    assert!(
        e.iter().any(|err| err.message.contains("empty")),
        "consumer empty apiKey must be rejected: {e:?}"
    );
}

// ── loopback_port helper ─────────────────────────────────────────────────

#[cfg(feature = "proxy")]
#[test]
fn loopback_localhost_explicit_port_detected() {
    // localhost with an explicit port should trigger loop detection.
    let w = warns(r#"{ "port": 9000, "proxy": "http://localhost:9000" }"#);
    assert!(
        w.iter().any(|m| m.contains("loop")),
        "localhost loop must warn: {w:?}"
    );
}

#[cfg(feature = "proxy")]
#[test]
fn loopback_localhost_default_http_port() {
    // localhost without explicit port → defaults to 80 for http.
    let w = warns(r#"{ "port": 80, "proxy": "http://localhost" }"#);
    assert!(
        w.iter().any(|m| m.contains("loop")),
        "localhost:80 loop must warn: {w:?}"
    );
}

#[cfg(feature = "proxy")]
#[test]
fn loopback_localhost_https_default_port() {
    // https://localhost without port → defaults to 443.
    let w = warns(
        r#"{ "port": 443, "tls": { "cert": "c.pem", "key": "c.key" },
                 "proxy": "https://localhost" }"#,
    );
    assert!(
        w.iter().any(|m| m.contains("loop")),
        "https://localhost:443 loop must warn: {w:?}"
    );
}

#[cfg(feature = "proxy")]
#[test]
fn loopback_127_x_subnet_detected() {
    // 127.0.0.2 is still loopback.
    let w = warns(r#"{ "port": 8080, "proxy": "http://127.0.0.2:8080" }"#);
    assert!(
        w.iter().any(|m| m.contains("loop")),
        "127.0.0.2 loop must warn: {w:?}"
    );
}

#[cfg(feature = "proxy")]
#[test]
fn non_loopback_host_no_loop_warn() {
    let w = warns(r#"{ "port": 8080, "proxy": "http://10.0.0.1:8080" }"#);
    assert!(
        w.iter().all(|m| !m.contains("loop")),
        "non-loopback must not warn: {w:?}"
    );
}

// ── is_valid_ip_or_cidr ──────────────────────────────────────────────────

#[test]
fn ipv6_cidr_valid_max_prefix() {
    // /128 is the maximum valid prefix for IPv6.
    assert!(
        errs(r#"{ "ipFilter": { "deny": ["::1/128"] } }"#).is_empty(),
        "::1/128 must be valid"
    );
}

#[test]
fn ipv6_cidr_too_large_prefix_invalid() {
    let e = errs(r#"{ "ipFilter": { "deny": ["::1/129"] } }"#);
    assert!(!e.is_empty(), "/129 is invalid for IPv6");
}

#[test]
fn ipv4_solo_address_valid() {
    assert!(
        errs(r#"{ "ipFilter": { "allow": ["192.168.1.100"] } }"#).is_empty(),
        "plain IPv4 address must be valid"
    );
}

#[test]
fn ipv6_solo_address_valid() {
    assert!(
        errs(r#"{ "ipFilter": { "allow": ["2001:db8::1"] } }"#).is_empty(),
        "plain IPv6 address must be valid"
    );
}

// ── feature_warnings for various features ────────────────────────────────

#[test]
#[cfg(not(feature = "rhai"))]
fn warning_for_rhai_middleware_without_feature() {
    let w = warns(
        r#"{ "port": 8080,
                 "middleware": [{ "type": "script", "path": "./filter.rhai" }] }"#,
    );
    assert!(
        w.iter().any(|m| m.contains("rhai")),
        "missing rhai feature must warn: {w:?}"
    );
}

#[test]
#[cfg(not(feature = "forward-auth"))]
fn warning_for_forward_auth_without_feature() {
    let w = warns(
        r#"{ "port": 8080,
                 "forwardAuth": { "url": "http://auth:4000/verify" } }"#,
    );
    assert!(
        w.iter().any(|m| m.contains("forward-auth")),
        "missing forward-auth feature must warn: {w:?}"
    );
}

#[test]
#[cfg(not(feature = "tcp"))]
fn warning_for_tcp_without_feature() {
    let w = warns(r#"{ "port": 3306, "tcp": { "targets": ["mysql:3306"] } }"#);
    assert!(
        w.iter().any(|m| m.contains("tcp")),
        "missing tcp feature must warn: {w:?}"
    );
}

#[test]
#[cfg(not(feature = "fault-injection"))]
fn warning_for_fault_injection_without_feature() {
    let w = warns(
        r#"{ "port": 8080,
                 "faultInjection": { "abort": { "percent": 50, "status": 503 } } }"#,
    );
    assert!(
        w.iter().any(|m| m.contains("fault-injection")),
        "missing fault-injection feature must warn: {w:?}"
    );
}

// `compression` is default-on (issue #114/#138) — this test only
// compiles/runs for a deliberate `--no-default-features` (or otherwise
// `compression`-excluding) build, same as the other `#[cfg(not(feature
// = "..."))]`-gated warning tests above/below. It never runs under this
// repo's normal `cargo test` / `cargo test --features full` CI jobs,
// both of which keep `compression` on via `default` — see the
// `compression` feature's own `Cargo.toml` comment.
#[test]
#[cfg(not(feature = "compression"))]
fn warning_for_compression_without_feature() {
    let w = warns(r#"{ "port": 8080, "compression": true }"#);
    assert!(
        w.iter().any(|m| m.contains("compression")),
        "missing compression feature must warn: {w:?}"
    );
}

// ── TCP port conflict with HTTP sites ────────────────────────────────────

#[test]
#[cfg(feature = "tcp")]
fn tcp_port_conflicts_with_http_site() {
    let e = errs(
        r#"[
                { "port": 8080 },
                { "port": 8080, "tcp": { "targets": ["db:5432"] } }
            ]"#,
    );
    assert!(
        !e.is_empty(),
        "TCP site on same port as HTTP site must error: {e:?}"
    );
}

// ── validate_rate_limit edge cases ───────────────────────────────────────

#[test]
fn rate_limit_zero_limit_is_rejected() {
    let e = errs(r#"{ "rateLimit": { "limit": 0, "windowSecs": 60 } }"#);
    assert!(!e.is_empty(), "limit=0 must be rejected");
    assert!(
        e.iter().any(|err| err.path.contains("rateLimit")),
        "error must point to rateLimit: {e:?}"
    );
}

#[test]
fn rate_limit_zero_window_secs_is_rejected() {
    let e = errs(r#"{ "rateLimit": { "limit": 100, "windowSecs": 0 } }"#);
    assert!(!e.is_empty(), "windowSecs=0 must be rejected");
}

#[test]
fn rate_limit_valid_config_no_errors() {
    assert!(
        errs(r#"{ "rateLimit": { "limit": 100, "windowSecs": 60 } }"#).is_empty(),
        "valid rate limit config must pass"
    );
}

// ── validate_consumers ────────────────────────────────────────────────────

#[test]
fn consumer_empty_username_is_rejected() {
    let e = errs(
        r#"{ "port": 8080,
                 "consumers": { "consumers": [{ "username": "", "apiKey": "key" }] } }"#,
    );
    assert!(
        e.iter().any(|err| err.message.contains("username")),
        "empty username must be rejected: {e:?}"
    );
}

#[test]
fn consumer_duplicate_usernames_rejected() {
    let e = errs(
        r#"{ "port": 8080,
                 "consumers": { "consumers": [
                     { "username": "alice", "apiKey": "key1" },
                     { "username": "alice", "apiKey": "key2" }
                 ] } }"#,
    );
    assert!(
        e.iter().any(|err| err.message.contains("duplicated")),
        "duplicate username must be rejected: {e:?}"
    );
}

#[test]
fn consumer_without_credentials_is_rejected() {
    // No apiKey, no basicAuth, no jwt → missing credentials.
    let e = errs(
        r#"{ "port": 8080,
                 "consumers": { "consumers": [{ "username": "nobody" }] } }"#,
    );
    assert!(
        !e.is_empty(),
        "consumer without any credentials must be rejected"
    );
}

#[test]
fn consumer_empty_basic_auth_password_rejected() {
    let e = errs(
        r#"{ "port": 8080,
                 "consumers": { "consumers": [
                     { "username": "alice", "basicAuth": { "password": "" } }
                 ] } }"#,
    );
    assert!(
        e.iter().any(|err| err.path.contains("basicAuth")),
        "empty basicAuth password must be rejected: {e:?}"
    );
}

#[test]
fn consumer_zero_rate_limit_is_rejected() {
    // limit=0/windowSecs=0 would silently lock this consumer out of every
    // request (TokenBucket::new(0, 0) starts with zero capacity) with no
    // config-time error — same class of bug as the site-level rateLimit
    // checks above, now enforced for consumer-level rateLimit too.
    let e = errs(
        r#"{ "port": 8080,
                 "consumers": { "consumers": [
                     { "username": "alice", "apiKey": "key",
                       "rateLimit": { "limit": 0, "windowSecs": 0 } }
                 ] } }"#,
    );
    assert!(
        e.iter().any(|err| err.path.contains("rateLimit")),
        "consumer rateLimit of 0 must be rejected: {e:?}"
    );
}

#[test]
fn consumer_valid_config_no_errors() {
    let e = errs(
        r#"{ "port": 8080,
                 "consumers": { "consumers": [
                     { "username": "alice", "apiKey": "valid-key" }
                 ] } }"#,
    );
    assert!(e.is_empty(), "valid consumer must pass: {e:?}");
}

// ── validate_forward_auth extra cases ────────────────────────────────────

#[test]
fn forward_auth_zero_timeout_rejected() {
    let e = errs(
        r#"{ "port": 8080, "forwardAuth": { "url": "http://auth:4000/verify", "timeoutMs": 0 } }"#,
    );
    assert!(
        e.iter().any(|err| err.path.contains("timeoutMs")),
        "timeoutMs=0 must be rejected: {e:?}"
    );
}

#[test]
fn forward_auth_empty_url_rejected() {
    let e = errs(r#"{ "port": 8080, "forwardAuth": { "url": "" } }"#);
    assert!(!e.is_empty(), "empty forwardAuth URL must be rejected");
}

#[test]
fn forward_auth_non_http_url_rejected() {
    let e = errs(r#"{ "port": 8080, "forwardAuth": { "url": "grpc://auth:4000/verify" } }"#);
    assert!(
        !e.is_empty(),
        "non-http(s) forwardAuth URL must be rejected"
    );
}

// ── validate_limits ───────────────────────────────────────────────────────

#[test]
fn limits_zero_inflight_requests_rejected() {
    let e = errs(r#"{ "port": 8080, "limits": { "maxInflightRequests": 0 } }"#);
    assert!(!e.is_empty(), "maxInflightRequests=0 must be rejected");
    assert!(
        e.iter().any(|err| err.path.contains("maxInflightRequests")),
        "error path must mention maxInflightRequests: {e:?}"
    );
}

#[test]
fn limits_zero_max_body_bytes_rejected() {
    let e = errs(r#"{ "port": 8080, "limits": { "maxBodyBytes": 0 } }"#);
    assert!(!e.is_empty(), "maxBodyBytes=0 must be rejected");
}

#[test]
fn limits_valid_config_no_errors() {
    let e = errs(
        r#"{ "port": 8080, "limits": { "maxInflightRequests": 100, "maxBodyBytes": 1048576 } }"#,
    );
    assert!(e.is_empty(), "valid limits config must pass: {e:?}");
}

#[test]
fn limits_zero_timeout_secs_rejected() {
    let e = errs(r#"{ "port": 8080, "limits": { "timeoutSecs": 0 } }"#);
    assert!(!e.is_empty(), "timeoutSecs=0 must be rejected");
    assert!(
        e.iter().any(|err| err.path.contains("timeoutSecs")),
        "error must mention timeoutSecs: {e:?}"
    );
}

#[test]
fn limits_timeout_secs_valid() {
    let e = errs(r#"{ "port": 8080, "limits": { "timeoutSecs": 30 } }"#);
    assert!(e.is_empty(), "valid timeoutSecs must pass: {e:?}");
}

// ── TCP + static conflict ─────────────────────────────────────────────────

#[test]
#[cfg(feature = "tcp")]
fn tcp_combined_with_static_rejected() {
    let e = errs(r#"{ "port": 3306, "tcp": { "targets": ["mysql:3306"] }, "static": "./dist" }"#);
    assert!(!e.is_empty(), "tcp + static must be rejected");
}

// ── validate_jwt_auth ─────────────────────────────────────────────────────

#[test]
fn jwt_auth_without_secret_or_jwks_rejected() {
    // No secret and no jwksUrl → configuration error.
    let e = errs(r#"{ "port": 8080, "jwtAuth": {} }"#);
    assert!(
        !e.is_empty(),
        "jwtAuth without secret or jwksUrl must be rejected"
    );
}

#[test]
fn jwt_auth_with_both_secret_and_jwks_rejected() {
    let e = errs(
        r#"{ "port": 8080, "jwtAuth": { "secret": "abc", "jwksUrl": "https://a.com/.well-known/jwks.json" } }"#,
    );
    assert!(
        !e.is_empty(),
        "jwtAuth with both secret and jwksUrl must be rejected"
    );
}

#[test]
fn jwt_auth_with_only_secret_valid() {
    // A short secret will warn but not error.
    let e = errs(
        r#"{ "port": 8080, "jwtAuth": { "secret": "my-secret-key-that-is-long-enough-32b" } }"#,
    );
    assert!(e.is_empty(), "jwtAuth with valid secret must pass: {e:?}");
}

#[test]
fn jwt_auth_with_invalid_jwks_url_rejected() {
    let e = errs(r#"{ "port": 8080, "jwtAuth": { "jwksUrl": "not-a-url" } }"#);
    assert!(!e.is_empty(), "invalid jwksUrl must be rejected");
}

#[test]
fn jwt_auth_jwks_refresh_secs_below_minimum_rejected() {
    let e = errs(
        r#"{ "port": 8080, "jwtAuth": {
                 "jwksUrl": "https://a.com/.well-known/jwks.json",
                 "jwksRefreshSecs": 5
            } }"#,
    );
    assert!(
        e.iter().any(|err| err.path.contains("jwksRefreshSecs")),
        "jwksRefreshSecs below 60 must be rejected: {e:?}"
    );
}

#[test]
fn jwt_auth_jwks_refresh_secs_at_minimum_valid() {
    let e = errs(
        r#"{ "port": 8080, "jwtAuth": {
                 "jwksUrl": "https://a.com/.well-known/jwks.json",
                 "jwksRefreshSecs": 60
            } }"#,
    );
    assert!(e.is_empty(), "jwksRefreshSecs=60 must pass: {e:?}");
}

// ── check_cert_expiry (via validate_tls) ─────────────────────────────────

#[test]
fn cert_expiry_valid_cert_no_error() {
    // Generate a self-signed cert valid for 1 year and verify no expiry error.
    let key_pair = rcgen::KeyPair::generate().expect("keygen");
    let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .expect("params")
        .self_signed(&key_pair)
        .expect("self-signed");
    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    let dir = tempfile::tempdir().unwrap();
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    std::fs::write(&cert_path, &cert_pem).unwrap();
    std::fs::write(&key_path, &key_pem).unwrap();

    // Use forward slashes in JSON path (avoid Windows backslash escaping issues).
    let cert_str = cert_path.to_string_lossy().replace('\\', "/");
    let key_str = key_path.to_string_lossy().replace('\\', "/");
    let config_json =
        format!(r#"{{ "port": 443, "tls": {{ "cert": "{cert_str}", "key": "{key_str}" }} }}"#);
    let e = errs(&config_json);
    // A freshly generated cert valid for 1 year must not produce expiry errors.
    assert!(
        !e.iter().any(|err| err.message.contains("expired")),
        "fresh cert must not produce expiry error: {e:?}"
    );
}

// ── validate_tls mTLS clientAuth ─────────────────────────────────────────

#[test]
fn mtls_empty_ca_path_rejected() {
    let e = errs(
        r#"{ "port": 443, "tls": { "cert": "server.pem", "key": "server.key",
                 "clientAuth": { "ca": "" } } }"#,
    );
    assert!(
        e.iter().any(|err| err.path.contains("clientAuth")),
        "empty CA path must be rejected: {e:?}"
    );
}

#[test]
fn mtls_without_tls_rejected() {
    // clientAuth without cert+key or acme must be rejected.
    let e = errs(r#"{ "port": 8080, "tls": { "clientAuth": { "ca": "ca.pem" } } }"#);
    assert!(
        e.iter()
            .any(|err| err.message.contains("cert") || err.path.contains("clientAuth")),
        "clientAuth without cert/key must be rejected: {e:?}"
    );
}

// ── is_valid_upstream_url ─────────────────────────────────────────────────

#[test]
fn upstream_url_http_valid() {
    let e = errs(r#"{ "port": 8080, "proxy": { "/": { "targets": ["http://backend:4000"] } } }"#);
    assert!(e.is_empty(), "valid http upstream must pass: {e:?}");
}

#[test]
fn upstream_url_no_scheme_rejected() {
    let e = errs(r#"{ "port": 8080, "proxy": { "/": { "targets": ["backend:4000"] } } }"#);
    assert!(
        !e.is_empty(),
        "upstream without scheme must be rejected: {e:?}"
    );
}

#[test]
fn upstream_url_https_valid() {
    let e = errs(
        r#"{ "port": 8080, "proxy": { "/": { "targets": ["https://api.example.com:443"] } } }"#,
    );
    assert!(e.is_empty(), "valid https upstream must pass: {e:?}");
}

// ── validate_rewrite_rules ────────────────────────────────────────────────

#[test]
fn rewrite_rule_invalid_regex_rejected() {
    let e = errs(
        r#"{ "port": 8080, "proxy": {
                "/": { "targets": ["http://b:4000"],
                        "rewrite": [{ "from": "[invalid", "to": "/v2/$1" }] } } }"#,
    );
    assert!(
        !e.is_empty(),
        "invalid rewrite regex must be rejected: {e:?}"
    );
}

#[test]
fn rewrite_rule_valid_regex_passes() {
    let e = errs(
        r#"{ "port": 8080, "proxy": {
                "/": { "targets": ["http://b:4000"],
                        "rewrite": [{ "from": "^/v1/(.*)", "to": "/v2/$1" }] } } }"#,
    );
    assert!(e.is_empty(), "valid rewrite regex must pass: {e:?}");
}

// ── validate_route_config: mirror URL ────────────────────────────────────

#[test]
fn mirror_url_http_valid() {
    let e = errs(
        r#"{ "port": 8080, "proxy": {
                "/": { "targets": ["http://b:4000"],
                        "mirror": "http://mirror:4001" } } }"#,
    );
    assert!(e.is_empty(), "valid mirror URL must pass: {e:?}");
}

#[test]
fn mirror_url_no_scheme_rejected() {
    let e = errs(
        r#"{ "port": 8080, "proxy": {
                "/": { "targets": ["http://b:4000"],
                        "mirror": "mirror:4001" } } }"#,
    );
    assert!(
        !e.is_empty(),
        "mirror URL without scheme must be rejected: {e:?}"
    );
}

// ── loopback_port with IPv6 ───────────────────────────────────────────────

#[cfg(feature = "proxy")]
#[test]
fn loopback_ipv6_loop_detected() {
    let w = warns(r#"{ "port": 8080, "proxy": "http://[::1]:8080" }"#);
    assert!(
        w.iter().any(|m| m.contains("loop")),
        "IPv6 loopback loop must warn: {w:?}"
    );
}

// ── validate_upload: empty dir ───────────────────────────────────────────

#[test]
fn upload_whitespace_dir_rejected() {
    let e = errs(r#"{ "upload": { "path": "/upload", "dir": "   " } }"#);
    assert!(!e.is_empty(), "whitespace-only dir must be rejected");
}

// ── check_weighted_targets ────────────────────────────────────────────────

#[test]
fn weighted_round_robin_with_simple_target_rejected() {
    let e = errs(
        r#"{ "port": 8080, "proxy": {
                "/": {
                    "targets": ["http://b:4000"],
                    "strategy": "weighted-round-robin"
                }
            } }"#,
    );
    assert!(
        !e.is_empty(),
        "simple target with weighted-round-robin must be rejected: {e:?}"
    );
    assert!(
        e.iter().any(|err| err.message.contains("weighted")),
        "error must mention weighted: {e:?}"
    );
}

#[test]
fn weighted_round_robin_with_weighted_target_passes() {
    let e = errs(
        r#"{ "port": 8080, "proxy": {
                "/": {
                    "targets": [{ "url": "http://b:4000", "weight": 3 }],
                    "strategy": "weighted-round-robin"
                }
            } }"#,
    );
    assert!(
        e.is_empty(),
        "weighted target with weighted-round-robin must pass: {e:?}"
    );
}

// ── validate_groups_config ────────────────────────────────────────────────

#[test]
fn group_with_empty_targets_rejected() {
    let e = errs(
        r#"{ "port": 8080, "proxy": {
                "/": {
                    "targets": [],
                    "groups": [{ "name": "empty-group", "targets": [] }]
                }
            } }"#,
    );
    assert!(
        !e.is_empty(),
        "group with no targets must be rejected: {e:?}"
    );
    assert!(
        e.iter().any(|err| err.path.contains("groups")),
        "error must mention groups: {e:?}"
    );
}

#[test]
fn group_with_targets_passes() {
    let e = errs(
        r#"{ "port": 8080, "proxy": {
                "/": {
                    "targets": [],
                    "groups": [{ "name": "g1", "targets": [{ "url": "http://b:4000", "weight": 1 }] }]
                }
            } }"#,
    );
    assert!(e.is_empty(), "group with targets must pass: {e:?}");
}

// ── validate_proxy_route_target: RoundRobin ───────────────────────────────

#[test]
fn round_robin_with_invalid_url_rejected() {
    let e = errs(
        r#"{ "port": 8080, "proxy": {
                "/": ["not-a-url", "http://valid:4000"]
            } }"#,
    );
    assert!(
        !e.is_empty(),
        "RoundRobin with invalid URL must be rejected: {e:?}"
    );
}

#[test]
fn round_robin_all_valid_passes() {
    let e = errs(
        r#"{ "port": 8080, "proxy": {
                "/": ["http://a:4000", "http://b:4000"]
            } }"#,
    );
    assert!(e.is_empty(), "valid RoundRobin must pass: {e:?}");
}

// ── validate_proxy: Single with invalid URL ────────────────────────────────

#[test]
fn single_proxy_invalid_url_rejected() {
    let e = errs(r#"{ "port": 8080, "proxy": "not-a-url" }"#);
    assert!(
        !e.is_empty(),
        "Single proxy with invalid URL must be rejected: {e:?}"
    );
}

// ── IPv6 TCP targets ──────────────────────────────────────────────────────

#[test]
#[cfg(feature = "tcp")]
fn ipv6_tcp_target_valid() {
    let e = errs(r#"{ "port": 3306, "tcp": { "targets": ["[::1]:3306"] } }"#);
    assert!(e.is_empty(), "IPv6 TCP target must pass: {e:?}");
}

// ── proxy loop detection via routes[] array ───────────────────────────────

#[cfg(feature = "proxy")]
#[test]
fn proxy_loop_detection_via_routes_array() {
    let w = warns(
        r#"{ "port": 8080, "routes": [{ "match": { "path": "/**" }, "proxy": "http://127.0.0.1:8080" }] }"#,
    );
    assert!(
        w.iter().any(|m| m.contains("loop")),
        "routes array pointing back to self must warn: {w:?}"
    );
}

#[cfg(feature = "proxy")]
#[test]
fn proxy_loop_detection_routes_array_external_no_warn() {
    let w = warns(
        r#"{ "port": 8080, "routes": [{ "match": { "path": "/**" }, "proxy": "http://api.example.com:8080" }] }"#,
    );
    assert!(
        w.iter().all(|m| !m.contains("loop")),
        "external host must not warn: {w:?}"
    );
}

// ── sanitize_for_log (issue #185: CWE-117-style log injection) ────────────

#[test]
fn sanitize_for_log_leaves_plain_text_unchanged() {
    assert_eq!(
        sanitize_for_log("http://127.0.0.1:8080/path?q=1"),
        "http://127.0.0.1:8080/path?q=1"
    );
}

#[test]
fn sanitize_for_log_escapes_newline() {
    // The exact forged-log-line shape issue #185 describes: a raw '\n'
    // would let a config value start what looks like a second,
    // independent log line.
    assert_eq!(
        sanitize_for_log("evil\nfake log line: [ERROR] pwned"),
        "evil\\nfake log line: [ERROR] pwned"
    );
    assert!(
        !sanitize_for_log("a\nb").contains('\n'),
        "no raw newline must survive sanitization"
    );
}

#[test]
fn sanitize_for_log_escapes_carriage_return() {
    assert_eq!(sanitize_for_log("a\rb"), "a\\rb");
}

#[test]
fn sanitize_for_log_escapes_other_control_chars() {
    // NUL and a bell character, as a stand-in for "any non-printable byte".
    let out = sanitize_for_log("a\0b\x07c");
    assert!(!out.contains('\0') && !out.contains('\x07'));
    assert_eq!(out, "a\\u{0}b\\u{7}c");
}

#[test]
fn sanitize_for_log_preserves_unicode() {
    // Non-control, non-ASCII text must pass through untouched -- this is
    // an escaping filter for control characters, not an ASCII-only one.
    assert_eq!(
        sanitize_for_log("héllo — wörld 日本語"),
        "héllo — wörld 日本語"
    );
}

// ── consumers.sharedJwt validation ───────────────────────────────────────

#[test]
fn consumers_shared_jwt_no_secret_or_jwks_rejected() {
    let e = errs(
        r#"{ "port": 8080,
                 "consumers": { "consumers": [{ "username": "u", "apiKey": "k" }],
                                "sharedJwt": {} } }"#,
    );
    assert!(
        !e.is_empty(),
        "sharedJwt without secret or jwksUrl must error: {e:?}"
    );
}
