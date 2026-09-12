//! ACME (Let's Encrypt) HTTP-01 certificate flow.
//!
//! Compiled only with this crate's own `acme` Cargo feature — mirrors the
//! pre-extraction `#![cfg(feature = "acme")]` file-level gate on
//! `src/server/acme.rs` (issue #114/#130). The root crate's `src/server/acme.rs`
//! is now a thin facade re-exporting this module's public items.
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context;
use dashmap::DashMap;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt,
    NewAccount, NewOrder, RetryPolicy,
};

use crate::config::AcmeConfig;

/// How many days before certificate expiry to trigger automatic renewal.
const RENEWAL_THRESHOLD_DAYS: i64 = 30;

/// How long to wait for the HTTP-01 challenge server to shut down gracefully
/// before forcibly aborting it (issue #352).
///
/// axum/hyper's graceful shutdown waits for any request that has already
/// been fully parsed and dispatched to a still-running handler before
/// closing that connection -- confirmed directly against this crate's
/// pinned axum/hyper versions (see the `challenge_server_shutdown_hangs_*`
/// test below), which also ruled out an earlier, less precise version of
/// this claim: a connection with merely *incomplete* request headers is
/// NOT treated as active and does not block shutdown on its own. A
/// pathologically slow handler invocation, or a peer slow enough to delay
/// completing an otherwise-dispatched request/response cycle, could
/// otherwise keep this task -- and the port/`_port_guard` it holds --
/// alive past the intended shutdown.
const CHALLENGE_SHUTDOWN_TIMEOUT_SECS: u64 = 10;

/// Writes `contents` to `path` with owner-only (0600) permissions on Unix,
/// covering both initial creation and the overwrite case.
///
/// Issue #278: the TLS private key and ACME account credentials are
/// secrets — plain `std::fs::write` can create files as `0644` (world-
/// readable) under a permissive umask. `OpenOptions::mode()` only applies
/// when `O_CREAT` actually creates the file, so a pre-existing file from
/// before this fix (or one that somehow ended up with looser permissions)
/// would keep its old mode on a mere re-open — the explicit
/// `set_permissions` call after writing re-tightens it every time,
/// covering the overwrite/renewal case, not just first creation.
///
/// On non-Unix platforms (no POSIX permission bits), falls back to a plain
/// write.
fn write_secret_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        // Deliberately `.truncate(false)` (explicit, not just omitted --
        // clippy::suspicious_open_options requires stating the intent):
        // OpenOptions::truncate(true) truncates as part of the `open()`
        // syscall itself, before this code gets a chance to chmod a
        // pre-existing looser-permission file first — that would leave a
        // window where the just-truncated (now empty) file is being
        // refilled with fresh secret content while still at its *old*
        // mode, e.g. `0644` (CodeRabbit finding on this PR). Truncating
        // manually via `set_len(0)` *after* chmod closes that window:
        // permissions are tightened before any content-modifying
        // operation ever touches the file.
        //
        // `O_NOFOLLOW` (issue #301): without it, a symlink pre-placed at
        // `path` by a local attacker able to write to the ACME storage
        // directory would be followed by `open()`, writing secret material
        // (TLS private key / ACME account credentials) to the symlink's
        // target instead of the intended location. Same pattern already
        // established twice in this codebase — see
        // `conduit_core::util::log_writer`'s and `static_files`'s own
        // `open_no_follow()`. `ELOOP` (symlink detected) surfaces as a
        // plain `io::Error` from `open()`, same as any other open failure.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.set_len(0)?;
        file.write_all(contents)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)
    }
}

// ── Per-port serialization lock ───────────────────────────────────────────────

/// One `Mutex` per HTTP-01 challenge port, ensuring that concurrent
/// `obtain_certificate` calls for different domains never race to bind the
/// same port.
static HTTP01_PORT_LOCKS: OnceLock<DashMap<u16, Arc<tokio::sync::Mutex<()>>>> = OnceLock::new();

