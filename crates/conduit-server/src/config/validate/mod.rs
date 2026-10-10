pub use conduit_config_core::validation::{partition_by_severity, Severity, ValidationError};

mod auth;
mod cross_site;
mod proxy_loop;
mod required_features;
mod site;
mod tls;
mod warnings;

#[cfg(feature = "redis")]
use self::cross_site::check_redis_store_consistency;
use self::cross_site::{
    admin_endpoint, validate_global, validate_http_redirect_ports, validate_no_duplicate_host_port,
};
use self::proxy_loop::check_proxy_loop_warnings;
use self::site::validate_site;
use self::warnings::{
    check_extra_key_warnings, check_global_feature_warnings, check_jwt_secret_warnings,
    check_metrics_auth_warnings, check_per_site_feature_warnings,
};
// The root crate's `src/config/validate/tests.rs` (which stays in the root — see this crate's own doc comment)
// needs this to build its log-injection-protection test inputs; re-exported here rather than making the whole
// `warnings` submodule `pub`.
pub use self::required_features::{compiled_features, required_features, FEATURE_ORDER};
pub use self::warnings::sanitize_for_log;

use crate::config::schema::AppConfig;

// ── Public API ─────────────────────────────────────────────────────────────

pub fn validate(config: &AppConfig) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    validate_no_duplicate_host_port(config, &mut errors);
    validate_http_redirect_ports(config, &mut errors);
    validate_global(config, &mut errors);
    #[cfg(feature = "redis")]
    check_redis_store_consistency(config, &mut errors);

    let admin = admin_endpoint(config);
    for (i, site) in config.sites.iter().enumerate() {
        validate_site(site, &admin, &format!("sites[{i}]"), &mut errors);
    }

    errors
}

/// Return human-readable warnings for config options that require a compile-time
/// feature which is not currently enabled.
///
/// The server still starts — all warnings describe things that will be silently
/// ignored at runtime.  Callers should log each entry with `tracing::warn!`.
///
/// ```text
/// for w in feature_warnings(&config) {
///     tracing::warn!("{w}");
/// }
/// ```
pub fn feature_warnings(config: &AppConfig) -> Vec<String> {
    let mut warnings: Vec<String> = Vec::new();

    check_global_feature_warnings(config, &mut warnings);
    check_per_site_feature_warnings(config, &mut warnings);
    check_proxy_loop_warnings(config, &mut warnings);
    check_jwt_secret_warnings(config, &mut warnings);
    check_metrics_auth_warnings(config, &mut warnings);
    check_extra_key_warnings(config, &mut warnings);

    warnings
}
