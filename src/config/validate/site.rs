//! Per-site orchestration: `validate_site` and the `tcp` site checks.

use super::ValidationError;

use conduit_cors::validate::validate_cors;
use conduit_ipfilter::validate::validate_ip_filter;
use conduit_limits::validate::validate_limits;
use conduit_metrics::validate::validate_metrics;
use conduit_middleware::validate::validate_middleware;
use conduit_ratelimit::validate::validate_rate_limit;
use conduit_redirects::validate::validate_redirect_rules;
use conduit_static::validate::validate_fallback;
use conduit_tcp::validate::validate_tcp;
use conduit_upload::validate::validate_upload;

use super::auth::{validate_api_key, validate_consumers, validate_forward_auth, validate_jwt_auth};
use super::proxy::{validate_proxy, validate_route_config};
use super::tls::validate_tls;

use crate::config::schema::{ProxyRouteTarget, SiteConfig, TcpConfig};

pub(super) fn validate_site(site: &SiteConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    validate_site_transport(site, prefix, errors);
    validate_site_routing(site, prefix, errors);
    validate_site_request_handling(site, prefix, errors);
    validate_site_auth(site, prefix, errors);
    validate_site_limits(site, prefix, errors);
}

/// `tls` / `tcp`.
fn validate_site_transport(site: &SiteConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    if let Some(tls) = &site.tls {
        validate_tls(tls, &format!("{prefix}.tls"), errors);
    }
    if let Some(tcp) = &site.tcp {
        validate_tcp_site(tcp, site, prefix, errors);
    }
}

/// `proxy` / `routes`.
fn validate_site_routing(site: &SiteConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    if let Some(proxy) = &site.proxy {
        validate_proxy(proxy, &format!("{prefix}.proxy"), errors);
    }
    validate_site_routes(site, prefix, errors);
}

fn validate_site_routes(site: &SiteConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    let Some(routes) = &site.routes else {
        return;
    };
    for (i, route) in routes.iter().enumerate() {
        if let Some(ProxyRouteTarget::Full(cfg)) = &route.proxy {
            validate_route_config(cfg, &format!("{prefix}.routes[{i}].proxy"), errors);
        }
    }
}

/// `ipFilter` / `upload` / `metrics` / `rateLimit` / `redirects` / `fallback` / `middleware`.
fn validate_site_request_handling(
    site: &SiteConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if let Some(ip_filter) = &site.ip_filter {
        validate_ip_filter(ip_filter, prefix, errors);
    }
    if let Some(upload) = &site.upload {
        validate_upload(upload, prefix, errors);
    }
    if let Some(metrics) = &site.metrics {
        validate_metrics(metrics, prefix, errors);
    }
    if let Some(rate_limit) = &site.rate_limit {
        validate_rate_limit(rate_limit, prefix, errors);
    }
    if let Some(redirects) = &site.redirects {
        validate_redirect_rules(redirects, prefix, errors);
    }
    if let Some(fb) = &site.fallback {
        validate_fallback(fb, prefix, errors);
    }
    if let Some(middleware) = &site.middleware {
        validate_middleware(middleware, prefix, errors);
    }
    if let Some(cors) = &site.cors {
        validate_cors(cors, prefix, errors);
    }
}

/// `apiKey` / `jwtAuth` / `forwardAuth` / `consumers`.
fn validate_site_auth(site: &SiteConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    if let Some(api_key_cfg) = &site.api_key {
        validate_api_key(api_key_cfg, prefix, errors);
    }
    if let Some(jwt) = &site.jwt_auth {
        validate_jwt_auth(jwt, &format!("{prefix}.jwtAuth"), errors);
    }
    if let Some(fa) = &site.forward_auth {
        validate_forward_auth(fa, &format!("{prefix}.forwardAuth"), errors);
    }
    if let Some(c) = &site.consumers {
        validate_consumers(c, &format!("{prefix}.consumers"), errors);
    }
}

/// `limits`.
fn validate_site_limits(site: &SiteConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    if let Some(ref limits) = site.limits {
        validate_limits(limits, &format!("{prefix}.limits"), errors);
    }
}

/// Validate a TCP proxy site configuration.
fn validate_tcp_site(
    tcp: &TcpConfig,
    site: &SiteConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    validate_tcp(tcp, prefix, errors);
    // TCP sites cannot be combined with HTTP features.
    if site.proxy.is_some() {
        errors.push(ValidationError::new(
            format!("{prefix}.tcp"),
            "tcp cannot be combined with proxy on the same site",
        ));
    }
    if site.static_files.is_some() {
        errors.push(ValidationError::new(
            format!("{prefix}.tcp"),
            "tcp cannot be combined with static on the same site",
        ));
    }
}