fn http01_port_lock(port: u16) -> Arc<tokio::sync::Mutex<()>> {
    HTTP01_PORT_LOCKS
        .get_or_init(DashMap::new)
        .entry(port)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// On-disk paths to a site's TLS certificate and private key obtained via ACME.
pub struct AcmeCertPaths {
    /// Path to the PEM-encoded certificate chain.
    pub cert: PathBuf,
    /// Path to the PEM-encoded private key.
    pub key: PathBuf,
}

/// Return `true` when the first certificate in `cert_pem` expires within
/// `days` days.  Returns `true` on any parse error so the cert is renewed.
pub fn cert_expires_within_days(cert_pem: &str, days: i64) -> bool {
    use x509_parser::pem::parse_x509_pem;
    let Ok((_, pem)) = parse_x509_pem(cert_pem.as_bytes()) else {
        return true;
    };
    let Ok(x509) = pem.parse_x509() else {
        return true;
    };
    let not_after = x509.validity().not_after.timestamp();
    let now = std::time::SystemTime::UNIX_EPOCH
        .elapsed()
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    not_after < now + days * 86_400
}

/// Load a cached certificate from `storage_dir`, or run the full ACME flow to
/// obtain a fresh one.
///
/// If valid cert/key files already exist and the certificate is not expiring
/// within [`RENEWAL_THRESHOLD_DAYS`] days, the cached paths are returned
/// without contacting the ACME server.
///
/// `challenges` is the shared HTTP-01 challenge store; it is populated during
/// the ACME flow so that the challenge handler can serve token responses.
///
/// `http_challenge_port` is the local TCP port on which the temporary HTTP-01
/// challenge server listens during certificate procurement (typically 80).
pub async fn load_or_obtain_certificate(
    acme_cfg: &AcmeConfig,
    domain: &str,
    challenges: Arc<DashMap<String, String>>,
    storage_dir: &Path,
    http_challenge_port: u16,
) -> anyhow::Result<AcmeCertPaths> {
    std::fs::create_dir_all(storage_dir)
        .with_context(|| format!("creating ACME storage directory {storage_dir:?}"))?;

    let cert_path = storage_dir.join(format!("{domain}.crt.pem"));
    let key_path = storage_dir.join(format!("{domain}.key.pem"));

    // Reuse the cached certificate when it is not about to expire.
    if cert_path.exists() && key_path.exists() {
        if let Ok(pem) = std::fs::read_to_string(&cert_path) {
            if !cert_expires_within_days(&pem, RENEWAL_THRESHOLD_DAYS) {
                tracing::info!(domain, "reusing cached ACME certificate");
                return Ok(AcmeCertPaths {
                    cert: cert_path,
                    key: key_path,
                });
            }
        }
        tracing::info!(domain, "ACME certificate expires soon — renewing");
    } else {
        tracing::info!(domain, "obtaining ACME certificate for the first time");
    }

    let (cert_pem, key_pem) =
        obtain_certificate(acme_cfg, domain, &challenges, http_challenge_port).await?;

    std::fs::write(&cert_path, &cert_pem)
        .with_context(|| format!("writing cert to {cert_path:?}"))?;
    // Owner-only permissions: this is a private key (issue #278).
    write_secret_file(&key_path, key_pem.as_bytes())
        .with_context(|| format!("writing key to {key_path:?}"))?;

    Ok(AcmeCertPaths {
        cert: cert_path,
        key: key_path,
    })
}

/// Run the complete ACME HTTP-01 flow and return `(cert_chain_pem, key_pem)`.
async fn obtain_certificate(
    acme_cfg: &AcmeConfig,
    domain: &str,
    challenges: &Arc<DashMap<String, String>>,
    http_challenge_port: u16,
) -> anyhow::Result<(String, String)> {
    let directory_url = acme_cfg
        .directory
        .as_deref()
        .unwrap_or_else(|| LetsEncrypt::Production.url())
        .to_owned();

    let account = load_or_create_account(acme_cfg, &directory_url).await?;

    let identifiers = [Identifier::Dns(domain.to_string())];
    let mut order = account
        .new_order(&NewOrder::new(&identifiers))
        .await
        .context("ACME new-order request failed")?;

    // Acquire the per-port lock so concurrent ACME orders (multiple domains,
    // or an issuance overlapping a renewal) never race to bind the same port.
    let _port_lock = http01_port_lock(http_challenge_port);
    let _port_guard = _port_lock.lock().await;

    // Bind the HTTP-01 challenge server port *before* spawning the background
    // task so that port-bind failures are reported here as ACME errors rather
    // than being silently swallowed inside the spawned task.
    let ch_listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{http_challenge_port}"))
        .await
        .with_context(|| {
            format!("failed to bind ACME HTTP-01 challenge server on port {http_challenge_port}")
        })?;
    tracing::debug!(
        port = http_challenge_port,
        "ACME HTTP-01 challenge server bound"
    );

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server_task = {
        let ch_map = challenges.clone();
        tokio::spawn(run_challenge_server(ch_listener, ch_map, stop_rx))
    };

    // Populate challenge tokens and notify the CA to begin validation.
    // Track every token we insert so we can remove only our own entries later,
    // leaving tokens belonging to other concurrent ACME operations intact.
    //
    // Issue #279: every fallible step below used to propagate its error via
    // `?` directly, which skipped the cleanup further down (shutdown signal,
    // awaiting the server task, removing our tokens) on any early return —
    // leaking the bound port (the spawned task keeps holding it even though
    // `_port_guard` gets dropped) and leaving stale challenge tokens in the
    // shared map. Captured into `flow_result` instead so cleanup always runs,
    // then propagated via `?` only after cleanup completes.
    let mut our_tokens: Vec<String> = Vec::new();
    let retry = RetryPolicy::default();
    let flow_result: anyhow::Result<()> = async {
        let mut authorizations = order.authorizations();
        while let Some(authz_result) = authorizations.next().await {
            let mut authz = authz_result.context("ACME authorizations fetch failed")?;

            if authz.status == AuthorizationStatus::Valid {
                continue; // already validated from a previous attempt
            }

            let mut challenge_handle = authz.challenge(ChallengeType::Http01).ok_or_else(|| {
                anyhow::anyhow!("no HTTP-01 challenge available for domain {domain}")
            })?;

            let key_auth = challenge_handle.key_authorization();
            let token = challenge_handle.token.clone();
            our_tokens.push(token.clone());
            challenges.insert(token, key_auth.as_str().to_owned());

            challenge_handle
                .set_ready()
                .await
                .context("set_challenge_ready failed")?;
        }

        // Poll the order until all challenges are validated and the order is ready.
        order
            .poll_ready(&retry)
            .await
            .context("ACME order timed out waiting for challenge validation")?;
        Ok(())
    }
    .await;

    // Always shut down the temporary challenge server and clean up our
    // tokens, regardless of whether the flow above succeeded (#279).
    let _ = stop_tx.send(());
    // Bounded (issue #352): a peer holding an "active" (partial-request)
    // connection open could otherwise keep this task -- and the port it
    // holds -- alive indefinitely, since axum's graceful shutdown only
    // closes idle connections promptly. Dropping the JoinHandle on timeout
    // would NOT stop the spawned task (it keeps running detached) -- an
    // explicit abort_handle().abort() is required to actually reclaim the
    // port.
    let abort_handle = server_task.abort_handle();
    match tokio::time::timeout(
        Duration::from_secs(CHALLENGE_SHUTDOWN_TIMEOUT_SECS),
        server_task,
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!("ACME HTTP-01 challenge server task did not exit cleanly: {e}");
        }
        Err(_) => {
            abort_handle.abort();
            tracing::warn!(
                timeout_secs = CHALLENGE_SHUTDOWN_TIMEOUT_SECS,
                "ACME HTTP-01 challenge server did not shut down within the timeout \
                 (a peer likely held an active connection open); forcibly aborted the \
                 task to reclaim the port"
            );
        }
    }
    // Remove only the tokens we inserted, preserving any tokens that belong
    // to concurrent ACME operations for other domains.
    for token in &our_tokens {
        challenges.remove(token);
    }

    flow_result?;

    // Finalize the order: instant-acme generates a fresh ECDSA key-pair and CSR
    // internally (via the `rcgen` feature), then sends the CSR to the CA.
    // Returns the private key as PEM.
    let key_pem = order.finalize().await.context("ACME finalize failed")?;

    // Retrieve the issued certificate chain (polls until the CA makes it available).
    let cert_chain_pem = order
        .poll_certificate(&retry)
        .await
        .context("ACME certificate fetch failed")?;

    Ok((cert_chain_pem, key_pem))
}

