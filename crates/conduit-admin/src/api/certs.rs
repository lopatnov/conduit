//! `POST /certs/reload` and the PEM validation behind it.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::BufReader;
use std::sync::Arc;

use conduit_runtime::proxy::service::AppState;

use super::error::{AdminError, AdminResult};

// ── Certificate rotation ──────────────────────────────────────────────────────

/// Request body for `POST /certs/reload`.
#[derive(Deserialize)]
pub(super) struct CertReloadRequest {
    /// PEM-encoded certificate chain (leaf + intermediates).
    cert: String,
    /// PEM-encoded private key (PKCS#1, PKCS#8, or SEC1).
    key: String,
}

/// `POST /certs/reload` — validate new cert+key and write them to disk.
///
/// The new certificate is validated (cert/key must match and be parseable),
/// then written atomically to the file paths configured in `tls.cert` /
/// `tls.key`.
///
/// # Activation requires a process restart — `conduit reload`/`/reload` will NOT do it (issue #190)
///
/// This endpoint only rewrites file *content* at the existing `tls.cert`/
/// `tls.key` paths — the paths themselves don't change. Pingora's TLS
/// listener loads its `rustls::ServerConfig` once at startup and never
/// re-reads the cert/key files afterward, so the running listener keeps
/// serving the *old* certificate until the process actually restarts.
/// `detect_cold_changes()` (used by `/reload`) compares config *values*, not
/// file contents — since the path strings are unchanged, it cannot detect
/// this case either, so a `/reload` after this endpoint returns `200 OK`
/// without having activated anything. There is no config-only way to make
/// `/reload` pick up a rotated certificate; only a real restart does.
///
/// # Notes on zero-downtime rotation
///
/// Conduit installs the certificate once, when it builds the listener's rustls
/// config, and does not yet register a certificate resolver that could swap it
/// at runtime (Pingora 0.9 exposes `TlsSettings::set_cert_resolver` for that).
/// Until it does, zero-downtime rotation (hot-swap without restarting the
/// listener) requires a process upgrade: start the new process with `--upgrade` so it
/// inherits the listening socket FDs from the old process, then send SIGQUIT
/// to the old process.  On systems managed by systemd this is done via
/// `systemctl reload conduit`.
///
/// # Errors
///
/// Returns `400 Bad Request` when:
/// - No site has `tls.cert` / `tls.key` configured.
/// - The provided cert/key PEM is invalid or the pair does not match.
///
/// Returns `500 Internal Server Error` when the atomic file write fails.
pub(super) async fn certs_reload_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CertReloadRequest>,
) -> AdminResult<Json<Value>> {
    // Find the first site that has manual TLS cert/key configured.
    let config = state.config.load();
    let (cert_path, key_path) = config
        .sites
        .iter()
        .find_map(|site| {
            let tls = site.tls.as_ref()?;
            let cert = tls.cert.as_deref()?;
            let key = tls.key.as_deref()?;
            Some((cert.to_owned(), key.to_owned()))
        })
        .ok_or_else(|| {
            AdminError::BadRequest(
                "no site has tls.cert/tls.key configured — nothing to rotate".to_owned(),
            )
        })?;

    // Validate cert+key before touching any files.
    validate_cert_key_pem(&body.cert, &body.key)
        .map_err(|e| AdminError::BadRequest(format!("invalid cert/key: {e}")))?;

    // Write cert atomically: write to a temp file next to the destination,
    // then rename so readers never see a partial write.
    atomic_write(&cert_path, body.cert.as_bytes()).map_err(|e| {
        AdminError::ServerError(format!("failed to write cert to {cert_path}: {e}"))
    })?;
    atomic_write(&key_path, body.key.as_bytes())
        .map_err(|e| AdminError::ServerError(format!("failed to write key to {key_path}: {e}")))?;

    tracing::info!(cert = %cert_path, key = %key_path, "TLS certificate written via /certs/reload");

    Ok(Json(json!({
        "status": "ok",
        "cert_path": cert_path,
        "key_path": key_path,
        "note": "certificate written to disk but NOT yet active — the running TLS listener \
                 was built once at startup and does not re-read cert/key files; POST /reload \
                 will not activate it either, since the config paths themselves haven't \
                 changed. A restart of the process (or --upgrade for a zero-downtime process \
                 swap) is required to actually serve the new certificate."
    })))
}

/// Write `data` to `path` atomically by writing to a sibling `.tmp` file
/// and then renaming it into place.
pub(super) fn atomic_write(path: &str, data: &[u8]) -> std::io::Result<()> {
    use std::fs;
    use std::io::Write as _;
    let tmp = format!("{path}.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.flush()?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
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
