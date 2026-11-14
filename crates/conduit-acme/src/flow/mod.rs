//! ACME (Let's Encrypt) HTTP-01 certificate flow.
//!
//! Compiled only with this crate's own `acme` Cargo feature — mirrors the
//! pre-extraction `#![cfg(feature = "acme")]` file-level gate on
//! `src/server/acme.rs` (issue #114/#130). The root crate's `src/server/acme.rs`
//! is now a thin facade re-exporting this module's public items.
//!
//! Submodules: [`http01`] (challenge server + port locks), [`storage`]
//! (owner-only secret writes), [`renewal`] (the background renewal task).
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use dashmap::DashMap;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt,
    NewAccount, NewOrder, RetryPolicy,
};

use crate::config::AcmeConfig;

mod http01;
mod renewal;
mod storage;

pub use http01::ChallengeSource;
pub use renewal::{renew_if_due, run_renewal_loop, RenewalJob};

use http01::ChallengeServer;
use storage::{
    cached_pair_matches, pair_paths, unsupported_key_format, write_secret_atomic, PendingPair,
};

/// The most days before expiry at which renewal starts. A certificate with a shorter life renews earlier in proportion
/// (see [`cert_renewal_due`]).
const RENEWAL_THRESHOLD_DAYS: i64 = 30;

/// On-disk paths to a site's TLS certificate and private key obtained via ACME.
#[derive(Debug, Clone)]
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

/// `true` when the first certificate in `cert_pem` is due for renewal at `now` (seconds since the Unix epoch): when
/// no more than a third of its lifetime is left, and never later than [`RENEWAL_THRESHOLD_DAYS`] before expiry.
///
/// A fixed 30 days is right for a 90-day certificate (a third of it) and wrong for the shorter ones the CAs are moving to
/// (issue #594): 64-day certificates renew with 21 days left, 45-day ones with 15, a 6-day one with 2 (a fixed
/// threshold would make that one due on every check, a new order every 12 hours). A parse error, an empty or inverted
/// validity period counts as due, so a certificate that cannot be read is replaced rather than kept.
pub fn cert_renewal_due(cert_pem: &str, now: i64) -> bool {
    use x509_parser::pem::parse_x509_pem;
    let Ok((_, pem)) = parse_x509_pem(cert_pem.as_bytes()) else {
        return true;
    };
    let Ok(x509) = pem.parse_x509() else {
        return true;
    };
    let not_before = x509.validity().not_before.timestamp();
    let not_after = x509.validity().not_after.timestamp();
    let lifetime = not_after - not_before;
    if lifetime <= 0 {
        return true;
    }
    let threshold = (lifetime / 3).min(RENEWAL_THRESHOLD_DAYS * 86_400);
    now >= not_after - threshold
}

