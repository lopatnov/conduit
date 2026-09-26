//! Config validation for `cors`: called by the root crate's `config::validate`.

use crate::config::CorsConfig;
use conduit_config_core::validation::ValidationError;

/// Reject `cors.credentials: true` unless `cors.origins` is an explicit,
/// non-wildcard allowlist.
///
/// `crate::cors::build_response_headers` echoes the request's
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
pub fn validate_cors(cors: &CorsConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
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
