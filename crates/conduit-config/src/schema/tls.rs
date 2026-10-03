//! TLS listener config: `TlsConfig`, mTLS `TlsClientAuth`, ACME re-export, `Http2Config`.

use serde::{Deserialize, Serialize};

// ── TLS ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TlsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_redirect_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub versions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ciphers: Option<Vec<String>>,
    // Auto-TLS via Let's Encrypt (Phase 3.1)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acme: Option<AcmeConfig>,
    /// Mutual TLS — require and verify client certificates.
    ///
    /// When set, every TLS connection must present a certificate signed by
    /// the configured CA.  Clients without a valid certificate are rejected
    /// at the TLS handshake (before any HTTP processing).
    ///
    /// ```yaml
    /// tls:
    ///   cert: ./server.crt
    ///   key:  ./server.key
    ///   clientAuth:
    ///     ca: ./ca.crt       # PEM file containing the CA that signs client certs
    ///     optional: false    # true = request cert but don't require it
    /// ```
    #[serde(rename = "clientAuth", skip_serializing_if = "Option::is_none")]
    pub client_auth: Option<TlsClientAuth>,
}

/// mTLS client certificate verification configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TlsClientAuth {
    /// Path to the CA certificate file (PEM format) used to verify client certs.
    pub ca: String,
    /// When `true`, client certificates are requested but not required
    /// (equivalent to nginx `ssl_verify_client optional`).
    /// When `false` (default), clients without a valid cert are rejected.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

// Extracted into crates/conduit-acme (#114/#130) — always compiled (like
// `conduit_otlp::OtlpConfig`) so `tls.acme` stays parseable in every build.
pub use conduit_acme::AcmeConfig;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Http2Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_streams: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_window_size: Option<u32>,
    /// Allow HTTP/2 upgrade on plaintext (cleartext) connections — h2c.
    ///
    /// When `true`, a client connecting on a plain HTTP port can negotiate
    /// HTTP/2 without TLS.  Useful for internal gRPC traffic or when TLS is
    /// handled by an upstream load-balancer.
    ///
    /// **Does not affect TLS ports** — those always negotiate HTTP/2 via ALPN.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h2c: Option<bool>,
}
