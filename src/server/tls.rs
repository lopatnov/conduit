use pingora_core::listeners::tls::TlsSettings;

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
