//! Validation of `tls` and `tls.clientAuth`, including the certificate near-expiry warning.

use std::time::{Duration, SystemTime};

use super::ValidationError;

use crate::config::schema::{TlsClientAuth, TlsConfig};

pub(super) fn validate_tls(tls: &TlsConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    let has_acme = tls.acme.is_some();
    let has_cert = tls.cert.is_some();
    let has_key = tls.key.is_some();

    if has_acme && (has_cert || has_key) {
        errors.push(ValidationError::new(
            prefix,
            "Cannot combine 'acme' with 'cert'/'key' — use Auto-TLS or manual certificates, not both",
        ));
    }

    // XOR: one is set but not the other
    if !has_acme && (has_cert ^ has_key) {
        let missing = if has_cert { "key" } else { "cert" };
        errors.push(ValidationError::new(
            prefix,
            format!("TLS 'cert' and 'key' must both be set — '{missing}' is missing"),
        ));
    }

    if let Some(ref ca) = tls.client_auth {
        validate_tls_client_auth(ca, prefix, has_cert, has_acme, errors);
    }

    // tls.versions / tls.ciphers: parsed but never enforced (issue #189).
    // Pingora's rustls `TlsSettings` gives conduit no hook to influence
    // protocol-version or cipher-suite selection — `TlsSettings::build()`
    // hardcodes `ServerConfig::builder_with_protocol_versions(&[TLS12, TLS13])`
    // with the default rustls cipher suite set, all fields are private, and
    // the constructors (`intermediate()`, and `with_callbacks()` in 0.9) take no
    // version or cipher parameter
    // (confirmed against vendored `pingora-core-0.8.1/src/listeners/tls/
    // rustls/mod.rs`, and again against 0.9.0: `Acceptor::from_server_config`
    // exists there, but a listener only accepts `TlsSettings`, so it cannot be
    // installed). Silently accepting these fields would let an operator
    // believe they've restricted TLS versions/ciphers for compliance reasons
    // when nothing is actually enforced — a hard validation error forces
    // them to notice and remove the setting, rather than a warning they
    // could miss in startup logs. Revisit once Pingora exposes a
    // `ServerConfig`-customization hook (tracked in CLAUDE.md as blocked).
    if tls.versions.is_some() {
        errors.push(ValidationError::new(
            format!("{prefix}.versions"),
            "tls.versions is not currently enforced — Pingora 0.9's rustls TLS backend gives \
             Conduit no API to restrict protocol versions (TLS 1.2 and 1.3 are always both \
             enabled). Remove this field; see \
             https://github.com/lopatnov/conduit/issues/189 for status.",
        ));
    }
    if tls.ciphers.is_some() {
        errors.push(ValidationError::new(
            format!("{prefix}.ciphers"),
            "tls.ciphers is not currently enforced — Pingora 0.9's rustls TLS backend gives \
             Conduit no API to restrict cipher suites (the default rustls suite set is always \
             used). Remove this field; see \
             https://github.com/lopatnov/conduit/issues/189 for status.",
        ));
    }

    // Cert expiry check — only for manual certificates (ACME manages renewal itself).
    if !has_acme {
        if let Some(ref cert_path) = tls.cert {
            check_cert_expiry(cert_path, &format!("{prefix}.cert"), errors);
        }
    }
}

/// Validate the `tls.clientAuth` (mTLS) configuration block.
fn validate_tls_client_auth(
    ca: &TlsClientAuth,
    prefix: &str,
    has_cert: bool,
    has_acme: bool,
    errors: &mut Vec<ValidationError>,
) {
    if ca.ca.is_empty() {
        errors.push(ValidationError::new(
            format!("{prefix}.clientAuth.ca"),
            "tls.clientAuth.ca must be a path to a PEM CA file",
        ));
    }
    // clientAuth requires cert+key (or acme) to make sense.
    if !has_cert && !has_acme {
        errors.push(ValidationError::new(
            format!("{prefix}.clientAuth"),
            "tls.clientAuth requires tls.cert+tls.key or tls.acme to be configured",
        ));
    }
}

/// Check a PEM certificate file for expiry.
///
/// - If the cert is already expired → hard validation error (blocks startup/reload).
/// - If the cert expires within 30 days → advisory `Severity::Warning` (issue #191):
///   `conduit validate` still exits non-zero on it (prompting renewal before a real
///   deploy), but `run_server()`/`/reload` log it and continue rather than refusing
///   to start on an otherwise-valid certificate.
/// - If the file does not exist yet → silently ignored (cert may be provisioned later).
/// - If the file cannot be parsed → silently ignored (startup will fail with a clearer error).
fn check_cert_expiry(cert_path: &str, prefix: &str, errors: &mut Vec<ValidationError>) {
    let pem_bytes = match std::fs::read(cert_path) {
        Ok(b) => b,
        Err(_) => return, // file not found yet — skip
    };

    // Parse the first PEM block.
    let (_, pem) = match x509_parser::pem::parse_x509_pem(&pem_bytes) {
        Ok(v) => v,
        Err(_) => return, // not a valid PEM — skip
    };
    let cert = match pem.parse_x509() {
        Ok(c) => c,
        Err(_) => return, // unparseable DER — skip
    };

    let not_after = cert.validity().not_after.to_datetime();

    // Convert `not_after` (x509_parser's `time::OffsetDateTime`) to `SystemTime`.
    let unix_secs = not_after.unix_timestamp();
    let expires_at = if unix_secs >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_secs(unix_secs as u64)
    } else {
        SystemTime::UNIX_EPOCH
    };

    let now = SystemTime::now();
    let warn_threshold = now + Duration::from_secs(30 * 24 * 3600);

    if expires_at <= now {
        errors.push(ValidationError::new(
            prefix,
            format!("TLS certificate has expired (not_after = {unix_secs})"),
        ));
    } else if expires_at <= warn_threshold {
        let days_left = expires_at.duration_since(now).unwrap_or_default().as_secs() / 86400;
        errors.push(ValidationError::warning(
            prefix,
            format!("WARNING: TLS certificate expires in {days_left} day(s) — renew soon"),
        ));
    }
}
