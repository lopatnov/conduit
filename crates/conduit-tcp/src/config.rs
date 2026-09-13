use serde::{Deserialize, Serialize};

/// Raw TCP proxy configuration.
///
/// Proxies a raw TCP connection to one of the specified upstream addresses.
/// No HTTP parsing — bytes are forwarded as-is in both directions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TcpConfig {
    /// Upstream addresses to forward connections to, e.g. `["mysql:3306"]`.
    /// Plain `host:port` strings — no `http://` prefix.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Load balancing strategy: `"round-robin"` (default) or `"random"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    /// Connection timeout to upstream in milliseconds (default: 5000).
    #[serde(rename = "connectTimeoutMs", skip_serializing_if = "Option::is_none")]
    pub connect_timeout_ms: Option<u64>,
}
