//! Config validation for `forwardAuth`: called by the root crate's `config::validate`.

// `url` is optional: only the forwardAuth-targets-the-Admin-API check parses a URL, and that check exists only
// with the `forward-auth` feature.
#[cfg(feature = "forward-auth")]
use url::Url as ParsedUrl;

use conduit_config_core::validation::ValidationError;

/// Where the Admin API listens, as far as the forwardAuth lint needs to know it (built by the root crate from
/// `global.admin.bind`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminEndpoint {
    /// The host of `global.admin.bind` — an IP address (an IPv6 one without brackets) or a name — or `None` when
    /// it is not known. A loopback / "this host" URL is always flagged; this adds the concrete address the Admin
    /// API is bound to, which is what a non-loopback bind (`192.0.2.10:2019`) makes reachable (#470).
    pub host: Option<String>,
    /// The port the Admin API listens on.
    pub port: u16,
}

impl AdminEndpoint {
    /// An endpoint known only by its port (the loopback and "this host" addresses are still flagged).
    pub fn on_port(port: u16) -> Self {
        Self { host: None, port }
    }
}

/// The `host:port` a forwardAuth `url` reaches the Conduit Admin API at, or `None` when it does not.
///
/// It reaches it when its port is the admin port (80/443 for a URL without one, like a real request) and its host
/// is a loopback or "this host" address (`localhost`, any `*.localhost`, an IPv4 `127.0.0.0/8` address, `0.0.0.0`,
/// IPv6 `::1` / `::`, or an IPv4-mapped IPv6 one) — reported as `127.0.0.1:<port>` — or the address the Admin API
/// is bound to (`admin.host`), reported as that address.
///
/// Host classification uses the parsed [`url::Host`], not `host_str()`: `host_str()` returns an IPv6 host
/// in brackets (`[::1]`), which a string comparison against `"::1"` never matches (#447). The bind host is
/// compared as an address when both sides are addresses (so `2001:db8:0::1` equals `2001:db8::1` and an IPv4-mapped
/// form equals its IPv4 one), and case-insensitively, without a trailing dot, when both are names.
#[cfg(feature = "forward-auth")]
fn admin_target(url: &str, admin: &AdminEndpoint) -> Option<String> {
    let parsed = ParsedUrl::parse(url).ok()?;
    if parsed.port_or_known_default() != Some(admin.port) {
        return None;
    }
    let host = parsed.host()?;
    if is_this_host(&host) {
        return Some(format!("127.0.0.1:{}", admin.port));
    }
    let bind = admin.host.as_deref()?;
    is_bind_host(&host, bind).then(|| match bind.parse::<std::net::Ipv6Addr>() {
        Ok(_) => format!("[{bind}]:{}", admin.port),
        Err(_) => format!("{bind}:{}", admin.port),
    })
}

/// A loopback or "this host" address: connecting to it reaches this machine.
#[cfg(feature = "forward-auth")]
fn is_this_host(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(name) => {
            let name = name.trim_end_matches('.');
            name == "localhost" || name.ends_with(".localhost")
        }
        // `0.0.0.0` / `[::]` are "this host" when connected to (Linux, macOS), so they reach the Admin API too.
        url::Host::Ipv4(addr) => addr.is_loopback() || addr.is_unspecified(),
        url::Host::Ipv6(addr) => {
            addr.is_loopback()
                || addr.is_unspecified()
                || addr
                    .to_ipv4_mapped()
                    .is_some_and(|v4| v4.is_loopback() || v4.is_unspecified())
        }
    }
}

/// Whether the URL host is the host the Admin API is bound to (see [`admin_target`]).
#[cfg(feature = "forward-auth")]
fn is_bind_host(host: &url::Host<&str>, bind: &str) -> bool {
    let bind = bind.trim_start_matches('[').trim_end_matches(']');
    let bind_ip = bind.parse::<std::net::IpAddr>().ok();
    match host {
        url::Host::Domain(name) => {
            bind_ip.is_none()
                && name
                    .trim_end_matches('.')
                    .eq_ignore_ascii_case(bind.trim_end_matches('.'))
        }
        url::Host::Ipv4(addr) => {
            bind_ip.is_some_and(|b| b.to_canonical() == std::net::IpAddr::V4(*addr))
        }
        url::Host::Ipv6(addr) => {
            bind_ip.is_some_and(|b| b.to_canonical() == std::net::IpAddr::V6(*addr).to_canonical())
        }
    }
}