fn unix_now() -> i64 {
    std::time::SystemTime::UNIX_EPOCH
        .elapsed()
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// What the files already on disk are good for.
#[derive(Debug, PartialEq, Eq)]
enum CacheState {
    /// A matching pair, not within the renewal threshold.
    Reusable,
    /// A matching pair that should be renewed.
    Expiring,
    /// Unreadable, unparseable, or the key is not the certificate's own.
    Unusable,
}

fn cache_state(cert_pem: Option<&str>, key_pem: Option<&str>) -> CacheState {
    match (cert_pem, key_pem) {
        (Some(cert), Some(key)) if cached_pair_matches(cert, key) => {
            if cert_renewal_due(cert, unix_now()) {
                CacheState::Expiring
            } else {
                CacheState::Reusable
            }
        }
        _ => CacheState::Unusable,
    }
}

/// Load a cached certificate from `storage_dir`, or run the full ACME flow to
/// obtain a fresh one.
///
/// If valid cert/key files already exist and the certificate is not expiring
/// due for renewal ([`cert_renewal_due`]), the cached paths are returned
/// without contacting the ACME server.
///
/// `challenges` is the shared HTTP-01 challenge store; it is populated during
/// the ACME flow so that the challenge handler can serve token responses.
///
/// `http_challenge_port` is the local TCP port on which the temporary HTTP-01
/// challenge server listens during certificate procurement (typically 80).
///
/// When the order fails but the cached certificate is still valid (renewal
/// starts 30 days ahead), the cached files are returned with a warning rather
/// than an error.
pub async fn load_or_obtain_certificate(
    acme_cfg: &AcmeConfig,
    domain: &str,
    challenges: Arc<DashMap<String, String>>,
    storage_dir: &Path,
    http_challenge_port: u16,
) -> anyhow::Result<AcmeCertPaths> {
    let result = load_or_obtain_with(
        acme_cfg,
        domain,
        challenges,
        storage_dir,
        ChallengeSource::Bind(http_challenge_port),
    )
    .await;

    // Renewal is attempted 30 days ahead, so a failed order (CA outage, rate
    // limit) with a certificate that is still valid must not drop the site to
    // plain HTTP on its TLS port: keep serving the cached one.
    match result {
        Err(e) => {
            // An unusable domain name failed above; there is no cached pair to fall back to.
            let Ok((cert_path, key_path)) = pair_paths(storage_dir, domain) else {
                return Err(e);
            };
            let cert = tokio::fs::read_to_string(&cert_path).await.ok();
            let key = tokio::fs::read_to_string(&key_path).await.ok();
            if cached_pair_is_valid_now(cert.as_deref(), key.as_deref()) {
                tracing::error!(
                    domain,
                    error = %e,
                    "ACME order failed — serving the cached certificate until it expires"
                );
                Ok(AcmeCertPaths {
                    cert: cert_path,
                    key: key_path,
                })
            } else {
                Err(e)
            }
        }
        ok => ok,
    }
}

/// A matching cert/key pair whose certificate has not expired yet.
fn cached_pair_is_valid_now(cert_pem: Option<&str>, key_pem: Option<&str>) -> bool {
    match (cert_pem, key_pem) {
        (Some(cert), Some(key)) => {
            cached_pair_matches(cert, key) && !cert_expires_within_days(cert, 0)
        }
        _ => false,
    }
}

/// [`load_or_obtain_certificate`] with an explicit [`ChallengeSource`].
async fn load_or_obtain_with(
    acme_cfg: &AcmeConfig,
    domain: &str,
    challenges: Arc<DashMap<String, String>>,
    storage_dir: &Path,
    challenge_source: ChallengeSource,
) -> anyhow::Result<AcmeCertPaths> {
    // Check the name before touching the disk: an invalid host must not even create the directory.
    let (cert_path, key_path) = pair_paths(storage_dir, domain)
        .with_context(|| format!("invalid ACME domain for site host {domain:?}"))?;

    tokio::fs::create_dir_all(storage_dir)
        .await
        .with_context(|| format!("creating ACME storage directory {storage_dir:?}"))?;

    // Reuse the cached certificate when it is not about to expire *and* the
    // key on disk is the one it was issued for. A crash between the two
    // renames of a previous write (or a hand-edited file) can leave a
    // mismatched pair, which Pingora's rustls setup would turn into a panic
    // at start; re-ordering heals it instead.
    if cert_path.exists() && key_path.exists() {
        let cert_pem = tokio::fs::read_to_string(&cert_path).await.ok();
        let key_pem = tokio::fs::read_to_string(&key_path).await.ok();
        match cache_state(cert_pem.as_deref(), key_pem.as_deref()) {
            CacheState::Reusable => {
                tracing::info!(domain, "reusing cached ACME certificate");
                return Ok(AcmeCertPaths {
                    cert: cert_path,
                    key: key_path,
                });
            }
            CacheState::Expiring => {
                tracing::info!(domain, "ACME certificate expires soon — renewing");
            }
            CacheState::Unusable => match key_pem.as_deref().and_then(unsupported_key_format) {
                Some(format) => tracing::warn!(
                    domain,
                    format,
                    "the private key in {key_path:?} is {format}: only a PKCS#8 `PRIVATE KEY` is read, \
                     so this key is not used. A new key and certificate are being ordered and will replace \
                     the file; convert the key with `openssl pkcs8 -topk8 -nocrypt` to keep it"
                ),
                None => tracing::warn!(
                    domain,
                    "cached ACME certificate and key are unreadable or do not match — \
                     obtaining a new certificate"
                ),
            },
        }
    } else {
        tracing::info!(domain, "obtaining ACME certificate for the first time");
    }

    // Stage the output files *before* contacting the CA: an unwritable
    // storage directory must fail here, not after an order was issued and
    // thrown away (each such order counts against the CA's rate limits).
    let pending = PendingPair::create(storage_dir, domain)
        .with_context(|| format!("preparing to write ACME files in {storage_dir:?}"))?;

    let (cert_pem, key_pem) =
        run_acme_order(acme_cfg, domain, &challenges, challenge_source).await?;

    // Synchronous and cancellation-safe: both files are written, `fsync`ed
    // and renamed into place without an `.await` in between. The key is
    // owner-only from creation (issue #278).
    pending
        .commit(&cert_pem, &key_pem)
        .with_context(|| format!("writing ACME certificate and key in {storage_dir:?}"))?;

    Ok(AcmeCertPaths {
        cert: cert_path,
        key: key_path,
    })
}

/// Run the complete ACME HTTP-01 flow and return `(cert_chain_pem, key_pem)`.
async fn run_acme_order(
    acme_cfg: &AcmeConfig,
    domain: &str,
    challenges: &Arc<DashMap<String, String>>,
    challenge_source: ChallengeSource,
) -> anyhow::Result<(String, String)> {
    let directory_url = acme_cfg
        .directory
        .as_deref()
        .unwrap_or_else(|| LetsEncrypt::Production.url())
        .to_owned();

    let account = load_or_create_account(acme_cfg, &directory_url).await?;

    let identifiers = [Identifier::Dns(domain.to_string())];
    let new_order = match acme_cfg.profile.as_deref() {
        Some(profile) => NewOrder::new(&identifiers).profile(profile),
        None => NewOrder::new(&identifiers),
    };
    let mut order = account
        .new_order(&new_order)
        .await
        .context("ACME new-order request failed")?;

    // `Bind`: take the per-port lock and bind the challenge port before
    // anything else, so a failure is an ACME error. `Shared`: a running
    // listener serves the tokens, nothing to start.
    let challenge_server = ChallengeServer::start(challenge_source, challenges).await?;

    // Populate challenge tokens and notify the CA to begin validation.
    // Track every token we insert so we can remove only our own entries later,
    // leaving tokens belonging to other concurrent ACME operations intact.
    //
    // Issue #279: every fallible step below used to propagate its error via
    // `?` directly, which skipped the cleanup further down (shutdown signal,
    // awaiting the server task, removing our tokens) on any early return —
    // leaking the bound port (the spawned task keeps holding it even though
    // the port lock gets dropped) and leaving stale challenge tokens in the
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
    if let Some(server) = challenge_server {
        server.stop().await;
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

/// FNV-1a, 64 bit: a hash that never changes between Rust versions (`DefaultHasher` may), for a file name.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Refuse a credentials file that records another CA's directory than the one asked for. instant-acme sends orders
/// to the directory stored in the credentials, so a file reached through a name collision (or copied by hand) would
/// silently send them to the wrong CA (issue #555).
fn check_recorded_directory(json: &str, directory_url: &str, path: &Path) -> anyhow::Result<()> {
    let recorded = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| {
            v.get("directory")
                .and_then(|d| d.as_str())
                .map(str::to_owned)
        });
    match recorded {
        Some(dir) if dir != directory_url => anyhow::bail!(
            "ACME account file {path:?} belongs to the directory {dir}, not {directory_url}; move the file away so \
             that an account is created for this directory"
        ),
        _ => Ok(()),
    }
}

/// Where the account for `directory_url` is kept (issue #555). `acme_account.json` is the account of the CA it was created
/// for (the credentials record the directory URL); another directory, such as production after staging, gets its own
/// file instead of being tried against the first CA's account. A file with no recorded directory (an older version's)
/// is used as before.
fn account_credentials_path(storage: &Path, directory_url: &str) -> PathBuf {
    let default = storage.join("acme_account.json");
    let recorded = std::fs::read_to_string(&default)
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|v| {
            v.get("directory")
                .and_then(|d| d.as_str())
                .map(str::to_owned)
        });
    match recorded {
        Some(dir) if dir != directory_url => {
            // The slug only makes the name readable; the hash of the whole URL keeps two directories that slug alike
            // (`https://a.b/dir`, `https://a_b/dir`, or one long shared prefix) apart.
            let slug: String = directory_url
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .take(48)
                .collect();
            storage.join(format!(
                "acme_account.{slug}.{:016x}.json",
                fnv1a_64(directory_url.as_bytes())
            ))
        }
        _ => default,
    }
}

/// Load a persisted ACME account from `storage_dir/acme_account.json` (or the file of this directory, see
/// [`account_credentials_path`]), or create a new one with the ACME server and persist the credentials.
async fn load_or_create_account(
    acme_cfg: &AcmeConfig,
    directory_url: &str,
) -> anyhow::Result<Account> {
    let storage = acme_cfg.storage.as_deref().unwrap_or("./certs");
    let creds_path = account_credentials_path(Path::new(storage), directory_url);

    if creds_path.exists() {
        let json = tokio::fs::read_to_string(&creds_path)
            .await
            .with_context(|| format!("reading ACME credentials from {creds_path:?}"))?;
        check_recorded_directory(&json, directory_url, &creds_path)?;
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

    tokio::fs::create_dir_all(storage).await?;
    let json = serde_json::to_string_pretty(&credentials)?;
    // Owner-only permissions: this file holds the ACME account's private
    // key material (issue #278).
    write_secret_atomic(&creds_path, json.as_bytes())
        .with_context(|| format!("saving ACME credentials to {creds_path:?}"))?;
    tracing::info!("new ACME account created and saved to {creds_path:?}");

    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directories_that_slug_alike_get_different_account_files() {
        let dir = tempfile::tempdir().unwrap();
        // the default file belongs to another CA, so both URLs below get a file of their own
        std::fs::write(
            dir.path().join("acme_account.json"),
            r#"{"directory":"https://other.example/dir"}"#,
        )
        .unwrap();
        let a = account_credentials_path(dir.path(), "https://a.b/dir");
        let b = account_credentials_path(dir.path(), "https://a_b/dir");
        assert_ne!(a, b);
        let long = format!("https://ca.example/{}", "x".repeat(200));
        assert_ne!(
            account_credentials_path(dir.path(), &format!("{long}1")),
            account_credentials_path(dir.path(), &format!("{long}2")),
            "a shared prefix longer than the slug"
        );
        // the same URL always maps to the same file
        assert_eq!(a, account_credentials_path(dir.path(), "https://a.b/dir"));
        assert_eq!(fnv1a_64(b"abc"), 0xe71f_a219_0541_574b);
    }

    #[test]
    fn a_credentials_file_for_another_directory_is_refused() {
        let path = Path::new("/tmp/acme_account.json");
        let json = r#"{"directory":"https://one.example/dir","id":"x"}"#;
        assert!(check_recorded_directory(json, "https://one.example/dir", path).is_ok());
        let err = check_recorded_directory(json, "https://two.example/dir", path)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("one.example") && err.contains("two.example"),
            "{err}"
        );
        // a file with no recorded directory (an older version's) is used as before
        assert!(check_recorded_directory(r#"{"id":"x"}"#, "https://two.example/dir", path).is_ok());
    }

    /// A self-signed certificate that is valid for `lifetime_days` days, `days_left` of which remain at `now`.
    fn cert_with_life(lifetime_days: i64, days_left: i64, now: i64) -> String {
        let key = rcgen::KeyPair::generate().expect("keygen");
        let mut params =
            rcgen::CertificateParams::new(vec!["example.com".to_string()]).expect("params");
        let not_after = now + days_left * 86_400;
        params.not_after = time::OffsetDateTime::from_unix_timestamp(not_after).expect("not_after");
        params.not_before =
            time::OffsetDateTime::from_unix_timestamp(not_after - lifetime_days * 86_400)
                .expect("not_before");
        params.self_signed(&key).expect("self-signed").pem()
    }

    #[test]
    fn renewal_starts_at_a_third_of_the_lifetime_and_never_later_than_30_days_before_expiry() {
        let now = 1_900_000_000;
        // (lifetime, days left, due?)
        for (life, left, due) in [
            (90, 31, false),
            (90, 29, true), // today's behaviour for a 90-day certificate
            (64, 22, false),
            (64, 20, true), // a third of 64 days is 21
            (45, 16, false),
            (45, 14, true), // a third of 45 days is 15
            (6, 3, false),
            (6, 1, true), // a 6-day certificate is not due on its first day
            (6, 6, false),
            (398, 31, false),
            (398, 29, true), // longer than 90 days: still the 30-day cap
        ] {
            let pem = cert_with_life(life, left, now);
            assert_eq!(
                cert_renewal_due(&pem, now),
                due,
                "{life}-day certificate, {left} days left"
            );
        }
        assert!(
            cert_renewal_due("not a certificate", now),
            "an unreadable certificate is due"
        );
    }

    #[test]
    fn the_account_file_follows_the_directory_it_was_created_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        let staging = "https://acme-staging-v02.api.letsencrypt.org/directory";
        let prod = "https://acme-v02.api.letsencrypt.org/directory";
        // nothing stored yet: the default file, whatever the directory
        assert_eq!(
            account_credentials_path(dir.path(), prod),
            dir.path().join("acme_account.json")
        );
        // an account created for staging
        std::fs::write(
            dir.path().join("acme_account.json"),
            format!(r#"{{"id":"https://x/acct/1","key_pkcs8":"AA","directory":"{staging}"}}"#),
        )
        .expect("write");
        assert_eq!(
            account_credentials_path(dir.path(), staging),
            dir.path().join("acme_account.json")
        );
        let other = account_credentials_path(dir.path(), prod);
        assert_ne!(
            other,
            dir.path().join("acme_account.json"),
            "production must not reuse staging's account"
        );
        assert!(other
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("acme_account.")
                && n.ends_with(".json")
                && !n.contains('/')));
        // an older file with no recorded directory is used as before
        std::fs::write(
            dir.path().join("acme_account.json"),
            r#"{"id":"https://x/acct/1","key_pkcs8":"AA"}"#,
        )
        .expect("write");
        assert_eq!(
            account_credentials_path(dir.path(), prod),
            dir.path().join("acme_account.json")
        );
    }

    /// Build a self-signed cert + key PEM pair with a caller-controlled
    /// `not_after`, so tests can exercise both "far from expiry" and
    /// "expiring soon" branches of `cert_expires_within_days` deterministically.
    fn self_signed_pair_with_not_after(not_after: time::OffsetDateTime) -> (String, String) {
        let key_pair = rcgen::KeyPair::generate().expect("keygen");
        let mut params =
            rcgen::CertificateParams::new(vec!["example.com".to_string()]).expect("params");
        params.not_after = not_after;
        let cert = params.self_signed(&key_pair).expect("self-signed cert");
        (cert.pem(), key_pair.serialize_pem())
    }

    fn self_signed_cert_with_not_after(not_after: time::OffsetDateTime) -> String {
        self_signed_pair_with_not_after(not_after).0
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

    /// Exercises the storage-directory creation and cached-certificate read in
    /// `load_or_obtain_certificate`. The ACME directory points at a closed
    /// port, so the call can only succeed by taking the "reuse the cached
    /// certificate" branch — falling through to the ACME flow would fail.
    #[tokio::test]
    async fn fresh_cached_certificate_is_reused_without_contacting_the_acme_server() {
        let dir = tempfile::TempDir::new().unwrap();
        let storage_dir = dir.path().join("certs");
        std::fs::create_dir_all(&storage_dir).unwrap();
        let (cert_pem, key_pem) = self_signed_pair_with_not_after(
            time::OffsetDateTime::now_utc() + time::Duration::days(365),
        );
        std::fs::write(storage_dir.join("example.com.crt.pem"), &cert_pem).unwrap();
        std::fs::write(storage_dir.join("example.com.key.pem"), &key_pem).unwrap();

        let cfg = AcmeConfig {
            email: "ops@example.com".to_string(),
            directory: Some("http://127.0.0.1:1/directory".to_string()),
            storage: None,
            challenge: None,
            profile: None,
        };
        let paths = load_or_obtain_certificate(
            &cfg,
            "example.com",
            Arc::new(DashMap::new()),
            &storage_dir,
            0,
        )
        .await
        .expect("a fresh cached certificate must be reused without contacting the ACME server");

        assert_eq!(paths.cert, storage_dir.join("example.com.crt.pem"));
        assert_eq!(paths.key, storage_dir.join("example.com.key.pem"));
    }
    /// A cached cert whose key does not belong to it (a crash between the two
    /// renames of an earlier write) must not be handed to the TLS stack: it is
    /// re-ordered instead.
    #[test]
    fn cache_state_classifies_the_files_on_disk() {
        let far = time::OffsetDateTime::now_utc() + time::Duration::days(365);
        let soon = time::OffsetDateTime::now_utc() + time::Duration::days(5);
        let (cert, key) = self_signed_pair_with_not_after(far);
        let (_, other_key) = self_signed_pair_with_not_after(far);
        let (expiring_cert, expiring_key) = self_signed_pair_with_not_after(soon);

        assert_eq!(cache_state(Some(&cert), Some(&key)), CacheState::Reusable);
        assert_eq!(
            cache_state(Some(&expiring_cert), Some(&expiring_key)),
            CacheState::Expiring
        );
        assert_eq!(
            cache_state(Some(&cert), Some(&other_key)),
            CacheState::Unusable,
            "a mismatched pair must be re-ordered, not reused"
        );
        assert_eq!(cache_state(Some(&cert), None), CacheState::Unusable);
        assert_eq!(cache_state(None, Some(&key)), CacheState::Unusable);
    }
    #[test]
    fn a_failed_renewal_may_fall_back_only_to_a_valid_matching_pair() {
        let now = time::OffsetDateTime::now_utc();
        let (valid_cert, valid_key) =
            self_signed_pair_with_not_after(now + time::Duration::days(5));
        let (expired_cert, expired_key) =
            self_signed_pair_with_not_after(now - time::Duration::days(1));
        let (_, other_key) = self_signed_pair_with_not_after(now + time::Duration::days(5));

        // Inside the renewal window but not expired: still served.
        assert!(cached_pair_is_valid_now(
            Some(&valid_cert),
            Some(&valid_key)
        ));
        assert!(!cached_pair_is_valid_now(
            Some(&expired_cert),
            Some(&expired_key)
        ));
        assert!(!cached_pair_is_valid_now(
            Some(&valid_cert),
            Some(&other_key)
        ));
        assert!(!cached_pair_is_valid_now(Some(&valid_cert), None));
        assert!(!cached_pair_is_valid_now(None, None));
    }
    /// instant-acme's HTTP client needs a process-level rustls provider; the
    /// binary installs `ring` in `run_server`, tests that reach the CA do it
    /// here.
    fn install_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    /// A directory nothing listens on: any order fails fast with a connect error.
    fn unreachable_ca() -> AcmeConfig {
        AcmeConfig {
            email: "ops@example.com".to_string(),
            directory: Some("http://127.0.0.1:1/directory".to_string()),
            storage: None,
            challenge: None,
            profile: None,
        }
    }

    fn write_pair(dir: &Path, not_after: time::OffsetDateTime) -> (String, String) {
        let (cert, key) = self_signed_pair_with_not_after(not_after);
        std::fs::write(dir.join("example.com.crt.pem"), &cert).unwrap();
        std::fs::write(dir.join("example.com.key.pem"), &key).unwrap();
        (cert, key)
    }

    /// Renewal starts 30 days ahead, so a CA outage inside that window must not
    /// take the site down: the still-valid cached pair keeps being served.
    #[tokio::test]
    async fn failed_order_falls_back_to_a_still_valid_cached_pair() {
        install_crypto_provider();
        let dir = tempfile::TempDir::new().unwrap();
        let (cert, key) = write_pair(
            dir.path(),
            time::OffsetDateTime::now_utc() + time::Duration::days(5),
        );

        let paths = load_or_obtain_certificate(
            &unreachable_ca(),
            "example.com",
            Arc::new(DashMap::new()),
            dir.path(),
            0,
        )
        .await
        .expect("a valid cached certificate must survive a failed renewal");

        assert_eq!(std::fs::read_to_string(&paths.cert).unwrap(), cert);
        assert_eq!(std::fs::read_to_string(&paths.key).unwrap(), key);
        assert!(!dir.path().join("example.com.key.pem.tmp").exists());
    }

    #[tokio::test]
    async fn failed_order_with_an_expired_cached_pair_is_an_error() {
        install_crypto_provider();
        let dir = tempfile::TempDir::new().unwrap();
        write_pair(
            dir.path(),
            time::OffsetDateTime::now_utc() - time::Duration::days(1),
        );

        let result = load_or_obtain_certificate(
            &unreachable_ca(),
            "example.com",
            Arc::new(DashMap::new()),
            dir.path(),
            0,
        )
        .await;

        assert!(result.is_err(), "an expired certificate must not be served");
        assert!(!dir.path().join("example.com.crt.pem.tmp").exists());
        assert!(!dir.path().join("example.com.key.pem.tmp").exists());
    }

    #[tokio::test]
    async fn an_unusable_storage_location_fails_before_any_order() {
        // `create_dir_all` over an existing *file* fails before the CA (here a
        // closed port, which would otherwise be the reported error) is tried.
        let dir = tempfile::TempDir::new().unwrap();
        let blocker = dir.path().join("certs");
        std::fs::write(&blocker, "not a directory").unwrap();

        let err = load_or_obtain_certificate(
            &unreachable_ca(),
            "example.com",
            Arc::new(DashMap::new()),
            &blocker,
            0,
        )
        .await
        .expect_err("storage dir is a file");
        assert!(err.to_string().contains("creating ACME storage directory"));
    }

    /// Issue #554: the site host names the files, so a host such as `../x` must not reach a
    /// cached pair that sits outside the storage directory, nor create anything on disk.
    #[tokio::test]
    async fn a_host_that_is_not_a_plain_dns_name_is_refused_before_any_disk_or_ca_access() {
        let dir = tempfile::TempDir::new().unwrap();
        let storage = dir.path().join("certs");
        std::fs::create_dir(&storage).unwrap();
        // A perfectly good pair that `certs/../x.*.pem` points at, one level above the storage dir.
        let (cert, key) = self_signed_pair_with_not_after(
            time::OffsetDateTime::now_utc() + time::Duration::days(365),
        );
        std::fs::write(dir.path().join("x.crt.pem"), cert).unwrap();
        std::fs::write(dir.path().join("x.key.pem"), key).unwrap();

        let err = load_or_obtain_certificate(
            &unreachable_ca(),
            "../x",
            Arc::new(DashMap::new()),
            &storage,
            0,
        )
        .await
        .expect_err("a path-like host must be refused, not served from the cache");
        assert!(
            format!("{err:#}").contains("invalid ACME domain"),
            "got: {err:#}"
        );
        assert_eq!(std::fs::read_dir(&storage).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn an_invalid_host_does_not_even_create_the_storage_directory() {
        let dir = tempfile::TempDir::new().unwrap();
        let storage = dir.path().join("not-yet");
        let err = load_or_obtain_certificate(
            &unreachable_ca(),
            "a/b",
            Arc::new(DashMap::new()),
            &storage,
            0,
        )
        .await
        .expect_err("invalid host");
        assert!(format!("{err:#}").contains("invalid ACME domain"));
        assert!(!storage.exists());
    }
}
