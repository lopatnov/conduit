//! Per-site orchestration: `validate_site` and the `tcp` site checks.

use super::ValidationError;

use conduit_auth_consumers::validate::validate_consumers;
use conduit_auth_forward::validate::{validate_forward_auth, AdminEndpoint};
use conduit_auth_jwt::validate::validate_jwt_auth;
use conduit_cors::validate::validate_cors;
use conduit_ipfilter::validate::validate_ip_filter;
use conduit_limits::validate::validate_limits;
use conduit_metrics::validate::validate_metrics;
use conduit_middleware::validate::validate_middleware;
use conduit_proxy_http::validate::{validate_proxy, validate_route_config};
use conduit_ratelimit::validate::validate_rate_limit;
use conduit_redirects::validate::validate_redirect_rules;
use conduit_static::validate::validate_fallback;
use conduit_tcp::validate::{validate_proxy_protocol, validate_tcp};
use conduit_upload::validate::validate_upload;

use super::auth::validate_api_key;
use super::tls::{validate_acme_host, validate_tls};

use crate::config::schema::{ProxyRouteTarget, SiteConfig, TcpConfig};

/// `admin`: where the Admin API listens, which a forwardAuth URL must not point at.
pub(super) fn validate_site(
    site: &SiteConfig,
    admin: &AdminEndpoint,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    validate_site_transport(site, prefix, errors);
    validate_site_routing(site, prefix, errors);
    validate_site_request_handling(site, prefix, errors);
    validate_site_auth(site, admin, prefix, errors);
    validate_site_limits(site, prefix, errors);
}

/// `tls` / `tcp`.
fn validate_site_transport(site: &SiteConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    if let Some(tls) = &site.tls {
        validate_tls(tls, &format!("{prefix}.tls"), errors);
        validate_acme_host(site.host.as_deref(), tls, prefix, errors);
    }
    if let Some(tcp) = &site.tcp {
        validate_tcp_site(tcp, site, prefix, errors);
    }
    if let Some(pp) = &site.proxy_protocol {
        validate_proxy_protocol(pp, prefix, errors);
        // HTTP and TLS listeners find their policy by the local port. Port 0 binds an OS-assigned port, so the required
        // header policy would not match (fail open). Plain TCP proxy sites read their policy from the site directly.
        if site.tcp.is_none() && site.port == Some(0) {
            errors.push(ValidationError::new(
                format!("{prefix}.port"),
                "proxyProtocol needs a fixed port: port 0 binds an OS-assigned port, so the PROXY protocol policy cannot match it",
            ));
        }
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
fn validate_site_auth(
    site: &SiteConfig,
    admin: &AdminEndpoint,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if let Some(api_key_cfg) = &site.api_key {
        validate_api_key(api_key_cfg, prefix, errors);
    }
    if let Some(jwt) = &site.jwt_auth {
        validate_jwt_auth(jwt, &format!("{prefix}.jwtAuth"), errors);
    }
    if let Some(fa) = &site.forward_auth {
        validate_forward_auth(fa, admin, &format!("{prefix}.forwardAuth"), errors);
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

/// `tls` on a `tcp` site terminates TLS with a certificate and key (and optionally `clientAuth`). What belongs to an HTTP
/// listener does not apply: `acme` (its HTTP-01 challenge is answered on a plain HTTP port, which this site does not
/// have) and `httpRedirectPort` (there is no HTTP to redirect). Each is refused, so a key that does nothing is never
/// mistaken for one that works (issue #757).
fn validate_tcp_site_tls(site: &SiteConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    let Some(tls) = &site.tls else {
        return;
    };
    if tls.acme.is_some() {
        errors.push(ValidationError::new(
            format!("{prefix}.tls.acme"),
            "tls.acme does not apply to a tcp site: the certificate is ordered over HTTP-01 on a plain HTTP port, which \
             a tcp site does not have. Set tls.cert and tls.key (renew them with a tool of your own and restart)",
        ));
    }
    if tls.http_redirect_port.is_some() {
        errors.push(ValidationError::new(
            format!("{prefix}.tls.httpRedirectPort"),
            "tls.httpRedirectPort does not apply to a tcp site: there is no HTTP to redirect",
        ));
    }
    // A site without a port is on 80 as a tcp site and on 443 as an HTTPS one: a TLS port that depends on which was
    // meant is a guess, so it has to be written down.
    if site.port.is_none() {
        errors.push(ValidationError::new(
            format!("{prefix}.port"),
            "a tcp site with tls needs an explicit port",
        ));
    }
    // The header is read in a hook before the handshake and its policy is found by the local port: port 0 would refuse to
    // start (fail closed), which `validate` should say first.
    if site.port == Some(0) && site.proxy_protocol.is_some() {
        errors.push(ValidationError::new(
            format!("{prefix}.port"),
            "proxyProtocol on a tcp site with tls needs a fixed port: port 0 binds an OS-assigned port, so the PROXY protocol policy cannot match it",
        ));
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
    validate_tcp_site_tls(site, prefix, errors);
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
    // A TCP site relays bytes and runs no filter chain, so an access-control or traffic-control key on it would be
    // silently ignored: an operator who adds `ipFilter` to a database port would believe it is protected. Refuse it.
    let ignored = [
        ("ipFilter", site.ip_filter.is_some()),
        ("rateLimit", site.rate_limit.is_some()),
        ("basicAuth", site.basic_auth.is_some()),
        ("apiKey", site.api_key.is_some()),
        ("jwtAuth", site.jwt_auth.is_some()),
        ("forwardAuth", site.forward_auth.is_some()),
        ("consumers", site.consumers.is_some()),
        ("limits", site.limits.is_some()),
        ("middleware", site.middleware.is_some()),
    ];
    for (key, present) in ignored {
        if present {
            errors.push(ValidationError::new(
                format!("{prefix}.{key}"),
                format!(
                    "{key} does not apply to a tcp site (it relays bytes and runs no HTTP filters): remove it and \
                     restrict who can reach the port with a firewall or network policy"
                ),
            ));
        }
    }
}
