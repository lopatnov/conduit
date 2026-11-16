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
    /// Most connections open on this site at once; a connection over it is closed at accept, before any upstream
    /// connect (default: no limit).
    #[serde(rename = "maxConnections", skip_serializing_if = "Option::is_none")]
    pub max_connections: Option<u32>,
    /// Most connections open at once from one client address (default: no limit). IPv6 clients are counted per /64,
    /// an IPv4-mapped IPv6 address as the IPv4 address it carries.
    #[serde(
        rename = "maxConnectionsPerIp",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_connections_per_ip: Option<u32>,
    /// Seconds a connection may be quiet, in both directions, before it is closed (default: 3600; `0` = never).
    #[serde(rename = "idleTimeoutSecs", skip_serializing_if = "Option::is_none")]
    pub idle_timeout_secs: Option<u64>,
}
