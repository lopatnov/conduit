//! Splitting the port off a `Host` header value.

/// The host part of a `Host` header value, without the port: `example.com:8080` → `example.com`,
/// and a bracketed IPv6 literal keeps its brackets — `[::1]:8080` → `[::1]` (the form the header
/// carries, and what `url::Url::host_str()` returns; a site's `host` is compared against it).
/// Cutting at the first `:` used to turn every IPv6 literal into `"["`, so such a request never
/// matched its site by host and all of them shared one cache namespace. An unterminated bracket
/// is malformed and returned as is; whatever follows the closing bracket (a port, or junk) is dropped.
pub fn host_without_port(host: &str) -> &str {
    if host.starts_with('[') {
        return match host.find(']') {
            Some(end) => &host[..=end],
            None => host,
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

    /// Whatever follows the closing bracket is not part of the host, valid port or not.
    #[test]
    fn drops_whatever_follows_the_closing_bracket() {
        assert_eq!(host_without_port("[::1]junk"), "[::1]");
        assert_eq!(host_without_port("[::1]:not-a-port"), "[::1]");
    }
}
