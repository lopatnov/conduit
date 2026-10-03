use serde::{Deserialize, Serialize};

/// `false` | `true` | `{ "hstsMaxAgeSecs": 31536000, ... }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum SecurityHeadersConfig {
    Enabled(bool),
    Options(SecurityHeadersOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SecurityHeadersOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hsts_max_age_secs: Option<u64>,
    /// Add `includeSubDomains` to the HSTS header (default: `true` when `hstsMaxAgeSecs` is set).
    #[serde(
        rename = "hstsIncludeSubDomains",
        skip_serializing_if = "Option::is_none"
    )]
    pub hsts_include_subdomains: Option<bool>,
    /// Add `preload` directive to the HSTS header for submission to the preload list.
    #[serde(rename = "hstsPreload", skip_serializing_if = "Option::is_none")]
    pub hsts_preload: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub csp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x_frame_options: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referrer_policy: Option<String>,
    /// `Permissions-Policy` header value controlling browser feature access.
    ///
    /// Replaces the deprecated `Feature-Policy` header. Example:
    /// `"geolocation=(), microphone=(), camera=()"` — deny all device access.
    #[serde(rename = "permissionsPolicy", skip_serializing_if = "Option::is_none")]
    pub permissions_policy: Option<String>,
    /// List of allowed `Host` header values.
    ///
    /// When set, requests with a `Host` not in this list are rejected with
    /// `400 Bad Request`. Protects against HTTP Host header injection attacks
    /// where an application generates absolute URLs from an untrusted `Host`.
    ///
    /// Pattern from traefik `AllowedHosts`. Use `*` to allow any host.
    #[serde(rename = "allowedHosts", skip_serializing_if = "Option::is_none")]
    pub allowed_hosts: Option<Vec<String>>,
}
