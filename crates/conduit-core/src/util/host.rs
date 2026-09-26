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
/// (What comes after the `:` is not checked here; the strict `Host` validation guard does that.)
pub fn host_without_port(host: &str) -> &str {
    if host.starts_with('[') {
        return match host.find(']') {
            Some(end) if host[end + 1..].is_empty() || host[end + 1..].starts_with(':') => {
                &host[..=end]
            }
            _ => host,
        };
    }
    host.split(':').next().unwrap_or(host)
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

    /// The port itself is not validated here (`example.com:junk` is `example.com` as well).
    #[test]
    fn does_not_validate_the_port() {
        assert_eq!(host_without_port("[::1]:not-a-port"), "[::1]");
        assert_eq!(host_without_port("[::1]:"), "[::1]");
    }
}
