//! Background renewal of ACME certificates while the process runs (#491).
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;

use super::{
    cert_expires_within_days, load_or_obtain_with, ChallengeSource, RENEWAL_THRESHOLD_DAYS,
};
use crate::config::AcmeConfig;

/// Spawn a background task that renews the certificate for `domain` when it is
/// within [`RENEWAL_THRESHOLD_DAYS`] days of expiry.
///
/// Checks every 12 hours. The renewed files are written to `storage_dir`; the
/// running process keeps serving the certificate it loaded at startup.
///
/// Renewal runs while the proxy listeners are up, so pass
/// [`ChallengeSource::Shared`] when a listener already owns the HTTP-01 port —
/// [`ChallengeSource::Bind`] would then fail with `EADDRINUSE` every time.
pub fn spawn_renewal_task(
    acme_cfg: AcmeConfig,
    domain: String,
    challenges: Arc<DashMap<String, String>>,
    storage_dir: PathBuf,
    challenge_source: ChallengeSource,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(12 * 3600));
        loop {
            interval.tick().await;

            let cert_path = storage_dir.join(format!("{domain}.crt.pem"));
            if cert_path.exists() {
                match tokio::fs::read_to_string(&cert_path).await {
                    Ok(pem) if !cert_expires_within_days(&pem, RENEWAL_THRESHOLD_DAYS) => {
                        continue; // not expiring soon
                    }
                    _ => {}
                }
            }

            tracing::info!(domain, "renewing ACME certificate");
            match load_or_obtain_with(
                &acme_cfg,
                &domain,
                challenges.clone(),
                &storage_dir,
                challenge_source,
            )
            .await
            {
                Ok(_) => tracing::info!(domain, "ACME certificate renewed successfully"),
                Err(e) => tracing::error!(domain, error = %e, "ACME certificate renewal failed"),
            }
        }
    });
}
