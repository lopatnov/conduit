//! Config validation for `ipFilter`: called by the root crate's `config::validate`.

use crate::config::IpFilterConfig;
use conduit_config_core::validation::ValidationError;

pub fn validate_ip_filter(cfg: &IpFilterConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    for (field, list) in [
        ("allow", cfg.allow.as_deref()),
        ("deny", cfg.deny.as_deref()),
    ] {
        let Some(entries) = list else { continue };
        for (i, entry) in entries.iter().enumerate() {
            if !is_valid_ip_or_cidr(entry) {
                errors.push(ValidationError::new(
                    format!("{prefix}.ipFilter.{field}[{i}]"),
                    format!("Invalid IP address or CIDR block: '{entry}'"),
                ));
            }
        }
    }
}

/// Return `true` when `s` is a valid IPv4, IPv6, or CIDR notation address.
fn is_valid_ip_or_cidr(s: &str) -> bool {
    use std::net::IpAddr;
    if s.contains('/') {
        // CIDR: split on '/' and validate both parts.
        let mut parts = s.splitn(2, '/');
        let addr = parts.next().unwrap_or("");
        let prefix = parts.next().unwrap_or("");
        let Ok(ip) = addr.parse::<IpAddr>() else {
            return false;
        };
        let Ok(prefix_len) = prefix.parse::<u32>() else {
            return false;
        };
        let max_prefix = match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        prefix_len <= max_prefix
    } else {
        s.parse::<IpAddr>().is_ok()
    }
}