/// Create an [`instant_acme::AccountBuilder`] with the appropriate TLS root
/// certificate configuration.
///
/// When the environment variable `CONDUIT_ACME_EXTRA_ROOT` is set to a path,
/// the file at that path is loaded as an additional trusted root CA (PEM
/// format).  This is intended for CI environments that use test ACME servers
/// such as [Pebble](https://github.com/letsencrypt/pebble) with self-signed
/// certificates that are not in the system trust store.
fn account_builder() -> anyhow::Result<instant_acme::AccountBuilder> {
    if let Ok(ca_path) = std::env::var("CONDUIT_ACME_EXTRA_ROOT") {
        tracing::debug!(
            ca_path,
            "using custom ACME root CA from CONDUIT_ACME_EXTRA_ROOT"
        );
        Account::builder_with_root(&ca_path)
            .with_context(|| format!("loading custom ACME root CA from {ca_path:?}"))
    } else {
        Account::builder().context("failed to create ACME HTTP client")
    }
}

/// Load a persisted ACME account from `storage_dir/acme_account.json`, or
/// create a new one with the ACME server and persist the credentials.
async fn load_or_create_account(
    acme_cfg: &AcmeConfig,
    directory_url: &str,
) -> anyhow::Result<Account> {
    let storage = acme_cfg.storage.as_deref().unwrap_or("./certs");
    let creds_path = PathBuf::from(storage).join("acme_account.json");

    if creds_path.exists() {
        let json = std::fs::read_to_string(&creds_path)
            .with_context(|| format!("reading ACME credentials from {creds_path:?}"))?;
        let creds: AccountCredentials = serde_json::from_str(&json)
            .with_context(|| format!("parsing ACME credentials in {creds_path:?}"))?;
        let account = account_builder()?
            .from_credentials(creds)
            .await
            .context("restoring ACME account from credentials failed")?;
        tracing::debug!("ACME account restored from {creds_path:?}");
        return Ok(account);
    }

    let (account, credentials) = account_builder()?
        .create(
            &NewAccount {
                contact: &[&format!("mailto:{}", acme_cfg.email)],
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            directory_url.to_owned(),
            None,
        )
        .await
        .context("ACME account creation failed")?;

    std::fs::create_dir_all(storage)?;
    let json = serde_json::to_string_pretty(&credentials)?;
    // Owner-only permissions: this file holds the ACME account's private
    // key material (issue #278).
    write_secret_file(&creds_path, json.as_bytes())
        .with_context(|| format!("saving ACME credentials to {creds_path:?}"))?;
    tracing::info!("new ACME account created and saved to {creds_path:?}");

    Ok(account)
}

/// Serve ACME HTTP-01 challenges on the pre-bound `listener`.
///
/// Responds to `GET /.well-known/acme-challenge/{token}` with the
/// corresponding key-authorization from `challenges`.
/// Shuts down when `stop_rx` fires.
///
/// The caller must bind the [`TcpListener`] *before* spawning this task so
/// that port-bind failures are surfaced as ACME errors rather than being
/// silently swallowed in the background.
async fn run_challenge_server(
    listener: tokio::net::TcpListener,
    challenges: Arc<DashMap<String, String>>,
    stop_rx: tokio::sync::oneshot::Receiver<()>,
) {
    use axum::extract::{Path, State};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;

    async fn challenge_handler(
        Path(token): Path<String>,
        State(store): State<Arc<DashMap<String, String>>>,
    ) -> impl IntoResponse {
        match store.get(&token) {
            Some(key_auth) => (
                axum::http::StatusCode::OK,
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                key_auth.clone(),
            )
                .into_response(),
            None => axum::http::StatusCode::NOT_FOUND.into_response(),
        }
    }

    let app = Router::new()
        .route(
            "/.well-known/acme-challenge/{token}",
            get(challenge_handler),
        )
        .with_state(challenges);

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            stop_rx.await.ok();
        })
        .await
    {
        tracing::error!(error = %e, "ACME HTTP-01 challenge server accept loop failed");
    }
}

