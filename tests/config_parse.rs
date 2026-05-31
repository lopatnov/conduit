use conduit::config::{
    from_str,
    schema::{
        CompressionConfig,
        CorsConfig,
        // New Phase 4 types
        ForwardAuthConfig,
        HealthCheckConfig,
        HotReloadConfig,
        JwtAuthConfig,
        LoadBalanceStrategy,
        LogFormat,
        LoggingConfig,
        ProxyConfig,
        ProxyRouteTarget,
        ProxyTarget,
        ResponseTimeConfig,
        SecurityHeadersConfig,
        StaticConfig,
        UpstreamTlsConfig,
    },
};
use serial_test::serial;

// ── Helpers ────────────────────────────────────────────────────────────────

fn parse(json: &str) -> conduit::config::AppConfig {
    from_str(json).expect("parse failed")
}

fn parse_err(json: &str) -> String {
    from_str(json).unwrap_err().to_string()
}

// ── ConfigFile forms ───────────────────────────────────────────────────────

#[test]
fn single_form_port_only() {
    let cfg = parse(r#"{ "port": 8080 }"#);
    assert_eq!(cfg.sites.len(), 1);
    assert_eq!(cfg.sites[0].port, Some(8080));
    assert!(cfg.global.is_none());
}

#[test]
fn sites_array_form() {
    let cfg = parse(r#"[{ "port": 8080 }, { "port": 8081 }]"#);
    assert_eq!(cfg.sites.len(), 2);
    assert_eq!(cfg.sites[0].port, Some(8080));
    assert_eq!(cfg.sites[1].port, Some(8081));
    assert!(cfg.global.is_none());
}

#[test]
fn full_form_with_global() {
    let cfg = parse(
        r#"{
            "global": { "workers": 4, "shutdownTimeoutSecs": 30 },
            "sites": [{ "port": 443 }]
        }"#,
    );
    assert!(cfg.global.is_some());
    let g = cfg.global.unwrap();
    assert_eq!(g.workers, Some(4));
    assert_eq!(g.shutdown_timeout_secs, Some(30));
    assert_eq!(cfg.sites[0].port, Some(443));
}

#[test]
fn full_form_admin_bind() {
    let cfg = parse(
        r#"{
            "global": { "admin": { "bind": "0.0.0.0:2019" } },
            "sites": []
        }"#,
    );
    let bind = cfg.global.unwrap().admin.unwrap().bind.unwrap();
    assert_eq!(bind, "0.0.0.0:2019");
}

