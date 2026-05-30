use pingora_core::listeners::tls::TlsSettings;

// ── SNI note ──────────────────────────────────────────────────────────────────
//
// Multi-certificate SNI (serving a different cert per hostname on the same port)
// is not currently supported.  Pingora's rustls `TlsSettings` calls
// `ServerConfig::with_single_cert` internally, and the rustls feature flag
// explicitly disables `with_callbacks()` with the message "Certificate callbacks
// are not supported with feature 'rustls'."
//
// Consequence: when multiple HTTPS sites share a port, only the first
// `tls.cert` / `tls.key` pair registered for that port is used.  All sites on
// the port receive the same certificate regardless of the SNI hostname the
// client sends.
//
// Future path: use Pingora's boringssl/openssl backend (build feature
// `openssl_derived`) which does support `TlsAcceptCallbacks`, or wait for
// Pingora to expose a `ResolvesServerCert` API for its rustls backend.

/// Build a Pingora [`TlsSettings`] from explicit certificate and key file paths.
///
/// Set `enable_h2` to `true` to negotiate HTTP/2 via ALPN (in addition to HTTP/1.1).
pub fn make_tls_settings(
    cert_path: &str,
    key_path: &str,
    enable_h2: bool,
) -> anyhow::Result<TlsSettings> {
    let mut settings = TlsSettings::intermediate(cert_path, key_path)
        .map_err(|e| anyhow::anyhow!("failed to load TLS certificate from {cert_path}: {e}"))?;

    if enable_h2 {
        settings.enable_h2();
    }

    Ok(settings)
}
