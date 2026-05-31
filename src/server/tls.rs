use std::sync::Arc;

use pingora_core::listeners::tls::TlsSettings;
// pingora-core re-exports pingora-rustls as `tls` when the rustls feature is enabled.
use pingora_core::tls::{ClientCertVerifier, RootCertStore, WebPkiClientVerifier};

use crate::config::schema::TlsClientAuth;

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

/// Build a Pingora [`TlsSettings`] with mTLS client certificate verification.
///
/// Loads the CA certificate from `client_auth.ca` (PEM) and builds a
/// `WebPkiClientVerifier` that Pingora uses for every incoming TLS connection.
///
/// When `client_auth.optional` is `true`, client certificates are requested
/// but not required (equivalent to nginx `ssl_verify_client optional`).
pub fn make_tls_settings_with_client_auth(
    cert_path: &str,
    key_path: &str,
    enable_h2: bool,
    client_auth: &TlsClientAuth,
) -> anyhow::Result<TlsSettings> {
    let mut settings = make_tls_settings(cert_path, key_path, enable_h2)?;

    let verifier = build_client_verifier(&client_auth.ca, client_auth.optional)
        .map_err(|e| anyhow::anyhow!("failed to load mTLS CA from {}: {e}", client_auth.ca))?;

    settings.set_client_cert_verifier(verifier);
    tracing::info!(
        ca = %client_auth.ca,
        optional = client_auth.optional,
        "mTLS client certificate verification enabled"
    );
    Ok(settings)
}

/// Load CA certificates from a PEM file and build a `WebPkiClientVerifier`.
fn build_client_verifier(
    ca_path: &str,
    optional: bool,
) -> anyhow::Result<Arc<dyn ClientCertVerifier>> {
    let mut root_store = RootCertStore::empty();

    // pingora_core::tls::load_ca_file_into_store loads all CA certs from a PEM file.
    pingora_core::tls::load_ca_file_into_store(ca_path, &mut root_store)
        .map_err(|e| anyhow::anyhow!("failed to load CA certs from {ca_path}: {e}"))?;

    if root_store.is_empty() {
        anyhow::bail!("no CA certificates found in {ca_path}");
    }

    let root_store = Arc::new(root_store);
    let verifier: Arc<dyn ClientCertVerifier> = if optional {
        // Request cert but do not require it (nginx ssl_verify_client optional).
        // `.allow_unauthenticated()` makes the certificate optional.
        WebPkiClientVerifier::builder(root_store)
            .allow_unauthenticated()
            .build()
            .map_err(|e| anyhow::anyhow!("build optional client verifier: {e}"))?
    } else {
        // Require a valid client certificate — reject the handshake otherwise.
        WebPkiClientVerifier::builder(root_store)
            .build()
            .map_err(|e| anyhow::anyhow!("build required client verifier: {e}"))?
    };

    Ok(verifier)
}
