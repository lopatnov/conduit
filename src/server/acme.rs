use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use dashmap::DashMap;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt,
    NewAccount, NewOrder, RetryPolicy,
};

use crate::config::schema::AcmeConfig;

/// How many days before certificate expiry to trigger automatic renewal.
const RENEWAL_THRESHOLD_DAYS: i64 = 30;

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
    std::fs::write(&key_path, &key_pem).with_context(|| format!("writing key to {key_path:?}"))?;

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
    {
        let ch_map = challenges.clone();
        tokio::spawn(run_challenge_server(ch_listener, ch_map, stop_rx));
    }

    // Populate challenge tokens and notify the CA to begin validation.
    // Track every token we insert so we can remove only our own entries later,
    // leaving tokens belonging to other concurrent ACME operations intact.
    let mut our_tokens: Vec<String> = Vec::new();
    let mut authorizations = order.authorizations();
    while let Some(authz_result) = authorizations.next().await {
        let mut authz = authz_result.context("ACME authorizations fetch failed")?;

        if authz.status == AuthorizationStatus::Valid {
            continue; // already validated from a previous attempt
        }

        let mut challenge_handle = authz
            .challenge(ChallengeType::Http01)
            .ok_or_else(|| anyhow::anyhow!("no HTTP-01 challenge available for domain {domain}"))?;

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
    let retry = RetryPolicy::default();
    order
        .poll_ready(&retry)
        .await
        .context("ACME order timed out waiting for challenge validation")?;

    // Shut down the temporary challenge server (best effort — errors are non-fatal).
    let _ = stop_tx.send(());

    // Remove only the tokens we inserted, preserving any tokens that belong
    // to concurrent ACME operations for other domains.
    for token in &our_tokens {
        challenges.remove(token);
    }

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
    std::fs::write(&creds_path, json)
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
        .route("/.well-known/acme-challenge/:token", get(challenge_handler))
        .with_state(challenges);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            stop_rx.await.ok();
        })
        .await
        .ok();
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
