//! Splitting the port off a `Host` header value.

/// The host part of a `Host` header value, without the port: `example.com:8080` → `example.com`,
/// and a bracketed IPv6 literal keeps its brackets — `[::1]:8080` → `[::1]` (the form the header
/// carries, and what `url::Url::host_str()` returns; a site's `host` is compared against it).
/// Cutting at the first `:` used to turn every IPv6 literal into `"["`, so such a request never
/// matched its site by host and all of them shared one cache namespace.
///
/// After a closing bracket only nothing or a `:port` may follow. Anything else — `[::1]junk`,
/// `[::1]@evil.com` — is malformed, and so is an unterminated bracket: both are returned as is, so
/// they match no site and no `allowedHosts` entry instead of passing as the literal in front of them.
/// What follows the `:` must be a port — ASCII digits or nothing (RFC 3986 §3.2.3), for a bracketed
/// literal or a plain host alike. `example.com:80@evil.com` and `[::1]:80@evil.com` are returned as
/// is, so they match no site and no `allowedHosts` entry; cutting them at the first `:` would let
/// them pass as `example.com` while the header itself, which is later echoed into
/// `X-Forwarded-Host`, names another host (issue #474).
pub fn host_without_port(host: &str) -> &str {
    let is_port = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
    if host.starts_with('[') {
        return match host.find(']') {
            Some(end) if host[end + 1..].is_empty() => &host[..=end],
            Some(end) if host[end + 1..].strip_prefix(':').is_some_and(is_port) => &host[..=end],
            _ => host,
        };
    }
    match host.split_once(':') {
        None => host,
        Some((name, port)) if is_port(port) => name,
        Some(_) => host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_port() {
        assert_eq!(host_without_port("example.com"), "example.com");
        assert_eq!(host_without_port("example.com:8080"), "example.com");
        assert_eq!(host_without_port("127.0.0.1:80"), "127.0.0.1");
        assert_eq!(host_without_port(""), "");
    }

    /// The bug: `split(':').next()` cut every bracketed IPv6 literal down to `"["`.
    #[test]
    fn keeps_a_bracketed_ipv6_literal_whole() {
        assert_eq!(host_without_port("[::1]:8080"), "[::1]");
        assert_eq!(host_without_port("[::1]"), "[::1]");
        assert_eq!(host_without_port("[2001:db8::1]:443"), "[2001:db8::1]");
    }

    #[test]
    fn leaves_an_unterminated_bracket_alone() {
        assert_eq!(host_without_port("[::1"), "[::1");
    }

    #[test]
    fn keeps_ipv4_mapped_and_zone_id_literals_whole() {
        assert_eq!(
            host_without_port("[::ffff:127.0.0.1]:80"),
            "[::ffff:127.0.0.1]"
        );
        assert_eq!(host_without_port("[fe80::1%25eth0]:80"), "[fe80::1%25eth0]");
    }

    /// Only nothing or `:port` may follow the closing bracket. Treating `[::1]@evil.com` as `[::1]`
    /// would let it pass an `allowedHosts` entry for the literal while the header itself — the
    /// value later echoed into `X-Forwarded-Host` — names another host.
    #[test]
    fn does_not_let_junk_after_the_closing_bracket_pass_as_the_literal() {
        for junk in ["[::1]junk", "[::1]@evil.com", "[::1].evil.com", "[::1] :80"] {
            assert_eq!(host_without_port(junk), junk);
        }
    }

    /// An empty port is legal (RFC 3986 §3.2.3); anything that is not digits is not a port.
    #[test]
    fn accepts_an_empty_port_and_rejects_a_non_numeric_one() {
        assert_eq!(host_without_port("[::1]:"), "[::1]");
        assert_eq!(host_without_port("example.com:"), "example.com");
        for bad in ["[::1]:not-a-port", "example.com:junk", "example.com:80a"] {
            assert_eq!(host_without_port(bad), bad);
        }
    }

    /// Issue #474: text after the port must not let a foreign host pass as the allowed one.
    #[test]
    fn does_not_let_userinfo_after_the_port_pass_as_the_host() {
        for bad in [
            "allowed.example:80@evil.com",
            "[::1]:80@evil.com",
            "example.com:80:81",
            "example.com:80/evil",
        ] {
            assert_eq!(host_without_port(bad), bad);
        }
    }
}