/// Spawn a background task that renews the certificate for `domain` when it is
/// within [`RENEWAL_THRESHOLD_DAYS`] days of expiry.
///
/// Checks every 12 hours.
pub fn spawn_renewal_task(
    acme_cfg: AcmeConfig,
    domain: String,
    challenges: Arc<DashMap<String, String>>,
    storage_dir: PathBuf,
    http_challenge_port: u16,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(12 * 3600));
        loop {
            interval.tick().await;

            let cert_path = storage_dir.join(format!("{domain}.crt.pem"));
            if cert_path.exists() {
                match std::fs::read_to_string(&cert_path) {
                    Ok(pem) if !cert_expires_within_days(&pem, RENEWAL_THRESHOLD_DAYS) => {
                        continue; // not expiring soon
                    }
                    _ => {}
                }
            }

            tracing::info!(domain, "renewing ACME certificate");
            match load_or_obtain_certificate(
                &acme_cfg,
                &domain,
                challenges.clone(),
                &storage_dir,
                http_challenge_port,
            )
            .await
            {
                Ok(_) => tracing::info!(domain, "ACME certificate renewed successfully"),
                Err(e) => tracing::error!(domain, error = %e, "ACME certificate renewal failed"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a self-signed cert PEM with a caller-controlled `not_after`, so
    /// tests can exercise both "far from expiry" and "expiring soon"
    /// branches of `cert_expires_within_days` deterministically.
    fn self_signed_cert_with_not_after(not_after: time::OffsetDateTime) -> String {
        let key_pair = rcgen::KeyPair::generate().expect("keygen");
        let mut params =
            rcgen::CertificateParams::new(vec!["example.com".to_string()]).expect("params");
        params.not_after = not_after;
        params
            .self_signed(&key_pair)
            .expect("self-signed cert")
            .pem()
    }

    #[test]
    fn invalid_pem_is_treated_as_expiring() {
        assert!(
            cert_expires_within_days("not a valid pem", 30),
            "unparseable input must fail safe (treated as expiring) rather than silently \
             skipping renewal"
        );
    }

    #[test]
    fn cert_far_from_expiry_is_not_flagged() {
        let pem = self_signed_cert_with_not_after(
            time::OffsetDateTime::now_utc() + time::Duration::days(365),
        );
        assert!(!cert_expires_within_days(&pem, RENEWAL_THRESHOLD_DAYS));
    }

    #[test]
    fn cert_expiring_within_threshold_is_flagged() {
        let pem = self_signed_cert_with_not_after(
            time::OffsetDateTime::now_utc() + time::Duration::days(10),
        );
        assert!(cert_expires_within_days(&pem, RENEWAL_THRESHOLD_DAYS));
    }

    #[test]
    fn already_expired_cert_is_flagged() {
        let pem = self_signed_cert_with_not_after(
            time::OffsetDateTime::now_utc() - time::Duration::days(1),
        );
        assert!(cert_expires_within_days(&pem, RENEWAL_THRESHOLD_DAYS));
    }

    // ── write_secret_file (issue #278: owner-only permissions) ────────────────

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    #[cfg(unix)]
    fn write_secret_file_creates_with_owner_only_permissions() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("secret.pem");
        write_secret_file(&path, b"top secret key material").unwrap();
        assert_eq!(
            mode_of(&path),
            0o600,
            "a freshly created secret file must be owner-only, not umask-dependent"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"top secret key material");
    }

    #[test]
    #[cfg(unix)]
    fn write_secret_file_tightens_permissions_on_overwrite() {
        // Simulates a file that pre-dates this fix, or was otherwise created
        // with looser permissions (e.g. by an older Conduit build) — the
        // next write must re-tighten it to 0600, not just leave the
        // existing mode alone (OpenOptions::mode() only applies when
        // O_CREAT actually creates the file).
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("secret.pem");
        std::fs::write(&path, b"old content").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(mode_of(&path), 0o644, "test setup sanity check");

        write_secret_file(&path, b"new secret content").unwrap();

        assert_eq!(
            mode_of(&path),
            0o600,
            "overwriting a pre-existing file must re-tighten its permissions"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"new secret content");
    }

    #[test]
    #[cfg(unix)]
    fn write_secret_file_shorter_overwrite_leaves_no_stale_trailing_bytes() {
        // write_secret_file no longer uses OpenOptions::truncate(true)
        // (removed per a CodeRabbit finding on this PR — see its doc
        // comment) and truncates manually via set_len(0) instead. This
        // proves that still correctly drops old trailing content when the
        // new contents are shorter than what was there before, not just
        // that permissions end up right.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("secret.pem");
        write_secret_file(&path, b"a very long old secret payload").unwrap();
        write_secret_file(&path, b"short").unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"short",
            "no stale bytes from the longer previous content must survive"
        );
    }

    #[test]
    #[cfg(unix)]
    fn write_secret_file_rejects_symlink_at_target_path() {
        // Issue #301: a symlink pre-placed at `path` must not be followed —
        // O_NOFOLLOW should make the open() fail (ELOOP) rather than write
        // secret material through to the symlink's target.
        use std::os::unix::fs::symlink;
        let dir = tempfile::TempDir::new().unwrap();
        let real_target = dir.path().join("real-secret.pem");
        let link = dir.path().join("secret.pem");
        std::fs::write(&real_target, b"pre-existing, must not be overwritten").unwrap();
        symlink(&real_target, &link).unwrap();

        let result = write_secret_file(&link, b"attacker-adjacent content");
        assert!(result.is_err(), "writing through a symlink must fail");
        assert_eq!(
            std::fs::read(&real_target).unwrap(),
            b"pre-existing, must not be overwritten",
            "the symlink target must be untouched"
        );
    }

    /// Mirrors `run_challenge_server`'s exact shape (bind -> axum::serve
    /// with a `stop_rx`-driven `with_graceful_shutdown`) but with a
    /// deliberately slow handler, so the "active connection blocks
    /// graceful shutdown" mechanism can be exercised deterministically.
    ///
    /// An earlier version of these tests tried to trigger this by sending a
    /// request with incomplete headers (no trailing blank line) against the
    /// *real* `run_challenge_server`. That does NOT reproduce a hang on
    /// axum 0.8.9 / hyper 1.10 (confirmed empirically: shutdown completed
    /// well under 300ms) -- graceful shutdown only waits for a request that
    /// has already been fully parsed and dispatched to a still-running
    /// handler, not one still being read. Confirmed the real mechanism
    /// instead with a standalone probe: a genuinely slow handler blocks
    /// shutdown for exactly as long as it keeps running (measured >500ms
    /// against a 5-second sleep), matching axum/hyper's documented
    /// behavior. This helper reproduces that confirmed mechanism instead of
    /// the unconfirmed one.
    async fn spawn_slow_challenge_like_server(
        handler_delay: Duration,
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        use axum::routing::get;
        use axum::Router;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

        let app = Router::new().route(
            "/slow",
            get(move || async move {
                tokio::time::sleep(handler_delay).await;
                "done"
            }),
        );
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    stop_rx.await.ok();
                })
                .await;
        });
        (addr, task, stop_tx)
    }

    /// Returns the connected stream so the caller can keep it alive -- if
    /// dropped, the connection closes and hyper has nothing left to wait
    /// for, defeating the whole point of this helper (confirmed as a real
    /// bug in an earlier draft: the connection was dropped at the end of
    /// this function, and the "active connection" premise silently stopped
    /// applying because there was no longer any connection to be active).
    async fn connect_and_send_get(addr: std::net::SocketAddr, path: &str) -> tokio::net::TcpStream {
        use tokio::io::AsyncWriteExt;
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        // Give the request a moment to actually be parsed and dispatched to
        // the (slow) handler before the caller signals shutdown.
        tokio::time::sleep(Duration::from_millis(50)).await;
        client
    }

    #[tokio::test]
    async fn challenge_server_shutdown_hangs_on_active_connection_without_bound() {
        // Reproduces issue #352's premise: without a bounded wait, an
        // active (in-flight, not-yet-returned handler) connection keeps
        // graceful shutdown from ever completing. This is what
        // obtain_certificate()'s CHALLENGE_SHUTDOWN_TIMEOUT_SECS wrapper
        // guards against.
        let (addr, server_task, stop_tx) =
            spawn_slow_challenge_like_server(Duration::from_secs(5)).await;
        let _client = connect_and_send_get(addr, "/slow").await;
        stop_tx.send(()).unwrap();

        let result = tokio::time::timeout(Duration::from_millis(300), server_task).await;
        assert!(
            result.is_err(),
            "expected the shutdown to still be blocked by the active connection"
        );
    }

    #[tokio::test]
    async fn challenge_server_forcibly_aborted_after_timeout_releases_the_port() {
        // Verifies the actual fix: after the bounded wait elapses, calling
        // AbortHandle::abort() genuinely reclaims the port. Dropping the
        // JoinHandle alone would NOT be enough -- a tokio spawned task keeps
        // running detached when its JoinHandle is simply dropped, so this
        // proves the explicit abort_handle().abort() call is load-bearing,
        // not redundant.
        let (addr, server_task, stop_tx) =
            spawn_slow_challenge_like_server(Duration::from_secs(5)).await;
        let abort_handle = server_task.abort_handle();
        let _client = connect_and_send_get(addr, "/slow").await;
        stop_tx.send(()).unwrap();

        let result = tokio::time::timeout(Duration::from_millis(300), server_task).await;
        assert!(
            result.is_err(),
            "expected shutdown to be blocked by the active connection"
        );

        abort_handle.abort();
        // Task cancellation happens at the next yield point, not
        // synchronously with abort() -- give it a moment to actually land.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let rebind = tokio::net::TcpListener::bind(addr).await;
        assert!(
            rebind.is_ok(),
            "port should be free after the aborted task is reclaimed: {:?}",
            rebind.err()
        );
    }

    /// Sends a raw HTTP/1.1 GET and returns (status_code, body) -- avoids
    /// pulling in an HTTP client dependency this crate doesn't otherwise
    /// need, matching this codebase's established raw-`TcpStream` testing
    /// idiom (see `.claude/skills/testing/SKILL.md`).
    async fn raw_get(addr: std::net::SocketAddr, path: &str) -> (u16, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        let response = String::from_utf8_lossy(&raw);
        let mut parts = response.splitn(2, "\r\n\r\n");
        let head = parts.next().unwrap_or_default();
        let body = parts.next().unwrap_or_default().to_string();
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .expect("response must have a parseable status line");
        (status, body)
    }

    #[tokio::test]
    async fn challenge_server_route_serves_a_registered_token() {
        // Direct end-to-end test of run_challenge_server's real route
        // registration -- found necessary because none of the other tests
        // in this file exercised it directly. It previously used axum
        // 0.6/0.7-style ":token" path-parameter syntax, which axum 0.8
        // rejects: `Router::route()` panics immediately at registration
        // time with "Path segments must not start with `:`" -- meaning
        // *every* real HTTP-01 challenge attempt would have failed before
        // the challenge server ever started accepting connections. Caught
        // by chance while writing the shutdown-timeout tests above (they
        // originally called run_challenge_server directly too, and hit
        // this panic before being rewritten to use a synthetic app
        // instead). Negative control: reverting the route string from
        // "{token}" back to ":token" reproduces the exact panic above.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let challenges = Arc::new(DashMap::new());
        challenges.insert(
            "my-token".to_string(),
            "expected-key-authorization".to_string(),
        );
        let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let _server_task = tokio::spawn(run_challenge_server(listener, challenges, stop_rx));

        // Give the accept loop a moment to actually start listening.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let (status, body) = raw_get(addr, "/.well-known/acme-challenge/my-token").await;
        assert_eq!(status, 200);
        assert_eq!(body, "expected-key-authorization");
    }

    #[tokio::test]
    async fn challenge_server_route_404s_for_an_unregistered_token() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let challenges = Arc::new(DashMap::new());
        let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let _server_task = tokio::spawn(run_challenge_server(listener, challenges, stop_rx));

        tokio::time::sleep(Duration::from_millis(20)).await;

        let (status, _body) = raw_get(addr, "/.well-known/acme-challenge/no-such-token").await;
        assert_eq!(status, 404);
    }
}
