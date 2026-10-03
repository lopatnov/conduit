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
    for (i, t) in tcp.targets.iter().enumerate() {
        // Targets must be "host:port" — no http:// prefix.
        if t.starts_with("http://") || t.starts_with("https://") {
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