/// Reject a forwardAuth URL that targets the Conduit Admin API (`admin`: where `global.admin.bind` puts it, or the
/// documented default 2019 when none is configured) -- the `forward-auth` variant.
///
/// A misconfigured forwardAuth pointing to the admin API would allow an
/// attacker to exploit the proxy's own admin endpoint as the auth server.
/// Compiled only with `forward-auth` (issue #144, PR 4b): without it no
/// forwardAuth subrequest is ever made -- `feature_warnings()` says the whole
/// block is ignored -- so there is nothing for the rule to guard, and the
/// rule itself is unchanged wherever the guard it protects can exist.
#[cfg(feature = "forward-auth")]
fn check_forward_auth_admin_target(
    cfg: &crate::config::ForwardAuthConfig,
    admin: &AdminEndpoint,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if let Some(target) = admin_target(&cfg.url, admin) {
        errors.push(ValidationError::new(
            format!("{prefix}.url"),
            format!(
                "forwardAuth.url points to {target} — this is the Conduit Admin API. \
                 Routing external auth requests through the admin API is a security risk. \
                 Use a dedicated auth service instead."
            ),
        ));
    }
}

/// No-`forward-auth` variant of [`check_forward_auth_admin_target`]: the
/// forwardAuth block is ignored in such a build, so no admin-API target can
/// be exploited (and the URL parsing that needs the `url` crate is compiled out).
#[cfg(not(feature = "forward-auth"))]
fn check_forward_auth_admin_target(
    _cfg: &crate::config::ForwardAuthConfig,
    _admin: &AdminEndpoint,
    _prefix: &str,
    _errors: &mut Vec<ValidationError>,
) {
}

/// Validate a `forwardAuth` block. `admin` is where the Admin API listens (see
/// [`check_forward_auth_admin_target`]); the root crate reads it from `global.admin.bind`.
pub fn validate_forward_auth(
    cfg: &crate::config::ForwardAuthConfig,
    admin: &AdminEndpoint,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if cfg.url.is_empty() {
        errors.push(ValidationError::new(
            format!("{prefix}.url"),
            "forwardAuth.url must not be empty",
        ));
        return;
    } else if !cfg.url.starts_with("http://") && !cfg.url.starts_with("https://") {
        errors.push(ValidationError::new(
            format!("{prefix}.url"),
            "forwardAuth.url must be an http:// or https:// URL",
        ));
        return;
    }

    check_forward_auth_admin_target(cfg, admin, prefix, errors);
    if let Some(0) = cfg.timeout_ms {
        errors.push(ValidationError::new(
            format!("{prefix}.timeoutMs"),
            "forwardAuth.timeoutMs must be > 0",
        ));
    }
}

#[cfg(all(test, feature = "forward-auth"))]
mod tests {
    use super::*;

    /// The loopback rules, for an Admin API known only by its port.
    fn targets_admin_api(url: &str, port: u16) -> bool {
        admin_target(url, &AdminEndpoint::on_port(port)).is_some()
    }

    /// An Admin API bound to `host` on 2019 (`global.admin.bind: "<host>:2019"`).
    fn bound_to(host: &str) -> AdminEndpoint {
        AdminEndpoint {
            host: Some(host.to_owned()),
            port: 2019,
        }
    }

    #[test]
    fn the_default_admin_address_is_flagged() {
        assert!(targets_admin_api("http://127.0.0.1:2019/auth", 2019));
        assert!(targets_admin_api("http://localhost:2019/auth", 2019));
        assert!(targets_admin_api("http://127.1.2.3:2019/auth", 2019));
    }

    #[test]
    fn ipv6_loopback_is_flagged_although_host_str_brackets_it() {
        // The #447 case: `host_str()` is "[::1]", so the old `== "::1"` never matched.
        assert!(targets_admin_api("http://[::1]:2019/auth", 2019));
        assert!(targets_admin_api(
            "http://[::ffff:127.0.0.1]:2019/auth",
            2019
        ));
    }

