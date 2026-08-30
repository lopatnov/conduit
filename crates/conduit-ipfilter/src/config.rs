use serde::{Deserialize, Serialize};

/// IP allow/deny filtering configuration (`sites[].ipFilter`).
///
/// Applied before authentication and rate limiting (`CLAUDE.md` architectural
/// decision #11). Supports exact IP matches and CIDR blocks in both `allow`
/// and `deny`; when `allow` is set the site runs in whitelist mode, otherwise
/// `deny` runs in blacklist mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct IpFilterConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deny: Option<Vec<String>>,
    /// When `true`, read the client IP from `X-Forwarded-For` instead of the
    /// TCP connection address. Only enable when Conduit is behind a trusted
    /// reverse proxy that sets this header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_proxy: Option<bool>,
    /// Dry-run mode — log blocked IPs but allow requests through.
    ///
    /// **nginx `ngx_http_limit_conn_module` dry_run pattern.** When `true`,
    /// requests from denied IPs (or outside the allowlist) are logged as
    /// warnings but forwarded. Safe rollout: enable dry-run first, review
    /// logs, then disable dry-run to enforce.
    #[serde(rename = "dryRun", skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
}
