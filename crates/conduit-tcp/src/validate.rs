//! Config validation for `tcp`: called by the root crate's `config::validate`.

use conduit_config_core::validation::ValidationError;

use crate::config::{ProxyProtocolConfig, TcpConfig};

/// Validate the `targets` of a TCP proxy site (`tcp: { targets: [..] }`).
///
/// The combination checks that need the whole site (`tcp` next to `proxy` or `static`) stay in the root crate's
/// `config::validate`, which calls this first, so the order of the reported errors is unchanged.
pub fn validate_tcp(tcp: &TcpConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    if tcp.targets.is_empty() {
        errors.push(ValidationError::new(
            format!("{prefix}.tcp.targets"),
            "at least one target is required for a TCP proxy site",
        ));
    }
    // A cap of 0 would refuse every connection: that is a mistake, not "no limit" (omit the field for that).
    for (field, value) in [
        ("maxConnections", tcp.max_connections),
        ("maxConnectionsPerIp", tcp.max_connections_per_ip),
    ] {
        if value == Some(0) {
            errors.push(ValidationError::new(
                format!("{prefix}.tcp.{field}"),
                format!("{field} must be at least 1 (omit it for no limit)"),
            ));
        }
    }
    if let (Some(site), Some(per_ip)) = (tcp.max_connections, tcp.max_connections_per_ip) {
        if per_ip > site {
            errors.push(ValidationError::warning(
                format!("{prefix}.tcp.maxConnectionsPerIp"),
                format!(
                    "maxConnectionsPerIp ({per_ip}) is above maxConnections ({site}), so one client can use the whole \
                     site and the per-client limit never applies"
                ),
            ));
        }
    }
    for (i, t) in tcp.targets.iter().enumerate() {
        // Targets must be "host:port" — no URL scheme.
        if ["http://", "https://", "ws://", "wss://"]
            .iter()
            .any(|scheme| t.starts_with(scheme))
        {
            errors.push(ValidationError::new(
                format!("{prefix}.tcp.targets[{i}]"),
                format!("TCP target \"{t}\" must be a plain host:port — no http:// prefix"),
            ));
        } else {
            // Validate host:port using SocketAddr parsing (handles IPv4 and IPv6).
            let valid = t.parse::<std::net::SocketAddr>().is_ok()
                || t.rsplit_once(':')
                    .map(|(host, port)| {
                        !host.is_empty()
                            && !port.is_empty()
                            && port.chars().all(|c| c.is_ascii_digit())
                    })
                    .unwrap_or(false);
            if !valid {
                errors.push(ValidationError::new(
                    format!("{prefix}.tcp.targets[{i}]"),
                    format!(
                        "TCP target \"{t}\" must include a port, e.g. \"host:3306\" \
                         or \"[::1]:3306\" for IPv6"
                    ),
                ));
            }
        }
    }
}

/// `true` for a rule that covers every client: a zero prefix, or the IPv4-mapped network that holds the whole IPv4 space.
fn trusts_everyone(rule: &str) -> bool {
    let Some((addr, prefix)) = rule.split_once('/') else {
        return false;
    };
    let Ok(prefix) = prefix.parse::<u32>() else {
        return false;
    };
    prefix == 0
        || (prefix <= 96
            && addr
                .parse::<std::net::Ipv6Addr>()
                .is_ok_and(|a| a.to_ipv4_mapped().is_some()))
}