    #[test]
    fn unspecified_addresses_reach_this_host_and_are_flagged() {
        assert!(targets_admin_api("http://0.0.0.0:2019/auth", 2019));
        assert!(targets_admin_api("http://[::]:2019/auth", 2019));
        assert!(targets_admin_api("http://[::ffff:0.0.0.0]:2019/auth", 2019));
    }

    #[test]
    fn localhost_spellings_are_flagged() {
        assert!(targets_admin_api("http://LOCALHOST:2019/", 2019));
        assert!(targets_admin_api("http://localhost.:2019/", 2019));
        assert!(targets_admin_api("http://admin.localhost:2019/", 2019));
    }

    #[test]
    fn the_configured_admin_port_is_protected_and_2019_is_free_again() {
        assert!(targets_admin_api("http://127.0.0.1:3000/auth", 3000));
        assert!(!targets_admin_api("http://127.0.0.1:2019/auth", 3000));
    }

    #[test]
    fn a_url_without_a_port_uses_the_scheme_default() {
        assert!(targets_admin_api("http://127.0.0.1/auth", 80));
        assert!(targets_admin_api("https://127.0.0.1/auth", 443));
        assert!(!targets_admin_api("https://127.0.0.1/auth", 80));
    }

    #[test]
    fn other_hosts_are_not_flagged() {
        assert!(!targets_admin_api("http://auth-service:2019/verify", 2019));
        assert!(!targets_admin_api("http://10.0.0.1:2019/verify", 2019));
        assert!(!targets_admin_api("http://[2001:db8::1]:2019/verify", 2019));
        // A domain that merely starts with "127." is not loopback (the old `starts_with("127.")` said it was).
        assert!(!targets_admin_api(
            "http://127.example.com:2019/verify",
            2019
        ));
        assert!(!targets_admin_api("not a url", 2019));
    }

    /// #470: a non-loopback `global.admin.bind` makes that very address reach the Admin API.
    #[test]
    fn the_address_the_admin_api_is_bound_to_is_flagged() {
        let admin = bound_to("192.0.2.10");
        assert_eq!(
            admin_target("http://192.0.2.10:2019/auth", &admin).as_deref(),
            Some("192.0.2.10:2019")
        );
        // …but only on the admin port, and only that address
        assert!(admin_target("http://192.0.2.10:3000/auth", &admin).is_none());
        assert!(admin_target("http://192.0.2.11:2019/auth", &admin).is_none());
        // the loopback rules still apply and keep their `127.0.0.1` wording
        assert_eq!(
            admin_target("http://localhost:2019/auth", &admin).as_deref(),
            Some("127.0.0.1:2019")
        );
        // without a known bind host a concrete address is not flagged (the old behaviour)
        assert!(
            admin_target("http://192.0.2.10:2019/auth", &AdminEndpoint::on_port(2019)).is_none()
        );
    }

    #[test]
    fn a_bound_ipv6_address_matches_however_the_url_writes_it() {
        let admin = bound_to("2001:db8::1");
        for url in [
            "http://[2001:db8::1]:2019/auth",
            "http://[2001:db8:0:0:0:0:0:1]:2019/auth",
            "http://[2001:0DB8::1]:2019/auth",
        ] {
            assert_eq!(
                admin_target(url, &admin).as_deref(),
                Some("[2001:db8::1]:2019"),
                "{url}"
            );
        }
        assert!(admin_target("http://[2001:db8::2]:2019/auth", &admin).is_none());
    }

    #[test]
    fn an_ipv4_mapped_url_matches_a_bound_ipv4_address() {
        let admin = bound_to("192.0.2.10");
        assert!(admin_target("http://[::ffff:192.0.2.10]:2019/auth", &admin).is_some());
        assert!(admin_target("http://[::ffff:192.0.2.11]:2019/auth", &admin).is_none());
    }

    #[test]
    fn a_bound_host_name_matches_case_insensitively_and_with_a_trailing_dot() {
        let admin = bound_to("admin.example");
        assert!(admin_target("http://ADMIN.example:2019/auth", &admin).is_some());
        assert!(admin_target("http://admin.example.:2019/auth", &admin).is_some());
        assert!(admin_target("http://other.example:2019/auth", &admin).is_none());
        // a name is never mistaken for an address or the other way round
        assert!(admin_target("http://192.0.2.10:2019/auth", &admin).is_none());
    }
}
