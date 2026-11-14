//! The ACME account: where its credentials are stored (one file per CA directory, issue #555), restoring it and
//! creating it. Split out of `flow/mod.rs`, which crossed the 400-line soft limit.
use std::path::{Path, PathBuf};

use anyhow::Context;
use instant_acme::{Account, AccountCredentials, NewAccount};

use super::storage::write_secret_atomic;
use crate::config::AcmeConfig;

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
pub(super) fn fnv1a_64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Refuse a credentials file that records another CA's directory than the one asked for. instant-acme sends orders
/// to the directory stored in the credentials, so a file reached through a name collision (or copied by hand) would
/// silently send them to the wrong CA (issue #555).
pub(super) fn check_recorded_directory(
    json: &str,
    directory_url: &str,
    path: &Path,
) -> anyhow::Result<()> {
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
pub(super) fn account_credentials_path(storage: &Path, directory_url: &str) -> PathBuf {
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

/// `true` when an account for `directory_url` is already stored. A check that may end in "nothing to do" (the ARI
/// question of [`renewal`]) must not register a new account with the CA as a side effect.
pub(super) fn stored_account_exists(acme_cfg: &AcmeConfig, directory_url: &str) -> bool {
    let storage = acme_cfg.storage.as_deref().unwrap_or("./certs");
    account_credentials_path(Path::new(storage), directory_url).exists()
}

/// Load a persisted ACME account from `storage_dir/acme_account.json` (or the file of this directory, see
/// [`account_credentials_path`]), or create a new one with the ACME server and persist the credentials.
pub(super) async fn load_or_create_account(
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
