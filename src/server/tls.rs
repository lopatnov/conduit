use std::io::BufReader;
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

/// Validate a cert+key PEM pair without touching the disk.
///
/// Parses both PEM strings and attempts to build a `rustls::ServerConfig` from
/// them.  Returns `Ok(())` when they form a valid, matching pair; otherwise
/// returns an error message describing what is wrong (mismatched key, no
/// certificate found, malformed PEM, …). Does not check certificate expiry —
/// an already-expired but key-matched pair passes this check.
///
/// This is used by `POST /certs/reload` to reject invalid certs before writing
/// anything to disk.
pub fn validate_cert_key_pem(cert_pem: &str, key_pem: &str) -> anyhow::Result<()> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use rustls_pemfile::Item;

    // Parse certificates
    let cert_items: Vec<Item> = rustls_pemfile::read_all(&mut BufReader::new(cert_pem.as_bytes()))
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("failed to parse cert PEM: {e}"))?;

    let certs: Vec<CertificateDer<'static>> = cert_items
        .into_iter()
        .filter_map(|item| {
            if let Item::X509Certificate(der) = item {
                Some(CertificateDer::from(der.to_vec()))
            } else {
                None
            }
        })
        .collect();

    if certs.is_empty() {
        anyhow::bail!("no X.509 certificates found in cert PEM");
    }

    // Parse private key
    let key_items: Vec<Item> = rustls_pemfile::read_all(&mut BufReader::new(key_pem.as_bytes()))
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("failed to parse key PEM: {e}"))?;

    let key: PrivateKeyDer<'static> = key_items
        .into_iter()
        .find_map(|item| match item {
            Item::Pkcs1Key(k) => Some(PrivateKeyDer::Pkcs1(k.clone_key())),
            Item::Pkcs8Key(k) => Some(PrivateKeyDer::Pkcs8(k.clone_key())),
            Item::Sec1Key(k) => Some(PrivateKeyDer::Sec1(k.clone_key())),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("no private key found in key PEM"))?;

    // Build a ServerConfig — rustls verifies that the key matches the certificate.
    let _ = rustls::crypto::ring::default_provider().install_default();
    rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| anyhow::anyhow!("cert/key validation failed: {e}"))?;

    Ok(())
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::validate_cert_key_pem;

    /// Generate a self-signed (cert PEM, key PEM) pair for testing.
    fn generate_self_signed() -> (String, String) {
        let key_pair = rcgen::KeyPair::generate().expect("keygen");
        let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .expect("params")
            .self_signed(&key_pair)
            .expect("self-signed cert");
        (cert.pem(), key_pair.serialize_pem())
    }

    #[test]
    fn valid_self_signed_pair_passes() {
        let (cert, key) = generate_self_signed();
        assert!(
            validate_cert_key_pem(&cert, &key).is_ok(),
            "valid self-signed pair must pass validation"
        );
    }

    #[test]
    fn mismatched_key_is_rejected() {
        let (cert, _) = generate_self_signed();
        let (_, other_key) = generate_self_signed();
        let result = validate_cert_key_pem(&cert, &other_key);
        assert!(result.is_err(), "mismatched key must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("cert/key validation failed"),
            "error should mention validation: {msg}"
        );
    }

    #[test]
    fn empty_cert_pem_is_rejected() {
        let (_, key) = generate_self_signed();
        let result = validate_cert_key_pem("", &key);
        assert!(result.is_err(), "empty cert PEM must be rejected");
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no X.509 certificates"),);
    }

    #[test]
    fn empty_key_pem_is_rejected() {
        let (cert, _) = generate_self_signed();
        let result = validate_cert_key_pem(&cert, "");
        assert!(result.is_err(), "empty key PEM must be rejected");
        assert!(result.unwrap_err().to_string().contains("no private key"),);
    }

    #[test]
    fn garbage_pem_is_rejected() {
        let result = validate_cert_key_pem("not a cert", "not a key");
        assert!(result.is_err(), "garbage input must be rejected");
    }

    #[test]
    fn cert_with_non_cert_pem_item_is_rejected() {
        // A key PEM passed where the cert PEM is expected.
        let (_, key) = generate_self_signed();
        // Pass the key PEM as the cert PEM → no X.509 certificate found.
        let result = validate_cert_key_pem(&key, &key);
        assert!(result.is_err(), "key-as-cert must be rejected");
    }

    #[test]
    fn valid_pair_accepted_twice() {
        // Calling validate twice on the same pair must both succeed
        // (no global state mutation that would break the second call).
        let (cert, key) = generate_self_signed();
        assert!(validate_cert_key_pem(&cert, &key).is_ok());
        assert!(validate_cert_key_pem(&cert, &key).is_ok());
    }

    #[test]
    fn two_different_valid_pairs_both_accepted() {
        let (cert1, key1) = generate_self_signed();
        let (cert2, key2) = generate_self_signed();
        assert!(validate_cert_key_pem(&cert1, &key1).is_ok());
        assert!(validate_cert_key_pem(&cert2, &key2).is_ok());
    }

    #[test]
    fn cert_with_extra_whitespace_accepted() {
        let (cert, key) = generate_self_signed();
        let cert_with_ws = format!("\n{cert}\n");
        // Rustls should tolerate leading/trailing whitespace in PEM input.
        assert!(
            validate_cert_key_pem(&cert_with_ws, &key).is_ok(),
            "cert with surrounding whitespace must be accepted"
        );
    }

    // ── build_client_verifier ─────────────────────────────────────────────────

    /// Ensure the rustls crypto provider is installed before any test that
    /// uses WebPkiClientVerifier.  `validate_cert_key_pem` does this too, but
    /// when tests run in parallel the order is non-deterministic; calling here
    /// guarantees the provider is ready regardless of execution order.
    fn ensure_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    #[test]
    fn build_client_verifier_with_valid_ca_required() {
        use super::build_client_verifier;
        ensure_crypto_provider();
        // Generate a self-signed cert to use as CA.
        let key_pair = rcgen::KeyPair::generate().expect("keygen");
        let cert = rcgen::CertificateParams::new(vec!["ca.example.com".to_string()])
            .expect("params")
            .self_signed(&key_pair)
            .expect("cert");
        let cert_pem = cert.pem();

        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("ca.pem");
        std::fs::write(&ca_path, &cert_pem).unwrap();

        // Required (not optional) client auth.
        let result = build_client_verifier(ca_path.to_str().unwrap(), false);
        assert!(result.is_ok(), "valid CA must build verifier: {result:?}");
    }

    #[test]
    fn build_client_verifier_with_valid_ca_optional() {
        use super::build_client_verifier;
        ensure_crypto_provider();
        let key_pair = rcgen::KeyPair::generate().expect("keygen");
        let cert = rcgen::CertificateParams::new(vec!["ca.example.com".to_string()])
            .expect("params")
            .self_signed(&key_pair)
            .expect("cert");
        let cert_pem = cert.pem();

        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("ca.pem");
        std::fs::write(&ca_path, &cert_pem).unwrap();

        // Optional client auth.
        let result = build_client_verifier(ca_path.to_str().unwrap(), true);
        assert!(
            result.is_ok(),
            "optional CA must build verifier: {result:?}"
        );
    }

    #[test]
    fn build_client_verifier_missing_file_returns_error() {
        use super::build_client_verifier;
        ensure_crypto_provider();
        let result = build_client_verifier("/nonexistent/ca.pem", false);
        assert!(result.is_err(), "missing CA file must return error");
    }

    #[test]
    fn build_client_verifier_empty_pem_returns_error() {
        use super::build_client_verifier;
        ensure_crypto_provider();
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("empty.pem");
        std::fs::write(&ca_path, "").unwrap();
        let result = build_client_verifier(ca_path.to_str().unwrap(), false);
        assert!(result.is_err(), "empty CA file must return error");
    }
}