/// Validate `sites[].proxyProtocol` (`prefix` is the site's path, e.g. `sites[0]`).
pub fn validate_proxy_protocol(
    pp: &ProxyProtocolConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    let path = format!("{prefix}.proxyProtocol");
    if pp.trusted_sources.is_empty() {
        errors.push(ValidationError::new(
            format!("{path}.trustedSources"),
            "at least one trusted source is required: the header is believed only from a listed address (use \
             \"0.0.0.0/0\" to trust every client, which lets any of them claim any address)",
        ));
    }
    for (i, rule) in pp.trusted_sources.iter().enumerate() {
        if !conduit_config_core::cidr::is_valid_ip_or_cidr(rule) {
            errors.push(ValidationError::new(
                format!("{path}.trustedSources[{i}]"),
                format!("\"{rule}\" is not an IP address or a CIDR block (e.g. \"10.0.0.0/8\")"),
            ));
        } else if trusts_everyone(rule) {
            errors.push(ValidationError::warning(
                format!("{path}.trustedSources[{i}]"),
                format!(
                    "\"{rule}\" trusts every address, so any client can send a header that names any address \
                     as its own and slip past ipFilter and rate limits: list the load balancers instead"
                ),
            ));
        }
    }
    if pp.timeout_ms == Some(0) {
        errors.push(ValidationError::new(
            format!("{path}.timeoutMs"),
            "timeoutMs must be at least 1 (omit it for the default)",
        ));
    }
    if let Some(n) = pp.max_header_bytes {
        let range = ProxyProtocolConfig::MIN_HEADER_BYTES..=ProxyProtocolConfig::MAX_HEADER_BYTES;
        if !range.contains(&n) {
            errors.push(ValidationError::new(
                format!("{path}.maxHeaderBytes"),
                format!(
                    "maxHeaderBytes must be between {} and {} (the longest v1 header and the longest v2 header)",
                    range.start(),
                    range.end()
                ),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errors_for(targets: &[&str]) -> Vec<ValidationError> {
        let tcp = TcpConfig {
            targets: targets.iter().map(|t| (*t).to_owned()).collect(),
            ..TcpConfig::default()
        };
        let mut errors = Vec::new();
        validate_tcp(&tcp, "sites[0]", &mut errors);
        errors
    }

    fn limit_findings(site: Option<u32>, per_ip: Option<u32>) -> Vec<ValidationError> {
        let tcp = TcpConfig {
            targets: vec!["db:5432".to_owned()],
            max_connections: site,
            max_connections_per_ip: per_ip,
            ..TcpConfig::default()
        };
        let mut errors = Vec::new();
        validate_tcp(&tcp, "sites[0]", &mut errors);
        errors
    }

    #[test]
    fn a_zero_cap_is_a_hard_error_and_a_positive_one_is_clean() {
        use conduit_config_core::validation::Severity;
        for (site, per_ip, path) in [
            (Some(0), None, "sites[0].tcp.maxConnections"),
            (None, Some(0), "sites[0].tcp.maxConnectionsPerIp"),
        ] {
            let e = limit_findings(site, per_ip);
            assert_eq!(e.len(), 1, "{e:?}");
            assert_eq!(e[0].path, path);
            assert_eq!(e[0].severity, Severity::Error);
        }
        assert!(limit_findings(Some(100), Some(10)).is_empty());
        assert!(limit_findings(None, None).is_empty());
        assert!(limit_findings(Some(10), Some(10)).is_empty());
    }

    #[test]
    fn a_per_client_cap_above_the_site_cap_is_a_warning() {
        use conduit_config_core::validation::Severity;
        let e = limit_findings(Some(10), Some(20));
        assert_eq!(e.len(), 1, "{e:?}");
        assert_eq!(e[0].severity, Severity::Warning);
        assert!(e[0].message.contains("never applies"), "{e:?}");
    }

    #[test]
    fn a_url_scheme_on_a_tcp_target_is_refused() {
        for target in [
            "http://db:5432",
            "https://db:5432",
            "ws://db:5432",
            "wss://db:5432",
        ] {
            let errors = errors_for(&[target]);
            assert_eq!(errors.len(), 1, "{target}");
            assert!(errors[0].message.contains("plain host:port"), "{target}");
        }
        assert!(errors_for(&["db:5432"]).is_empty());
    }

    fn pp(sources: &[&str], timeout: Option<u64>, max: Option<usize>) -> Vec<ValidationError> {
        let cfg = ProxyProtocolConfig {
            trusted_sources: sources.iter().map(|s| (*s).to_owned()).collect(),
            timeout_ms: timeout,
            max_header_bytes: max,
        };
        let mut errors = Vec::new();
        validate_proxy_protocol(&cfg, "sites[2]", &mut errors);
        errors
    }

    #[test]
    fn proxy_protocol_needs_trusted_sources_that_parse() {
        use conduit_config_core::validation::Severity;
        assert!(pp(&["10.0.0.0/8", "2001:db8::1", "192.0.2.7"], None, None).is_empty());
        let e = pp(&[], None, None);
        assert_eq!(e.len(), 1, "{e:?}");
        assert_eq!(e[0].path, "sites[2].proxyProtocol.trustedSources");
        assert_eq!(e[0].severity, Severity::Error);
        let e = pp(&["10.0.0.0/8", "lb.example.com", "10.0.0.0/33"], None, None);
        assert_eq!(e.len(), 2, "{e:?}");
        assert_eq!(e[0].path, "sites[2].proxyProtocol.trustedSources[1]");
        assert_eq!(e[1].path, "sites[2].proxyProtocol.trustedSources[2]");
    }

    #[test]
    fn trusting_everyone_is_a_warning() {
        use conduit_config_core::validation::Severity;
        // the prefix is read as a number, so spellings of zero count too
        for rule in [
            "0.0.0.0/0",
            "::/0",
            "0.0.0.0/00",
            "::/+0",
            "::ffff:0.0.0.0/96",
        ] {
            let e = pp(&[rule], None, None);
            assert_eq!(e.len(), 1, "{e:?}");
            assert_eq!(e[0].severity, Severity::Warning);
            assert!(e[0].message.contains("any client"), "{}", e[0].message);
        }
    }

    #[test]
    fn proxy_protocol_bounds() {
        assert!(pp(&["10.0.0.1"], Some(1), Some(107)).is_empty());
        assert!(pp(&["10.0.0.1"], None, Some(65551)).is_empty());
        assert_eq!(
            pp(&["10.0.0.1"], Some(0), None)[0].path,
            "sites[2].proxyProtocol.timeoutMs"
        );
        for bad in [0, 106, 65552] {
            let e = pp(&["10.0.0.1"], None, Some(bad));
            assert_eq!(e.len(), 1, "{bad}: {e:?}");
            assert_eq!(e[0].path, "sites[2].proxyProtocol.maxHeaderBytes");
        }
    }

    #[test]
    fn a_peer_is_trusted_by_address_or_network() {
        let cfg = ProxyProtocolConfig {
            trusted_sources: vec!["10.0.0.0/8".into(), "2001:db8::1".into()],
            ..ProxyProtocolConfig::default()
        };
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        assert!(cfg.trusts(&ip("10.1.2.3")));
        assert!(cfg.trusts(&ip("::ffff:10.1.2.3")));
        assert!(cfg.trusts(&ip("2001:db8::1")));
        assert!(!cfg.trusts(&ip("11.0.0.1")));
        assert!(!cfg.trusts(&ip("2001:db8::2")));
        assert!(!ProxyProtocolConfig::default().trusts(&ip("10.1.2.3")));
    }
}
