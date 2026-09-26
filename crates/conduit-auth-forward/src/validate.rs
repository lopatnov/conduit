//! Config validation for `forwardAuth`: called by the root crate's `config::validate`.

// `url` is optional: only the forwardAuth-targets-the-Admin-API check parses a URL, and that check exists only
// with the `forward-auth` feature.
#[cfg(feature = "forward-auth")]
use url::Url as ParsedUrl;

use conduit_config_core::validation::ValidationError;

/// `true` when `url` points at the Conduit Admin API on `admin_port`: a loopback or "this host" address
/// (`localhost`, any `*.localhost`, an IPv4 `127.0.0.0/8` address, `0.0.0.0`, IPv6 `::1` / `::`, or an
/// IPv4-mapped IPv6 one), or the configured `admin_host`. The port defaults to 80/443 for a URL without one,
/// like a real request would.
///
/// Host classification uses the parsed [`url::Host`], not `host_str()`: `host_str()` returns an IPv6 host
/// in brackets (`[::1]`), which a string comparison against `"::1"` never matches (#447).
#[cfg(feature = "forward-auth")]
fn targets_admin_api(url: &str, admin_port: u16, admin_host: Option<&str>) -> bool {
    let Ok(parsed) = ParsedUrl::parse(url) else {
        return false;
    };
    let loopback = match parsed.host() {
        Some(url::Host::Domain(name)) => {
            let name = name.trim_end_matches('.');
            name == "localhost" || name.ends_with(".localhost")
        }
        // `0.0.0.0` / `[::]` are "this host" when connected to (Linux, macOS), so they reach the Admin API too.
        Some(url::Host::Ipv4(addr)) => addr.is_loopback() || addr.is_unspecified(),
        Some(url::Host::Ipv6(addr)) => {
            addr.is_loopback()
                || addr.is_unspecified()
                || addr
                    .to_ipv4_mapped()
                    .is_some_and(|v4| v4.is_loopback() || v4.is_unspecified())
        }
        None => false,
    };
    let same_host = admin_host.is_some_and(|h| {
        parsed.host_str().map(|s| s.trim_matches(['[', ']'])) == Some(h.trim_matches(['[', ']']))
    });
    (loopback || same_host) && parsed.port_or_known_default() == Some(admin_port)
}

/// Reject a forwardAuth URL that targets the Conduit Admin API (`admin_port`: the port of
/// `global.admin.bind`, or the documented default 2019 when none is configured) -- the
/// `forward-auth` variant.
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
    admin_port: u16,
    admin_host: Option<&str>,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if targets_admin_api(&cfg.url, admin_port, admin_host) {
        errors.push(ValidationError::new(
            format!("{prefix}.url"),
            format!(
                "forwardAuth.url points to the Conduit Admin API on port {admin_port}. \
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
    _admin_port: u16,
    _admin_host: Option<&str>,
    _prefix: &str,
    _errors: &mut Vec<ValidationError>,
) {
}

/// Validate a `forwardAuth` block. The Admin API address (see [`check_forward_auth_admin_target`]) is read
/// from `global.admin.bind` by the root crate.
pub fn validate_forward_auth(
    cfg: &crate::config::ForwardAuthConfig,
    admin_port: u16,
    admin_host: Option<&str>,
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

    check_forward_auth_admin_target(cfg, admin_port, admin_host, prefix, errors);
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

    #[test]
    fn the_default_admin_address_is_flagged() {
        assert!(targets_admin_api("http://127.0.0.1:2019/auth", 2019, None));
        assert!(targets_admin_api("http://localhost:2019/auth", 2019, None));
        assert!(targets_admin_api("http://127.1.2.3:2019/auth", 2019, None));
    }

    #[test]
    fn ipv6_loopback_is_flagged_although_host_str_brackets_it() {
        // The #447 case: `host_str()` is "[::1]", so the old `== "::1"` never matched.
        assert!(targets_admin_api("http://[::1]:2019/auth", 2019, None));
        assert!(targets_admin_api(
            "http://[::ffff:127.0.0.1]:2019/auth",
            2019,
            None
        ));
    }

    #[test]
    fn unspecified_addresses_reach_this_host_and_are_flagged() {
        assert!(targets_admin_api("http://0.0.0.0:2019/auth", 2019, None));
        assert!(targets_admin_api("http://[::]:2019/auth", 2019, None));
        assert!(targets_admin_api("http://[::ffff:0.0.0.0]:2019/auth", 2019, None));
    }

    #[test]
    fn localhost_spellings_are_flagged() {
        assert!(targets_admin_api("http://LOCALHOST:2019/", 2019, None));
        assert!(targets_admin_api("http://localhost.:2019/", 2019, None));
        assert!(targets_admin_api("http://admin.localhost:2019/", 2019, None));
    }

    #[test]
    fn the_configured_admin_port_is_protected_and_2019_is_free_again() {
        assert!(targets_admin_api("http://127.0.0.1:3000/auth", 3000, None));
        assert!(!targets_admin_api("http://127.0.0.1:2019/auth", 3000, None));
    }

    #[test]
    fn configured_admin_host_is_flagged() {
        assert!(targets_admin_api(
            "http://192.0.2.10:3000/auth",
            3000,
            Some("192.0.2.10")
        ));
        assert!(targets_admin_api(
            "http://[2001:db8::1]:3000/auth",
            3000,
            Some("[2001:db8::1]")
        ));
        assert!(!targets_admin_api(
            "http://192.0.2.11:3000/auth",
            3000,
            Some("192.0.2.10")
        ));
    }

    #[test]
    fn a_url_without_a_port_uses_the_scheme_default() {
        assert!(targets_admin_api("http://127.0.0.1/auth", 80, None));
        assert!(targets_admin_api("https://127.0.0.1/auth", 443, None));
        assert!(!targets_admin_api("https://127.0.0.1/auth", 80, None));
    }

    #[test]
    fn other_hosts_are_not_flagged() {
        assert!(!targets_admin_api("http://auth-service:2019/verify", 2019, None));
        assert!(!targets_admin_api("http://10.0.0.1:2019/verify", 2019, None));
        assert!(!targets_admin_api("http://[2001:db8::1]:2019/verify", 2019, None));
        // A domain that merely starts with "127." is not loopback (the old `starts_with("127.")` said it was).
        assert!(!targets_admin_api(
            "http://127.example.com:2019/verify",
            2019,
            None
        ));
        assert!(!targets_admin_api("not a url", 2019, None));
    }
}