#[test]
fn version_1_accepted() {
    let cfg = parse(r#"{ "version": 1, "port": 3000 }"#);
    assert_eq!(cfg.sites[0].port, Some(3000));
}

#[test]
fn version_too_new_rejected() {
    let err = parse_err(r#"{ "version": 99, "port": 3000 }"#);
    assert!(err.contains("version 99"), "got: {err}");
    assert!(err.contains("not supported"), "got: {err}");
}

// ── Static field renaming ──────────────────────────────────────────────────

#[test]
fn static_keyword_renamed() {
    let cfg = parse(r#"{ "port": 8080, "static": "./dist" }"#);
    let s = cfg.sites[0].static_files.as_ref().unwrap();
    assert_eq!(*s, StaticConfig::Single("./dist".into()));
}

#[test]
fn static_multi_form() {
    let cfg = parse(r#"{ "static": ["./dist", "./public"] }"#);
    match cfg.sites[0].static_files.as_ref().unwrap() {
        StaticConfig::Multi(v) => assert_eq!(v, &["./dist", "./public"]),
        other => panic!("expected Multi, got {other:?}"),
    }
}

#[test]
fn static_mapped_form() {
    let cfg = parse(r#"{ "static": { "/": "./dist", "/docs": "./docs-dist" } }"#);
    match cfg.sites[0].static_files.as_ref().unwrap() {
        StaticConfig::Mapped(m) => {
            assert_eq!(m.get("/").map(String::as_str), Some("./dist"));
            assert_eq!(m.get("/docs").map(String::as_str), Some("./docs-dist"));
        }
        other => panic!("expected Mapped, got {other:?}"),
    }
}

// ── Logging shorthand ──────────────────────────────────────────────────────

#[test]
fn logging_bool_true() {
    let cfg = parse(r#"{ "logging": true }"#);
    assert_eq!(cfg.sites[0].logging, Some(LoggingConfig::Enabled(true)));
}

#[test]
fn logging_bool_false() {
    let cfg = parse(r#"{ "logging": false }"#);
    assert_eq!(cfg.sites[0].logging, Some(LoggingConfig::Enabled(false)));
}

#[test]
fn logging_format_string() {
    let cfg = parse(r#"{ "logging": "dev" }"#);
    assert_eq!(
        cfg.sites[0].logging,
        Some(LoggingConfig::Format(LogFormat::Dev))
    );
}

#[test]
fn logging_format_json() {
    let cfg = parse(r#"{ "logging": "json" }"#);
    assert_eq!(
        cfg.sites[0].logging,
        Some(LoggingConfig::Format(LogFormat::Json))
    );
}

#[test]
fn logging_options_object() {
    let cfg = parse(r#"{ "logging": { "format": "combined", "file": "./access.log" } }"#);
    match cfg.sites[0].logging.as_ref().unwrap() {
        LoggingConfig::Options(o) => {
            assert_eq!(o.format, Some(LogFormat::Combined));
            assert_eq!(o.file.as_deref(), Some("./access.log"));
        }
        other => panic!("expected Options, got {other:?}"),
    }
}

// ── Compression shorthand ──────────────────────────────────────────────────

#[test]
fn compression_bool() {
    let cfg = parse(r#"{ "compression": true }"#);
    assert_eq!(
        cfg.sites[0].compression,
        Some(CompressionConfig::Enabled(true))
    );
}

#[test]
fn compression_options() {
    let cfg = parse(r#"{ "compression": { "algorithms": ["br", "gzip"], "level": 6 } }"#);
    match cfg.sites[0].compression.as_ref().unwrap() {
        CompressionConfig::Options(o) => {
            assert_eq!(
                o.algorithms.as_deref(),
                Some(&["br".to_string(), "gzip".to_string()][..])
            );
            assert_eq!(o.level, Some(6));
        }
        other => panic!("expected Options, got {other:?}"),
    }
}

// ── Other bool/object shorthand fields ────────────────────────────────────

#[test]
fn response_time_bool() {
    let cfg = parse(r#"{ "responseTime": true }"#);
    assert_eq!(
        cfg.sites[0].response_time,
        Some(ResponseTimeConfig::Enabled(true))
    );
}

#[test]
fn security_headers_bool() {
    let cfg = parse(r#"{ "securityHeaders": true }"#);
    assert_eq!(
        cfg.sites[0].security_headers,
        Some(SecurityHeadersConfig::Enabled(true))
    );
}

#[test]
fn cors_bool() {
    let cfg = parse(r#"{ "cors": true }"#);
    assert_eq!(cfg.sites[0].cors, Some(CorsConfig::Enabled(true)));
}

#[test]
fn cors_options() {
    let cfg =
        parse(r#"{ "cors": { "origins": ["https://app.example.com"], "credentials": true } }"#);
    match cfg.sites[0].cors.as_ref().unwrap() {
        CorsConfig::Options(o) => {
            assert_eq!(
                o.origins.as_deref(),
                Some(&["https://app.example.com".to_string()][..])
            );
            assert_eq!(o.credentials, Some(true));
        }
        other => panic!("expected Options, got {other:?}"),
    }
}

#[test]
fn hot_reload_bool() {
    let cfg = parse(r#"{ "hotReload": false }"#);
    assert_eq!(
        cfg.sites[0].hot_reload,
        Some(HotReloadConfig::Enabled(false))
    );
}

#[test]
fn health_check_bool() {
    let cfg = parse(r#"{ "healthCheck": true }"#);
    assert_eq!(
        cfg.sites[0].health_check,
        Some(HealthCheckConfig::Enabled(true))
    );
}

#[test]
fn health_check_options() {
    let cfg = parse(r#"{ "healthCheck": { "path": "/ping", "includeUpstreams": true } }"#);
    match cfg.sites[0].health_check.as_ref().unwrap() {
        HealthCheckConfig::Options(o) => {
            assert_eq!(o.path.as_deref(), Some("/ping"));
            assert_eq!(o.include_upstreams, Some(true));
        }
        other => panic!("expected Options, got {other:?}"),
    }
}

// ── Proxy forms ────────────────────────────────────────────────────────────

#[test]
fn proxy_single_string() {
    let cfg = parse(r#"{ "proxy": "http://localhost:4000" }"#);
    assert_eq!(
        cfg.sites[0].proxy,
        Some(ProxyConfig::Single("http://localhost:4000".into()))
    );
}

#[test]
fn proxy_route_url_shorthand() {
    let cfg = parse(r#"{ "proxy": { "/api": "http://backend:4000" } }"#);
    match cfg.sites[0].proxy.as_ref().unwrap() {
        ProxyConfig::Routes(routes) => {
            assert_eq!(
                routes.get("/api"),
                Some(&ProxyRouteTarget::Url("http://backend:4000".into()))
            );
        }
        other => panic!("expected Routes, got {other:?}"),
    }
}

#[test]
fn proxy_route_round_robin_shorthand() {
    let cfg = parse(r#"{ "proxy": { "/api": ["http://b1:4000", "http://b2:4000"] } }"#);
    match cfg.sites[0].proxy.as_ref().unwrap() {
        ProxyConfig::Routes(routes) => match routes.get("/api").unwrap() {
            ProxyRouteTarget::RoundRobin(urls) => {
                assert_eq!(urls, &["http://b1:4000", "http://b2:4000"]);
            }
            other => panic!("expected RoundRobin, got {other:?}"),
        },
        other => panic!("expected Routes, got {other:?}"),
    }
}

#[test]
fn proxy_route_full_with_strategy() {
    let cfg = parse(
        r#"{
            "proxy": {
                "/api": {
                    "targets": ["http://b1:4000", "http://b2:4000"],
                    "strategy": "least-conn",
                    "stripPrefix": true
                }
            }
        }"#,
    );
    match cfg.sites[0].proxy.as_ref().unwrap() {
        ProxyConfig::Routes(routes) => match routes.get("/api").unwrap() {
            ProxyRouteTarget::Full(r) => {
                assert_eq!(r.strategy, Some(LoadBalanceStrategy::LeastConn));
                assert_eq!(r.strip_prefix, Some(true));
                assert_eq!(r.targets.len(), 2);
            }
            other => panic!("expected Full, got {other:?}"),
        },
        other => panic!("expected Routes, got {other:?}"),
    }
}

#[test]
fn proxy_weighted_targets() {
    let cfg = parse(
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
        }"#,
    );
    match cfg.sites[0].proxy.as_ref().unwrap() {
        ProxyConfig::Routes(routes) => match routes.get("/api").unwrap() {
            ProxyRouteTarget::Full(r) => {
                assert_eq!(r.strategy, Some(LoadBalanceStrategy::WeightedRoundRobin));
                match &r.targets[0] {
                    ProxyTarget::Weighted(w) => {
                        assert_eq!(w.url, "http://b1:4000");
                        assert_eq!(w.weight, 3);
                    }
                    other => panic!("expected Weighted, got {other:?}"),
                }
            }
            other => panic!("expected Full, got {other:?}"),
        },
        other => panic!("expected Routes, got {other:?}"),
    }
}

#[test]
fn load_balance_strategies_parse() {
    for (s, expected) in [
        ("round-robin", LoadBalanceStrategy::RoundRobin),
        (
            "weighted-round-robin",
            LoadBalanceStrategy::WeightedRoundRobin,
        ),
        ("random", LoadBalanceStrategy::Random),
        ("least-conn", LoadBalanceStrategy::LeastConn),
        (
            "least-response-time",
            LoadBalanceStrategy::LeastResponseTime,
        ),
        ("ip-hash", LoadBalanceStrategy::IpHash),
        ("consistent-hash", LoadBalanceStrategy::ConsistentHash),
    ] {
        let json = format!(
            r#"{{ "proxy": {{ "/": {{ "targets": ["http://b:4000"], "strategy": "{s}" }} }} }}"#
        );
        let cfg = parse(&json);
        match cfg.sites[0].proxy.as_ref().unwrap() {
            ProxyConfig::Routes(routes) => match routes.get("/").unwrap() {
                ProxyRouteTarget::Full(r) => {
                    assert_eq!(r.strategy, Some(expected), "strategy: {s}");
                }
                _ => panic!("expected Full for strategy {s}"),
            },
            _ => panic!("expected Routes"),
        }
    }
}

// ── Complex configs from examples ──────────────────────────────────────────

#[test]
fn minimal_example() {
    let cfg = parse(
        r#"{ "port": 8080, "static": "./dist", "proxy": { "/api": "http://localhost:4000" } }"#,
    );
    assert_eq!(cfg.sites[0].port, Some(8080));
    assert!(cfg.sites[0].static_files.is_some());
    assert!(cfg.sites[0].proxy.is_some());
}

#[test]
fn rate_limit_config() {
    let cfg = parse(r#"{ "rateLimit": { "windowSecs": 60, "limit": 100, "keyBy": "ip" } }"#);
    let rl = cfg.sites[0].rate_limit.as_ref().unwrap();
    assert_eq!(rl.window_secs, 60);
    assert_eq!(rl.limit, 100);
    assert_eq!(rl.key_by.as_deref(), Some("ip"));
}

#[test]
fn basic_auth_config() {
    let cfg = parse(r#"{ "basicAuth": { "users": { "admin": "secret" }, "challenge": true } }"#);
    let ba = cfg.sites[0].basic_auth.as_ref().unwrap();
    assert_eq!(ba.users.get("admin").map(String::as_str), Some("secret"));
    assert_eq!(ba.challenge, Some(true));
}

#[test]
fn ip_filter_allow_deny() {
    let cfg = parse(
        r#"{ "ipFilter": { "allow": ["10.0.0.0/8"], "deny": ["1.2.3.4"], "trustProxy": true } }"#,
    );
    let f = cfg.sites[0].ip_filter.as_ref().unwrap();
    assert_eq!(f.allow.as_deref(), Some(&["10.0.0.0/8".to_string()][..]));
    assert_eq!(f.trust_proxy, Some(true));
}

#[test]
fn fallback_by_accept() {
    let cfg = parse(
        r#"{
            "fallback": {
                "byAccept": {
                    "html": { "status": 200, "file": "./index.html" },
                    "json": { "status": 404, "body": { "error": "Not Found" } }
                }
            }
        }"#,
    );
    let fb = cfg.sites[0].fallback.as_ref().unwrap();
    let by_accept = fb.by_accept.as_ref().unwrap();
    assert_eq!(by_accept.get("html").and_then(|r| r.status), Some(200));
    assert_eq!(by_accept.get("json").and_then(|r| r.status), Some(404));
}

#[test]
fn tls_acme_config() {
    let cfg = parse(
        r#"{
            "port": 443,
            "tls": { "acme": { "email": "admin@example.com", "challenge": "http-01" } }
        }"#,
    );
    let tls = cfg.sites[0].tls.as_ref().unwrap();
    let acme = tls.acme.as_ref().unwrap();
    assert_eq!(acme.email, "admin@example.com");
    assert_eq!(acme.challenge.as_deref(), Some("http-01"));
}

#[test]
fn proxy_cache_config() {
    let cfg = parse(
        r#"{
            "proxy": {
                "/api": {
                    "targets": ["http://backend:4000"],
                    "cache": { "store": "memory", "ttlSecs": 300, "skipIfCookie": true }
                }
            }
        }"#,
    );
    match cfg.sites[0].proxy.as_ref().unwrap() {
        ProxyConfig::Routes(routes) => match routes.get("/api").unwrap() {
            ProxyRouteTarget::Full(r) => {
                let cache = r.cache.as_ref().unwrap();
                assert_eq!(cache.store, "memory");
                assert_eq!(cache.ttl_secs, Some(300));
                assert_eq!(cache.skip_if_cookie, Some(true));
            }
            _ => panic!("expected Full"),
        },
        _ => panic!("expected Routes"),
    }
}

#[test]
fn proxy_retry_config() {
    let cfg = parse(
        r#"{
            "proxy": {
                "/api": {
                    "targets": ["http://backend:4000"],
                    "retry": { "attempts": 3, "conditions": ["connection_error", "5xx"], "backoffMs": 100 }
                }
            }
        }"#,
    );
    match cfg.sites[0].proxy.as_ref().unwrap() {
        ProxyConfig::Routes(routes) => match routes.get("/api").unwrap() {
            ProxyRouteTarget::Full(r) => {
                let retry = r.retry.as_ref().unwrap();
                assert_eq!(retry.attempts, 3);
                assert_eq!(retry.conditions, ["connection_error", "5xx"]);
                assert_eq!(retry.backoff_ms, Some(100));
            }
            _ => panic!("expected Full"),
        },
        _ => panic!("expected Routes"),
    }
}

#[test]
fn upload_config() {
    let cfg = parse(
        r#"{
            "upload": {
                "path": "/upload",
                "dir": "./uploads",
                "maxFileSizeBytes": 10485760,
                "maxFiles": 5,
                "allowedMimeTypes": ["image/jpeg", "image/png"]
            }
        }"#,
    );
    let u = cfg.sites[0].upload.as_ref().unwrap();
    assert_eq!(u.path, "/upload");
    assert_eq!(u.dir, "./uploads");
    assert_eq!(u.max_file_size_bytes, Some(10485760));
    assert_eq!(u.max_files, Some(5));
}

// ── Environment variable interpolation ────────────────────────────────────

#[test]
#[serial]
fn env_interpolation_in_config() {
    std::env::set_var("CONDUIT_PARSE_TEST_PORT", "9999");
    // interpolation is text-level: the value is substituted before JSON parsing
    let cfg = parse(r#"{ "metrics": { "token": "$CONDUIT_PARSE_TEST_PORT" } }"#);
    assert_eq!(
        cfg.sites[0].metrics.as_ref().unwrap().token.as_deref(),
        Some("9999")
    );
}

// ── routes array (Phase 3.6) ──────────────────────────────────────────────

#[test]
fn routes_array_parsed() {
    let cfg = parse(
        r#"{
            "routes": [
                {
                    "match": { "path": "/api/**", "method": ["GET", "POST"] },
                    "proxy": "http://backend:4000"
                },
                {
                    "match": { "path": "/public/**" },
                    "static": "./public"
                }
            ]
        }"#,
    );
    let routes = cfg.sites[0].routes.as_ref().expect("routes must be set");
    assert_eq!(routes.len(), 2);
    assert_eq!(routes[0].r#match.path.as_deref(), Some("/api/**"));
    let method = routes[0]
        .r#match
        .method
        .as_ref()
        .expect("method must be set");
    assert_eq!(method, &["GET", "POST"]);
    assert!(matches!(routes[0].proxy, Some(ProxyRouteTarget::Url(_))));
    assert_eq!(routes[1].r#match.path.as_deref(), Some("/public/**"));
    assert!(routes[1].static_files.is_some());
}

#[test]
fn routes_array_full_proxy_config() {
    let cfg = parse(
        r#"{
            "routes": [
                {
                    "match": { "path": "/api/**" },
                    "proxy": {
                        "targets": ["http://b1:4000", "http://b2:4000"],
                        "strategy": "least-conn",
                        "stripPrefix": true
                    }
                }
            ]
        }"#,
    );
    let route = &cfg.sites[0].routes.as_ref().unwrap()[0];
    if let Some(ProxyRouteTarget::Full(pcfg)) = &route.proxy {
        assert_eq!(pcfg.strategy, Some(LoadBalanceStrategy::LeastConn));
        assert_eq!(pcfg.strip_prefix, Some(true));
    } else {
        panic!("expected Full proxy target");
    }
}

// ── upstream groups (Phase 3.7b) ──────────────────────────────────────────

#[test]
fn groups_parsed() {
    let cfg = parse(
        r#"{
            "proxy": {
                "/api": {
                    "groups": [
                        { "name": "us-east", "targets": ["http://us1:4000", "http://us2:4000"], "strategy": "round-robin" },
                        { "name": "eu-west", "targets": ["http://eu1:4000"], "strategy": "least-conn" }
                    ],
                    "groupStrategy": "ip-hash"
                }
            }
        }"#,
    );
    if let Some(ProxyConfig::Routes(routes)) = &cfg.sites[0].proxy {
        if let Some(ProxyRouteTarget::Full(pcfg)) = routes.get("/api") {
            let groups = pcfg.groups.as_ref().expect("groups must be set");
            assert_eq!(groups.len(), 2);
            assert_eq!(groups[0].name, "us-east");
            assert_eq!(groups[0].targets.len(), 2);
            assert_eq!(groups[0].strategy, Some(LoadBalanceStrategy::RoundRobin));
            assert_eq!(groups[1].name, "eu-west");
            assert_eq!(groups[1].strategy, Some(LoadBalanceStrategy::LeastConn));
            assert_eq!(pcfg.group_strategy, Some(LoadBalanceStrategy::IpHash));
        } else {
            panic!("expected Full target for /api");
        }
    } else {
        panic!("expected Routes proxy config");
    }
}

#[test]
fn groups_no_top_level_targets_parsed() {
    // groups without top-level targets must deserialise successfully (serde(default))
    let cfg = parse(
        r#"{
            "proxy": {
                "/": {
                    "groups": [
                        { "name": "a", "targets": ["http://a:4000"] }
                    ]
                }
            }
        }"#,
    );
    if let Some(ProxyConfig::Routes(routes)) = &cfg.sites[0].proxy {
        if let Some(ProxyRouteTarget::Full(pcfg)) = routes.get("/") {
            assert!(
                pcfg.targets.is_empty(),
                "top-level targets must default to []"
            );
            assert!(pcfg.groups.is_some());
        } else {
            panic!("expected Full target");
        }
    } else {
        panic!("expected Routes");
    }
}

// ── rewrite rules (Phase 3.7) ─────────────────────────────────────────────

#[test]
fn rewrite_rules_parsed() {
    let cfg = parse(
        r#"{
            "proxy": {
                "/api": {
                    "targets": ["http://backend:4000"],
                    "rewrite": [
                        { "from": "^/v[0-9]+/(.+)$", "to": "/$1" },
                        { "from": "^/users/([0-9]+)$", "to": "/members/$1" }
                    ]
                }
            }
        }"#,
    );
    if let Some(ProxyConfig::Routes(routes)) = &cfg.sites[0].proxy {
        if let Some(ProxyRouteTarget::Full(pcfg)) = routes.get("/api") {
            let rules = pcfg.rewrite.as_ref().expect("rewrite must be set");
            assert_eq!(rules.len(), 2);
            assert_eq!(rules[0].from, "^/v[0-9]+/(.+)$");
            assert_eq!(rules[0].to, "/$1");
        } else {
            panic!("expected Full target");
        }
    } else {
        panic!("expected Routes");
    }
}

// ── Error paths ────────────────────────────────────────────────────────────

#[test]
fn bad_type_rejects_parse() {
    // serde's untagged ConfigFile enum swallows inner field paths when all variants fail,
    // so we cannot assert on the exact field name in the error. The validate step
    // (Phase 1.3) gives precise diagnostics on top of a successful parse.
    assert!(
        from_str(r#"{ "rateLimit": { "windowSecs": "not-a-number", "limit": 100 } }"#).is_err()
    );
}

#[test]
fn invalid_json_rejected() {
    assert!(from_str("{ not valid json }").is_err());
}

// ── Phase 4 new config fields ─────────────────────────────────────────────────

#[test]
fn jwt_auth_hs256_secret_parses() {
    let cfg = parse(r#"{ "port": 8080, "jwtAuth": { "secret": "mysecret" } }"#);
    let jwt = cfg.sites[0]
        .jwt_auth
        .as_ref()
        .expect("jwtAuth should be present");
    assert_eq!(jwt.secret.as_deref(), Some("mysecret"));
    assert!(jwt.jwks_url.is_none());
}

#[test]
fn jwt_auth_jwks_url_parses() {
    let cfg = parse(
        r#"{
        "port": 8080,
        "jwtAuth": {
            "jwksUrl": "https://accounts.example.com/.well-known/jwks.json",
            "audience": ["my-app"],
            "issuer": "https://accounts.example.com",
            "skipPaths": ["/__health__"]
        }
    }"#,
    );
    let jwt = cfg.sites[0]
        .jwt_auth
        .as_ref()
        .expect("jwtAuth should be present");
    assert!(jwt.jwks_url.is_some());
    assert_eq!(jwt.audience.as_ref().unwrap()[0], "my-app");
    assert_eq!(jwt.issuer.as_deref(), Some("https://accounts.example.com"));
    assert_eq!(jwt.skip_paths.as_ref().unwrap()[0], "/__health__");
}

#[test]
fn forward_auth_parses() {
    let cfg = parse(
        r#"{
        "port": 8080,
        "forwardAuth": {
            "url": "http://auth:9000/verify",
            "requestHeaders": ["Authorization", "Cookie"],
            "responseHeaders": ["X-User-ID"],
            "timeoutMs": 3000,
            "skipPaths": ["/__health__"]
        }
    }"#,
    );
    let fa = cfg.sites[0]
        .forward_auth
        .as_ref()
        .expect("forwardAuth should be present");
    assert_eq!(fa.url, "http://auth:9000/verify");
    assert_eq!(fa.request_headers.as_ref().unwrap().len(), 2);
    assert_eq!(fa.response_headers.as_ref().unwrap()[0], "X-User-ID");
    assert_eq!(fa.timeout_ms, Some(3000));
}

#[test]
fn mask_errors_parses() {
    let cfg = parse(r#"{ "port": 8080, "maskErrors": true }"#);
    assert_eq!(cfg.sites[0].mask_errors, Some(true));
    let cfg2 = parse(r#"{ "port": 8080, "maskErrors": false }"#);
    assert_eq!(cfg2.sites[0].mask_errors, Some(false));
}

#[test]
fn outlier_detection_parses() {
    let cfg = parse(
        r#"{
        "port": 8080,
        "outlierDetection": {
            "consecutive5xx": 5,
            "baseEjectionTimeSecs": 30,
            "maxEjectionTimeSecs": 300,
            "maxEjectionPercent": 10
        }
    }"#,
    );
    let od = cfg.sites[0]
        .outlier_detection
        .as_ref()
        .expect("outlierDetection present");
    assert_eq!(od.consecutive_5xx, Some(5));
    assert_eq!(od.base_ejection_time_secs, Some(30));
    assert_eq!(od.max_ejection_percent, Some(10));
}

#[test]
fn request_response_transform_parses() {
    let cfg = parse(
        r#"{
        "port": 8080,
        "requestTransform": { "setHeaders": { "X-Inject": "val" }, "removeHeaders": ["X-Del"] },
        "responseTransform": { "setHeaders": { "X-Served-By": "conduit" } }
    }"#,
    );
    let rt = cfg.sites[0]
        .request_transform
        .as_ref()
        .expect("requestTransform present");
    assert_eq!(
        rt.set_headers
            .as_ref()
            .unwrap()
            .get("X-Inject")
            .map(|s| s.as_str()),
        Some("val")
    );
    assert_eq!(rt.remove_headers.as_ref().unwrap()[0], "X-Del");
    let resp_t = cfg.sites[0]
        .response_transform
        .as_ref()
        .expect("responseTransform present");
    assert_eq!(resp_t.set_headers.as_ref().unwrap().len(), 1);
}

#[test]
fn upstream_tls_parses() {
    let cfg = parse(
        r#"{
        "port": 8080,
        "proxy": {
            "/api": {
                "targets": ["https://backend:4443"],
                "upstreamTls": { "verify": false, "serverName": "my-service" }
            }
        }
    }"#,
    );
    if let Some(conduit::config::schema::ProxyConfig::Routes(routes)) = &cfg.sites[0].proxy {
        if let Some(conduit::config::schema::ProxyRouteTarget::Full(route)) = routes.get("/api") {
            let tls = route.upstream_tls.as_ref().expect("upstreamTls present");
            assert_eq!(tls.verify, Some(false));
            assert_eq!(tls.server_name.as_deref(), Some("my-service"));
        } else {
            panic!("expected Full route");
        }
    } else {
        panic!("expected Routes proxy");
    }
}

#[test]
fn per_route_rate_limit_parses() {
    let cfg = parse(
        r#"{
        "port": 8080,
        "proxy": {
            "/api": {
                "targets": ["http://b:4000"],
                "rateLimit": { "windowSecs": 60, "limit": 10, "keyBy": "ip" }
            }
        }
    }"#,
    );
    if let Some(conduit::config::schema::ProxyConfig::Routes(routes)) = &cfg.sites[0].proxy {
        if let Some(conduit::config::schema::ProxyRouteTarget::Full(route)) = routes.get("/api") {
            let rl = route.rate_limit.as_ref().expect("rateLimit present");
            assert_eq!(rl.limit, 10);
            assert_eq!(rl.window_secs, 60);
        } else {
            panic!("expected Full route");
        }
    } else {
        panic!("expected Routes proxy");
    }
}

#[test]
fn retry_budget_percent_parses() {
    let cfg = parse(
        r#"{
        "port": 8080,
        "proxy": {
            "/api": {
                "targets": ["http://b:4000"],
                "retry": { "attempts": 3, "conditions": ["5xx"], "budgetPercent": 20.0 }
            }
        }
    }"#,
    );
    if let Some(conduit::config::schema::ProxyConfig::Routes(routes)) = &cfg.sites[0].proxy {
        if let Some(conduit::config::schema::ProxyRouteTarget::Full(route)) = routes.get("/api") {
            let retry = route.retry.as_ref().expect("retry present");
            assert_eq!(retry.budget_percent, Some(20.0));
        } else {
            panic!("expected Full route");
        }
    } else {
        panic!("expected Routes proxy");
    }
}

#[test]
fn admin_token_parses() {
    let cfg = parse(
        r#"{
        "global": { "admin": { "bind": "127.0.0.1:2019", "token": "secret" } },
        "sites": [{ "port": 8080 }]
    }"#,
    );
    let token = cfg
        .global
        .as_ref()
        .unwrap()
        .admin
        .as_ref()
        .unwrap()
        .token
        .as_deref();
    assert_eq!(token, Some("secret"));
}

#[test]
fn logging_skip_paths_parses() {
    let cfg = parse(
        r#"{
        "port": 8080,
        "logging": { "format": "json", "skipPaths": ["/__health__", "/metrics"] }
    }"#,
    );
    if let Some(LoggingConfig::Options(opts)) = &cfg.sites[0].logging {
        let skip = opts.skip_paths.as_ref().expect("skipPaths present");
        assert!(skip.contains(&"/__health__".to_string()));
    } else {
        panic!("expected LoggingConfig::Options");
    }
}

#[test]
fn mirror_url_parses() {
    let cfg = parse(
        r#"{
        "port": 8080,
        "proxy": {
            "/api": {
                "targets": ["http://primary:4000"],
                "mirror": "http://shadow:4000"
            }
        }
    }"#,
    );
    if let Some(conduit::config::schema::ProxyConfig::Routes(routes)) = &cfg.sites[0].proxy {
        if let Some(conduit::config::schema::ProxyRouteTarget::Full(route)) = routes.get("/api") {
            assert_eq!(route.mirror.as_deref(), Some("http://shadow:4000"));
        } else {
            panic!("expected Full route");
        }
    } else {
        panic!("expected Routes proxy");
    }
}
