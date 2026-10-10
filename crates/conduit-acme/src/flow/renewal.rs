//! Background renewal of ACME certificates while the process runs (#491).
//!
//! One loop, sequential: the jobs are planned once at startup from the
//! listeners that were actually bound (the planner lives next to the server's
//! listener bookkeeping), and renewed one after another. Sequential matters:
//! it bounds the burst against the CA, and jobs that share a storage
//! directory (`./certs` vs `certs` spell the same files) can never interleave
//! their account or certificate writes.
//!
//! The renewed files take effect at the next restart — the running listeners
//! keep the certificate they loaded; hot-swapping it is not implemented yet.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior};

use super::{cache_state, load_or_obtain_with, AcmeCertPaths, CacheState, ChallengeSource};
use crate::config::AcmeConfig;

/// How often the certificates are checked.
const CHECK_INTERVAL: Duration = Duration::from_secs(12 * 3600);

/// One certificate to keep fresh.
#[derive(Debug, Clone)]
pub struct RenewalJob {
    pub acme: AcmeConfig,
    pub domain: String,
    pub storage_dir: PathBuf,
    /// How the HTTP-01 token reaches the CA. Renewal runs while the proxy
    /// listeners are up, so this is [`ChallengeSource::Shared`] whenever a
    /// listener already owns the challenge port — [`ChallengeSource::Bind`]
    /// would fail with `EADDRINUSE` every time.
    pub source: ChallengeSource,
}

/// Renew `job`'s certificate when it is due.
///
/// `Ok(None)`: nothing to do, the CA was not contacted. `Ok(Some(paths))`:
/// a new certificate was written (the hook for a later hot-swap).
pub async fn renew_if_due(
    job: &RenewalJob,
    challenges: &Arc<DashMap<String, String>>,
) -> anyhow::Result<Option<AcmeCertPaths>> {
    let cert = tokio::fs::read_to_string(job.storage_dir.join(format!("{}.crt.pem", job.domain)))
        .await
        .ok();
    let key = tokio::fs::read_to_string(job.storage_dir.join(format!("{}.key.pem", job.domain)))
        .await
        .ok();
    if cache_state(cert.as_deref(), key.as_deref()) == CacheState::Reusable {
        return Ok(None);
    }

    tracing::info!(domain = job.domain, "renewing ACME certificate");
    let paths = load_or_obtain_with(
        &job.acme,
        &job.domain,
        challenges.clone(),
        &job.storage_dir,
        job.source,
    )
    .await?;
    Ok(Some(paths))
}

/// Check every certificate every 12 hours until `shutdown` fires.
///
/// The first check is one interval after start: startup issuance has just run
/// the same check, and an immediate second order for a site whose issuance
/// failed seconds ago would only repeat the failure.
pub async fn run_renewal_loop(
    jobs: Vec<RenewalJob>,
    challenges: Arc<DashMap<String, String>>,
    shutdown: watch::Receiver<bool>,
) {
    run_loop(jobs, challenges, shutdown, CHECK_INTERVAL).await;
}

async fn run_loop(
    jobs: Vec<RenewalJob>,
    challenges: Arc<DashMap<String, String>>,
    mut shutdown: watch::Receiver<bool>,
    period: Duration,
) {
    if jobs.is_empty() {
        return;
    }
    let mut ticker = tokio::time::interval_at(Instant::now() + period, period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = ticker.tick() => {}
        }
        for job in &jobs {
            tokio::select! {
                _ = shutdown.changed() => return,
                result = renew_if_due(job, &challenges) => match result {
                    Ok(Some(_)) => tracing::info!(
                        domain = job.domain,
                        "ACME certificate renewed — it takes effect at the next restart"
                    ),
                    Ok(None) => {}
                    Err(e) => tracing::error!(
                        domain = job.domain,
                        error = %e,
                        "ACME certificate renewal failed"
                    ),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_pair(dir: &std::path::Path, domain: &str, days: i64) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec![domain.to_string()]).unwrap();
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(days);
        let cert = params.self_signed(&key).unwrap();
        std::fs::write(dir.join(format!("{domain}.crt.pem")), cert.pem()).unwrap();
        std::fs::write(dir.join(format!("{domain}.key.pem")), key.serialize_pem()).unwrap();
    }

    fn job(dir: &std::path::Path) -> RenewalJob {
        RenewalJob {
            acme: AcmeConfig {
                email: "ops@example.com".to_owned(),
                // A closed port: any attempt to order would fail loudly.
                directory: Some("http://127.0.0.1:1/directory".to_owned()),
                storage: None,
                challenge: None,
            },
            domain: "example.com".to_owned(),
            storage_dir: dir.to_path_buf(),
            source: ChallengeSource::Shared,
        }
    }

    #[tokio::test]
    async fn a_certificate_far_from_expiry_is_left_alone_without_contacting_the_ca() {
        let dir = tempfile::TempDir::new().unwrap();
        fresh_pair(dir.path(), "example.com", 365);
        let result = renew_if_due(&job(dir.path()), &Arc::new(DashMap::new())).await;
        assert!(matches!(result, Ok(None)), "got {result:?}");
    }

    #[tokio::test]
    async fn the_loop_exits_when_shutdown_fires() {
        let dir = tempfile::TempDir::new().unwrap();
        fresh_pair(dir.path(), "example.com", 365);
        let (tx, rx) = watch::channel(false);
        let task = tokio::spawn(run_loop(
            vec![job(dir.path())],
            Arc::new(DashMap::new()),
            rx,
            Duration::from_secs(3600),
        ));
        tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("the loop must stop promptly on shutdown")
            .unwrap();
    }

    #[tokio::test]
    async fn no_jobs_means_the_loop_returns_immediately() {
        let (_tx, rx) = watch::channel(false);
        tokio::time::timeout(
            Duration::from_secs(2),
            run_loop(
                Vec::new(),
                Arc::new(DashMap::new()),
                rx,
                Duration::from_secs(3600),
            ),
        )
        .await
        .expect("an empty job list must not park a task");
    }
}
