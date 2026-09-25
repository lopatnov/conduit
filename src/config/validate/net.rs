//! Validation of `ipFilter` and `cors`.

use super::ValidationError;

use crate::config::schema::{CorsConfig, IpFilterConfig};

/// Reject `cors.credentials: true` unless `cors.origins` is an explicit,
/// non-wildcard allowlist.
///
/// `crate::filter::cors::build_response_headers` echoes the request's
/// `Origin` header back verbatim and sets
/// `Access-Control-Allow-Credentials: true` whenever `credentials` is set —
/// regardless of whether `origins` actually restricts anything. With
/// `origins` unset (defaults to "any origin") or containing `"*"`, this lets
/// *any* website make credentialed (cookie-bearing) cross-origin requests
/// and read the response — a real CSRF/data-exfiltration vector, not just a
/// spec nicety (browsers themselves only refuse the literal combination of
/// wildcard `Access-Control-Allow-Origin: *` with
/// `Access-Control-Allow-Credentials: true`; echoing the origin back
/// bypasses that browser-side guard entirely). Fail at config-load time
/// rather than silently downgrading to a safe response at runtime, matching
/// this codebase's convention for configs that can't be honestly satisfied
/// (see issue #189's `tls.versions`/`tls.ciphers` rejection).
pub(super) fn validate_cors(cors: &CorsConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    let CorsConfig::Options(opts) = cors else {
        return;
    };
    if opts.credentials != Some(true) {
        return;
    }
    let has_explicit_allowlist = opts
        .origins
        .as_ref()
        .is_some_and(|list| !list.is_empty() && !list.iter().any(|o| o == "*"));
    if !has_explicit_allowlist {
        errors.push(ValidationError::new(
            format!("{prefix}.cors.credentials"),
            "cors.credentials: true requires cors.origins to be an explicit, \
             non-wildcard list of allowed origins — without it, every origin's \
             credentialed cross-origin requests would be allowed"
                .to_owned(),
        ));
    }
}

pub(super) fn validate_ip_filter(
    cfg: &IpFilterConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
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
