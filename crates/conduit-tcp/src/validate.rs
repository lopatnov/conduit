//! Config validation for `tcp`: called by the root crate's `config::validate`.

use conduit_config_core::validation::ValidationError;

use crate::config::TcpConfig;

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
}
