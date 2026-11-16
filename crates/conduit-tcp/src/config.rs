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
    /// Write a PROXY protocol header (`"v1"` text or `"v2"` binary) to the upstream before any client byte, so that
    /// the upstream sees the real client address (MySQL/PostgreSQL behind HAProxy-style ACLs, Postfix, ...).
    /// The upstream must expect the header: one that does not will read it as garbage (default: none).
    #[serde(rename = "sendProxyProtocol", skip_serializing_if = "Option::is_none")]
    pub send_proxy_protocol: Option<crate::proxy_protocol::Version>,
}

/// `sites[].proxyProtocol`: accept the PROXY protocol (v1 and v2) on this site's port.
///
/// A load balancer in front of Conduit (HAProxy, an AWS NLB, nginx `proxy_protocol on`) sends a header with the real
/// client address at the start of each connection. With this block Conduit reads it, and the client address it
/// carries replaces the address of the socket for everything after it: `ipFilter`, rate limits, connection caps,
/// access logs and `X-Forwarded-For`.
///
/// The header is **required** and is believed only from `trustedSources`: a connection from any other address, or from a
/// trusted one that does not start with a valid header, is closed before a byte of it is read as payload (TLS
/// handshake, HTTP, or relay). That also means the load balancer's own health checks must send the header too (or be
/// answered on another port).
///
/// Works on `tcp` sites and on TLS sites (the header is read before the TLS handshake). A site without `tls` is not
/// supported yet and is refused at validation.
///
/// ```yaml
/// sites:
///   - port: 443
///     tls: { cert: cert.pem, key: key.pem }
///     proxyProtocol:
///       trustedSources: ["10.0.0.0/8"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProxyProtocolConfig {
    /// Addresses or CIDRs of the load balancers whose header is believed, e.g. `["10.0.0.0/8", "2001:db8::1"]`.
    /// At least one is required; `0.0.0.0/0` trusts everyone, which lets any client claim any address.
    #[serde(default)]
    pub trusted_sources: Vec<String>,
    /// Milliseconds a trusted peer has to send the whole header (default: 5000). One deadline for the whole header:
    /// a client that sends a byte at a time does not extend it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Longest header accepted, in bytes (default: 4096; 107 to 65551). A v2 header carries TLVs, which cloud load
    /// balancers fill with their own ids; the TLVs are checked and ignored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_header_bytes: Option<usize>,
}

impl ProxyProtocolConfig {
    /// The default for `timeoutMs`.
    pub const DEFAULT_TIMEOUT_MS: u64 = 5000;
    /// The least `maxHeaderBytes`: the longest v1 header.
    pub const MIN_HEADER_BYTES: usize = crate::proxy_protocol::V1_MAX_LEN;
    /// The most `maxHeaderBytes`: a v2 header with the largest length its 16-bit field can declare.
    pub const MAX_HEADER_BYTES: usize = 16 + 65535;

    /// `true` when the header of a connection from `peer` is believed.
    pub fn trusts(&self, peer: &std::net::IpAddr) -> bool {
        self.trusted_sources
            .iter()
            .any(|rule| conduit_config_core::cidr::matches_rule(peer, rule))
    }

    /// The read deadline for the header.
    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.timeout_ms.unwrap_or(Self::DEFAULT_TIMEOUT_MS))
    }

    /// The cap on the header length.
    pub fn header_cap(&self) -> usize {
        self.max_header_bytes
            .unwrap_or(crate::proxy_protocol::DEFAULT_MAX_HEADER_BYTES)
    }
}
