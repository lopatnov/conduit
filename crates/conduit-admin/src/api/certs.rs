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

/// Write `data` to `path` atomically by writing to a sibling temporary file and then renaming it
/// into place.
///
/// The temporary name is not predictable and is created with `create_new` (`O_EXCL`), so a
/// pre-planted symlink or file under that name is never followed or truncated. On Unix the new file
/// starts as `0600`, and when `path` already exists it takes over that file's permissions before the
/// rename — rotating a `0600` private key must not leave a world-readable one behind (issue #481).
pub(super) fn atomic_write(path: &str, data: &[u8]) -> std::io::Result<()> {
    use std::fs::{self, OpenOptions};
    use std::io::Write as _;
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let tmp = format!("{path}.{}.{nanos}.tmp", std::process::id());

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }

    let result = (|| {
        let mut f = options.open(&tmp)?;
        if let Ok(existing) = fs::metadata(path) {
            f.set_permissions(existing.permissions())?;
        }
        f.write_all(data)?;
        f.flush()?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
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

#[cfg(test)]
mod atomic_write_tests {
    use super::atomic_write;

    #[test]
    fn replaces_the_file_and_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.pem");
        let path = path.to_str().unwrap();
        std::fs::write(path, b"old").unwrap();
        atomic_write(path, b"new").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"new");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["key.pem"], "no .tmp file may be left");
    }

    /// Issue #481: rotating a `0600` key must not produce a world-readable one.
    #[cfg(unix)]
    #[test]
    fn keeps_the_mode_of_the_file_it_replaces_and_creates_new_files_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        let existing = dir.path().join("existing.pem");
        std::fs::write(&existing, b"old").unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o640)).unwrap();
        atomic_write(existing.to_str().unwrap(), b"new").unwrap();
        assert_eq!(mode(&existing), 0o640);

        let fresh = dir.path().join("fresh.pem");
        atomic_write(fresh.to_str().unwrap(), b"data").unwrap();
        assert_eq!(mode(&fresh), 0o600, "a new file must not follow the umask");
    }

    /// Issue #481: a pre-planted `<path>.tmp` symlink is neither followed nor truncated.
    #[cfg(unix)]
    #[test]
    fn does_not_follow_a_planted_tmp_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"precious").unwrap();
        let path = dir.path().join("cert.pem");
        std::os::unix::fs::symlink(&victim, dir.path().join("cert.pem.tmp")).unwrap();
        atomic_write(path.to_str().unwrap(), b"cert").unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
        assert_eq!(std::fs::read(&path).unwrap(), b"cert");
    }
}
