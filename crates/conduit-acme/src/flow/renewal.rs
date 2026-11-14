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

use anyhow::Context;
use dashmap::DashMap;
use tokio::sync::watch;
use tokio::time::Instant;

use super::{
    ari, cache_state, load_or_create_account, load_or_obtain_with, pair_paths,
    stored_account_exists, unix_now, AcmeCertPaths, CacheState, ChallengeSource,
};
use crate::config::{AcmeConfig, AcmeTimings};

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
    renew_scheduled(job, challenges, &mut None).await
}

/// [`renew_if_due`] that honours the CA's wish about when to be asked again: `ask_ca_at` is the earliest moment the
/// CA wants another renewal-information request for this certificate (RFC 9773 section 4.3.2). Before it the CA is not
/// asked; the lifetime rule is still checked, it costs nothing. A probe without usable advice leaves it `None`.
async fn renew_scheduled(
    job: &RenewalJob,
    challenges: &Arc<DashMap<String, String>>,
    ask_ca_at: &mut Option<Instant>,
) -> anyhow::Result<Option<AcmeCertPaths>> {
    let (cert_path, key_path) = pair_paths(&job.storage_dir, &job.domain)
        .with_context(|| format!("invalid ACME domain for site host {:?}", job.domain))?;
    let cert = tokio::fs::read_to_string(&cert_path).await.ok();
    let key = tokio::fs::read_to_string(&key_path).await.ok();
    let mut forced = false;
    if cache_state(cert.as_deref(), key.as_deref()) == CacheState::Reusable {
        // Not due by the lifetime rule: the CA may still want it renewed earlier (RFC 9773, issue #594).
        if ask_ca_at.is_some_and(|at| Instant::now() < at) {
            return Ok(None);
        }
        let advice = ca_advice(job, cert.as_deref().unwrap_or_default()).await;
        *ask_ca_at = advice.as_ref().map(|a| Instant::now() + a.ask_again_in);
        if !advice.is_some_and(|a| a.renew_now) {
            return Ok(None);
        }
        forced = true;
    }

    tracing::info!(domain = job.domain, "renewing ACME certificate");
    let paths = load_or_obtain_with(
        &job.acme,
        &job.domain,
        challenges.clone(),
        &job.storage_dir,
        job.source,
        forced,
    )
    .await;
    // A new certificate: what the CA said about the old one no longer applies. A failed order: ask again at the next
    // check, as without ARI, not only when the CA's hint runs out (it can be a day away).
    *ask_ca_at = None;
    Ok(Some(paths?))
}

/// The CA's renewal information (ARI) for `cert_pem`. Any failure, including a directory without ARI and an account
/// that cannot be restored, is `None`: the lifetime rule decides alone.
async fn ca_advice(job: &RenewalJob, cert_pem: &str) -> Option<ari::Advice> {
    let directory_url = job
        .acme
        .directory
        .as_deref()
        .unwrap_or_else(|| instant_acme::LetsEncrypt::Production.url());
    if !stored_account_exists(&job.acme, directory_url) {
        // No account yet: the first order creates it. Asking about a certificate that is not due must not.
        return None;
    }
    let probe = async {
        let account = match load_or_create_account(&job.acme, directory_url).await {
            Ok(account) => account,
            Err(e) => {
                tracing::warn!(domain = job.domain, error = %e, "cannot ask the CA for renewal information");
                return None;
            }
        };
        ari::advice(&account, cert_pem, unix_now(), &job.acme.timings()).await
    };
    tokio::time::timeout(job.acme.timings().ari_probe, probe)
        .await
        .unwrap_or_else(|_| {
            tracing::warn!(
                domain = job.domain,
                "the CA did not answer the renewal information probe in time"
            );
            None
        })
}

/// Check every certificate every `tls.acme.checkIntervalSecs` (12 hours by default) until `shutdown` fires; sooner when a
/// CA's renewal information (`Retry-After`) asks to be consulted again earlier (a hint never brings a check closer
/// than `retryAfterMinSecs`, 5 minutes by default).
///
/// The first check is one interval after start: startup issuance has just run
/// the same check, and an immediate second order for a site whose issuance
/// failed seconds ago would only repeat the failure.
pub async fn run_renewal_loop(
    jobs: Vec<RenewalJob>,
    challenges: Arc<DashMap<String, String>>,
    shutdown: watch::Receiver<bool>,
) {
    let period = check_period(&jobs);
    run_loop(jobs, challenges, shutdown, period).await;
}

/// One loop serves every site: the shortest configured `checkIntervalSecs` wins (12 hours when none is set).
fn check_period(jobs: &[RenewalJob]) -> Duration {
    jobs.iter()
        .map(|job| job.acme.timings().check_interval)
        .min()
        .unwrap_or_else(|| AcmeTimings::default().check_interval)
}

/// What the loop knows about one job between checks.
#[derive(Clone, Copy)]
struct JobState {
    /// Not checked before this moment: `period` after the last check, or earlier when the CA's hint asks for it.
    next_check: Instant,
    /// The earliest moment the CA wants to be asked about the certificate again.
    ask_ca_at: Option<Instant>,
}

impl JobState {
    fn new(now: Instant, period: Duration) -> Self {
        Self {
            next_check: now + period,
            ask_ca_at: None,
        }
    }

    /// Schedule the next check after one that ran at `now`: `period` away, or sooner when the CA wants to be asked
    /// again before that. A job without a hint (no ARI, an error, a failed order) waits the full period, whatever
    /// the other jobs do.
    fn checked(&mut self, now: Instant, period: Duration) {
        let wait = self
            .ask_ca_at
            .map_or(period, |at| at.saturating_duration_since(now).min(period));
        self.next_check = now + wait;
    }
}

/// Run the jobs that are due at `now`, and only those. `false` when `shutdown` fired meanwhile.
async fn check_due(
    jobs: &[RenewalJob],
    challenges: &Arc<DashMap<String, String>>,
    states: &mut [JobState],
    period: Duration,
    now: Instant,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    for (job, state) in jobs.iter().zip(states.iter_mut()) {
        if state.next_check > now {
            continue;
        }
        tokio::select! {
            _ = shutdown.changed() => return false,
            result = renew_scheduled(job, challenges, &mut state.ask_ca_at) => match result {
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
        state.checked(Instant::now(), period);
    }
    true
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
    let mut states = vec![JobState::new(Instant::now(), period); jobs.len()];

    loop {
        let wake_at = states
            .iter()
            .map(|s| s.next_check)
            .min()
            .unwrap_or_else(|| Instant::now() + period);
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = tokio::time::sleep_until(wake_at) => {}
        }
        if !check_due(
            &jobs,
            &challenges,
            &mut states,
            period,
            Instant::now(),
            &mut shutdown,
        )
        .await
        {
            return;
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
                profile: None,
                ..Default::default()
            },
            domain: "example.com".to_owned(),
            storage_dir: dir.to_path_buf(),
            source: ChallengeSource::Shared,
        }
    }

    #[tokio::test]
    async fn a_certificate_far_from_expiry_is_left_alone_when_the_ca_cannot_be_asked() {
        // The CA is asked for renewal information (ARI) even for a certificate that is not due by its lifetime;
        // an unreachable CA is no advice, not an error, and no order is placed.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tempfile::TempDir::new().unwrap();
        fresh_pair(dir.path(), "example.com", 365);
        let result = renew_if_due(&job(dir.path()), &Arc::new(DashMap::new())).await;
        assert!(matches!(result, Ok(None)));
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
    fn expiring_job(dir: &std::path::Path) -> RenewalJob {
        let _ = rustls::crypto::ring::default_provider().install_default();
        fresh_pair(dir, "example.com", 5);
        job(dir)
    }

    #[tokio::test]
    async fn a_due_certificate_with_an_unreachable_ca_is_an_error_and_keeps_the_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let job = expiring_job(dir.path());
        let before = std::fs::read(dir.path().join("example.com.key.pem")).unwrap();

        let result = renew_if_due(&job, &Arc::new(DashMap::new())).await;

        assert!(result.is_err());
        assert_eq!(
            std::fs::read(dir.path().join("example.com.key.pem")).unwrap(),
            before,
            "a failed renewal must leave the cached pair untouched"
        );
    }

    #[tokio::test]
    async fn the_loop_keeps_going_after_a_failed_renewal_and_stops_on_shutdown() {
        let dir = tempfile::TempDir::new().unwrap();
        let job = expiring_job(dir.path());
        let (tx, rx) = watch::channel(false);
        let task = tokio::spawn(run_loop(
            vec![job],
            Arc::new(DashMap::new()),
            rx,
            Duration::from_millis(20),
        ));
        // Long enough for at least one tick and its failed renewal.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            !task.is_finished(),
            "a failed renewal must not end the loop"
        );
        tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the loop must stop on shutdown")
            .unwrap();
    }

    #[tokio::test]
    async fn the_public_loop_entry_point_stops_on_shutdown() {
        let dir = tempfile::TempDir::new().unwrap();
        fresh_pair(dir.path(), "example.com", 365);
        let (tx, rx) = watch::channel(false);
        let task = tokio::spawn(run_renewal_loop(
            vec![job(dir.path())],
            Arc::new(DashMap::new()),
            rx,
        ));
        tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("the loop must stop on shutdown")
            .unwrap();
    }

    /// Issue #554: renewal reads the cached pair by the same names, so a path-like host must not
    /// make it look at (and report "nothing to do" for) a pair outside the storage directory.
    #[tokio::test]
    async fn a_path_like_host_is_an_error_not_a_pair_found_outside_the_storage_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let storage = dir.path().join("certs");
        std::fs::create_dir(&storage).unwrap();
        fresh_pair(dir.path(), "x", 365);
        let mut job = job(&storage);
        job.domain = "../x".to_owned();

        let err = renew_if_due(&job, &Arc::new(DashMap::new()))
            .await
            .expect_err("refused");
        assert!(
            format!("{err:#}").contains("invalid ACME domain"),
            "got: {err:#}"
        );
    }

    // ── ARI (RFC 9773, issue #594) against a mock CA ────────────────────────────

    use crate::flow::mock_ca::{Ari, MockCa};

    fn now() -> i64 {
        time::OffsetDateTime::now_utc().unix_timestamp()
    }

    /// A 90-day certificate issued `age_days` ago, signed by a CA key so that it carries an authority key
    /// identifier, written as the cached pair for `example.com`. Returns its ARI identifier.
    fn issued_pair(dir: &std::path::Path, age_days: i64) -> String {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(vec!["ca.test".to_owned()]).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params.self_signed(&ca_key).unwrap();
        let issuer = rcgen::Issuer::new(ca_params, ca_key);

        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["example.com".to_owned()]).unwrap();
        params.use_authority_key_identifier_extension = true;
        let issued = time::OffsetDateTime::now_utc() - time::Duration::days(age_days);
        params.not_before = issued;
        params.not_after = issued + time::Duration::days(90);
        let cert = params.signed_by(&key, &issuer).unwrap();
        std::fs::write(dir.join("example.com.crt.pem"), cert.pem()).unwrap();
        std::fs::write(dir.join("example.com.key.pem"), key.serialize_pem()).unwrap();
        crate::flow::ari::certificate_identifier(&cert.pem())
            .expect("identifier")
            .to_string()
    }

    /// A job against the mock CA, with an account already stored (renewal asks ARI only when there is one).
    async fn job_for(dir: &std::path::Path, ca: &MockCa) -> RenewalJob {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut job = job(dir);
        job.acme.directory = Some(ca.directory_url.clone());
        job.acme.storage = Some(dir.to_string_lossy().into_owned());
        load_or_create_account(&job.acme, &ca.directory_url)
            .await
            .expect("account on the mock CA");
        job
    }

    async fn renew(job: &RenewalJob) -> anyhow::Result<Option<crate::flow::AcmeCertPaths>> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        renew_if_due(job, &Arc::new(DashMap::new())).await
    }

    #[tokio::test]
    async fn an_open_ari_window_renews_a_certificate_that_is_not_due_and_names_the_one_it_replaces()
    {
        let dir = tempfile::TempDir::new().unwrap();
        let id = issued_pair(dir.path(), 10); // 80 of 90 days left: not due by the lifetime rule
        let ca = MockCa::start(Ari::Window {
            // already over: the point chosen inside it (it depends on the certificate's random serial) is past
            start: now() - 7200,
            end: now() - 3600,
        })
        .await;

        let err = renew(&job_for(dir.path(), &ca).await)
            .await
            .expect_err("the mock CA refuses every order");
        assert!(format!("{err:#}").contains("new-order"), "got: {err:#}");

        let seen = ca.seen.lock().unwrap();
        // every request was about the certificate on disk and about no other (the identifiers are not printed:
        // CodeQL treats a certificate identifier as sensitive data in a log)
        assert!(
            !seen.ari_ids.is_empty() && seen.ari_ids.iter().all(|i| *i == id),
            "{} request(s), not all about the certificate on disk",
            seen.ari_ids.len()
        );
        assert_eq!(
            seen.orders.len(),
            2,
            "the order that names `replaces`, then one without it"
        );
        assert_eq!(
            seen.orders[0]["replaces"],
            id.as_str(),
            "RFC 9773 section 5"
        );
        assert!(
            seen.orders[1].get("replaces").is_none(),
            "retried without the link"
        );
    }

    #[tokio::test]
    async fn a_future_ari_window_leaves_a_certificate_that_is_not_due_alone() {
        let dir = tempfile::TempDir::new().unwrap();
        issued_pair(dir.path(), 10);
        let ca = MockCa::start(Ari::Window {
            start: now() + 20 * 86_400,
            end: now() + 21 * 86_400,
        })
        .await;

        assert!(matches!(
            renew(&job_for(dir.path(), &ca).await).await,
            Ok(None)
        ));

        let seen = ca.seen.lock().unwrap();
        assert_eq!(seen.ari_ids.len(), 1, "the CA was asked");
        assert!(seen.orders.is_empty(), "and no order was placed");
    }

    #[tokio::test]
    async fn an_ari_window_that_closes_before_the_next_check_is_not_missed() {
        let dir = tempfile::TempDir::new().unwrap();
        issued_pair(dir.path(), 10);
        let ca = MockCa::start(Ari::Window {
            start: now() + 3600,
            end: now() + 3 * 3600,
        })
        .await;

        assert!(renew(&job_for(dir.path(), &ca).await).await.is_err());
        assert!(
            !ca.seen.lock().unwrap().orders.is_empty(),
            "renewed now: the next check would be after the window"
        );
    }

    #[tokio::test]
    async fn ari_cannot_postpone_a_renewal_the_lifetime_rule_wants() {
        let dir = tempfile::TempDir::new().unwrap();
        let id = issued_pair(dir.path(), 70); // 20 days left of 90: due, whatever the CA's window says
        let ca = MockCa::start(Ari::Window {
            start: now() + 15 * 86_400,
            end: now() + 16 * 86_400,
        })
        .await;

        assert!(renew(&job_for(dir.path(), &ca).await).await.is_err());

        let seen = ca.seen.lock().unwrap();
        assert!(!seen.orders.is_empty(), "the renewal went ahead");
        assert_eq!(seen.orders[0]["replaces"], id.as_str());
    }

    #[tokio::test]
    async fn a_ca_without_ari_gets_no_replaces_and_no_ari_request() {
        let dir = tempfile::TempDir::new().unwrap();
        issued_pair(dir.path(), 10);
        let ca = MockCa::start(Ari::Absent).await;
        // not due, no ARI in the directory: nothing happens
        assert!(matches!(
            renew(&job_for(dir.path(), &ca).await).await,
            Ok(None)
        ));
        assert!(ca.seen.lock().unwrap().ari_ids.is_empty());

        // due by the lifetime rule: the order goes out without `replaces` (a CA without ARI would refuse it)
        let due = tempfile::TempDir::new().unwrap();
        issued_pair(due.path(), 75);
        assert!(renew(&job_for(due.path(), &ca).await).await.is_err());
        let seen = ca.seen.lock().unwrap();
        assert_eq!(seen.orders.len(), 1);
        assert!(seen.orders[0].get("replaces").is_none());
        assert!(seen.ari_ids.is_empty());
    }

    #[tokio::test]
    async fn a_broken_ari_endpoint_is_no_advice_for_a_certificate_that_is_not_due() {
        let dir = tempfile::TempDir::new().unwrap();
        issued_pair(dir.path(), 10);
        let ca = MockCa::start(Ari::Broken).await;
        assert!(matches!(
            renew(&job_for(dir.path(), &ca).await).await,
            Ok(None)
        ));
        assert!(ca.seen.lock().unwrap().orders.is_empty());
    }

    #[tokio::test]
    async fn asking_about_a_certificate_that_is_not_due_does_not_register_an_account() {
        let dir = tempfile::TempDir::new().unwrap();
        issued_pair(dir.path(), 10);
        let ca = MockCa::start(Ari::Window {
            start: now() - 7200,
            end: now() - 3600,
        })
        .await;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut job = job(dir.path());
        job.acme.directory = Some(ca.directory_url.clone());
        job.acme.storage = Some(dir.path().to_string_lossy().into_owned());

        // no account stored: ARI is not asked and nothing is created, even though the window is open
        assert!(matches!(renew(&job).await, Ok(None)));
        let seen = ca.seen.lock().unwrap();
        assert_eq!(seen.accounts, 0);
        assert!(seen.ari_ids.is_empty() && seen.orders.is_empty());
    }

    #[tokio::test]
    async fn a_ca_that_never_answers_does_not_stall_the_renewal_check() {
        let dir = tempfile::TempDir::new().unwrap();
        issued_pair(dir.path(), 10);
        let ca = MockCa::start(Ari::Window {
            start: now() - 7200,
            end: now() - 3600,
        })
        .await;
        let job = job_for(dir.path(), &ca).await; // the account is stored, so restoring it re-fetches the directory
        ca.hang_directory
            .store(true, std::sync::atomic::Ordering::SeqCst);

        // virtual time: the idle runtime jumps to the probe's deadline instead of waiting 30 real seconds
        tokio::time::pause();
        let result = tokio::time::timeout(Duration::from_secs(120), renew(&job))
            .await
            .expect("the probe must give up by itself, well before the outer limit");
        assert!(
            matches!(result, Ok(None)),
            "no advice means the lifetime rule decides: nothing to do"
        );
    }

    #[tokio::test]
    async fn a_ca_that_never_answers_an_order_is_an_error_not_a_stuck_loop() {
        let dir = tempfile::TempDir::new().unwrap();
        issued_pair(dir.path(), 75); // due by the lifetime rule
        let ca = MockCa::start(Ari::Absent).await;
        let job = job_for(dir.path(), &ca).await;
        ca.hang_new_order
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let before = std::fs::read(dir.path().join("example.com.key.pem")).unwrap();

        // Real time until the order has reached the silent `newOrder` (the account and directory requests before it
        // go over real sockets, and a paused clock would jump past their timeouts while they are in flight); virtual
        // time from there, so the 60 s limit costs nothing.
        let task = tokio::spawn({
            let job = job.clone();
            async move { renew(&job).await }
        });
        for _ in 0..1000 {
            if ca.hung_new_orders.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            ca.hung_new_orders.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the order reached newOrder"
        );
        tokio::time::pause();
        let err = tokio::time::timeout(Duration::from_secs(600), task)
            .await
            .expect("the order must give up by itself, well before the outer limit")
            .unwrap()
            .expect_err("a silent CA is an error");
        let message = format!("{err:#}");
        assert!(
            message.contains("new-order request") && message.contains("timed out"),
            "the newOrder step is the one that timed out, got: {message}"
        );
        assert_eq!(
            std::fs::read(dir.path().join("example.com.key.pem")).unwrap(),
            before,
            "the cached pair is untouched"
        );
    }

    #[test]
    fn a_job_is_checked_again_after_the_period_or_after_the_hint_if_that_is_sooner() {
        let now = Instant::now();
        let period = Duration::from_secs(12 * 3600);
        let after = |ask: Option<Instant>| {
            let mut state = JobState::new(now, period);
            state.ask_ca_at = ask;
            state.checked(now, period);
            state.next_check
        };
        assert_eq!(after(None), now + period);
        // a hint beyond the period does not postpone the check
        assert_eq!(
            after(Some(now + Duration::from_secs(30 * 3600))),
            now + period
        );
        assert_eq!(
            after(Some(now + Duration::from_secs(3600))),
            now + Duration::from_secs(3600)
        );
        // a hint already due: due again at once
        assert_eq!(after(Some(now)), now);
    }

    #[tokio::test]
    async fn one_cas_short_hint_does_not_bring_forward_the_check_of_the_other_jobs() {
        // Job A's CA asks to be consulted again in 5 minutes; job B is due and its order keeps failing. B must be
        // retried after the period, not at A's cadence (every retry is a full order against the CA's limits).
        let (dir_a, dir_b) = (
            tempfile::TempDir::new().unwrap(),
            tempfile::TempDir::new().unwrap(),
        );
        issued_pair(dir_a.path(), 10);
        issued_pair(dir_b.path(), 75); // due by the lifetime rule
        let ca_a = MockCa::start(Ari::Window {
            start: now() + 20 * 86_400,
            end: now() + 21 * 86_400,
        })
        .await;
        ca_a.retry_after_secs
            .store(300, std::sync::atomic::Ordering::SeqCst);
        let ca_b = MockCa::start(Ari::Absent).await;
        let jobs = vec![
            job_for(dir_a.path(), &ca_a).await,
            job_for(dir_b.path(), &ca_b).await,
        ];
        let challenges = Arc::new(DashMap::new());
        let period = Duration::from_secs(12 * 3600);
        let start = Instant::now();
        let mut states = vec![JobState::new(start, period); 2];
        states.iter_mut().for_each(|s| s.next_check = start); // both due now
        let (_tx, mut rx) = watch::channel(false);

        assert!(check_due(&jobs, &challenges, &mut states, period, start, &mut rx).await);
        assert_eq!(ca_b.seen.lock().unwrap().orders.len(), 1, "B was tried");
        let a_next = states[0].next_check.saturating_duration_since(start);
        assert!(
            (290..=300).contains(&a_next.as_secs()),
            "A is due again in about 5 minutes, got {a_next:?}"
        );
        assert!(states[1].next_check >= start + period - Duration::from_secs(5));

        // A's hint ran out; B's did not exist, B is not due
        let later = start + Duration::from_secs(301);
        assert!(check_due(&jobs, &challenges, &mut states, period, later, &mut rx).await);
        assert_eq!(
            ca_b.seen.lock().unwrap().orders.len(),
            1,
            "B is not retried at A's cadence"
        );

        // after the period B is due again
        let much_later = start + period + Duration::from_secs(10);
        assert!(check_due(&jobs, &challenges, &mut states, period, much_later, &mut rx).await);
        assert_eq!(ca_b.seen.lock().unwrap().orders.len(), 2);
    }

    #[tokio::test]
    async fn the_ca_is_not_asked_again_before_its_retry_after() {
        let dir = tempfile::TempDir::new().unwrap();
        issued_pair(dir.path(), 10);
        let ca = MockCa::start(Ari::Window {
            start: now() + 20 * 86_400,
            end: now() + 21 * 86_400,
        })
        .await;
        ca.retry_after_secs
            .store(3600, std::sync::atomic::Ordering::SeqCst);
        let job = job_for(dir.path(), &ca).await;
        let challenges = Arc::new(DashMap::new());
        let mut ask_ca_at = None;

        assert!(matches!(
            renew_scheduled(&job, &challenges, &mut ask_ca_at).await,
            Ok(None)
        ));
        let at = ask_ca_at.expect("the CA's Retry-After is remembered");
        let in_secs = at.saturating_duration_since(Instant::now()).as_secs();
        assert!((3500..=3600).contains(&in_secs), "got {in_secs}s");
        assert_eq!(ca.seen.lock().unwrap().ari_ids.len(), 1);

        // a check before the hint runs out does not ask again
        assert!(matches!(
            renew_scheduled(&job, &challenges, &mut ask_ca_at).await,
            Ok(None)
        ));
        assert_eq!(ca.seen.lock().unwrap().ari_ids.len(), 1, "not asked again");

        // the hint ran out: asked again
        ask_ca_at = Some(Instant::now());
        assert!(matches!(
            renew_scheduled(&job, &challenges, &mut ask_ca_at).await,
            Ok(None)
        ));
        assert_eq!(ca.seen.lock().unwrap().ari_ids.len(), 2);
    }

    #[tokio::test]
    async fn the_retry_after_is_kept_within_five_minutes_and_a_day() {
        let dir = tempfile::TempDir::new().unwrap();
        issued_pair(dir.path(), 10);
        let ca = MockCa::start(Ari::Window {
            start: now() + 20 * 86_400,
            end: now() + 21 * 86_400,
        })
        .await;
        let job = job_for(dir.path(), &ca).await;
        let challenges = Arc::new(DashMap::new());
        for (hint, low, high) in [(1, 295, 300), (10 * 86_400, 86_395, 86_400)] {
            ca.retry_after_secs
                .store(hint, std::sync::atomic::Ordering::SeqCst);
            let mut ask_ca_at = None;
            renew_scheduled(&job, &challenges, &mut ask_ca_at)
                .await
                .unwrap();
            let secs = ask_ca_at
                .expect("remembered")
                .saturating_duration_since(Instant::now())
                .as_secs();
            assert!((low..=high).contains(&secs), "hint {hint}s gave {secs}s");
        }
    }

    #[tokio::test]
    async fn configured_retry_after_clamps_replace_the_defaults() {
        // `retryAfterMinSecs` and `retryAfterMaxSecs` (issue #746): a hint is kept within them, not within 5 minutes
        // and a day.
        let dir = tempfile::TempDir::new().unwrap();
        issued_pair(dir.path(), 10);
        let ca = MockCa::start(Ari::Window {
            start: now() + 20 * 86_400,
            end: now() + 21 * 86_400,
        })
        .await;
        let mut job = job_for(dir.path(), &ca).await;
        job.acme.retry_after_min_secs = Some(60);
        job.acme.retry_after_max_secs = Some(7200);
        let challenges = Arc::new(DashMap::new());
        for (hint, low, high) in [(1, 55, 60), (86_400, 7195, 7200), (1800, 1795, 1800)] {
            ca.retry_after_secs
                .store(hint, std::sync::atomic::Ordering::SeqCst);
            let mut ask_ca_at = None;
            renew_scheduled(&job, &challenges, &mut ask_ca_at)
                .await
                .unwrap();
            let secs = ask_ca_at
                .expect("remembered")
                .saturating_duration_since(Instant::now())
                .as_secs();
            assert!((low..=high).contains(&secs), "hint {hint}s gave {secs}s");
        }
    }

    #[tokio::test]
    async fn an_inverted_clamp_does_not_panic() {
        // `validate` rejects min > max, but a config that skipped it must not take the renewal loop down
        // (`Duration::clamp` panics on it).
        let dir = tempfile::TempDir::new().unwrap();
        issued_pair(dir.path(), 10);
        let ca = MockCa::start(Ari::Window {
            start: now() + 20 * 86_400,
            end: now() + 21 * 86_400,
        })
        .await;
        let mut job = job_for(dir.path(), &ca).await;
        job.acme.retry_after_min_secs = Some(7200);
        job.acme.retry_after_max_secs = Some(60);
        let mut ask_ca_at = None;
        renew_scheduled(&job, &Arc::new(DashMap::new()), &mut ask_ca_at)
            .await
            .unwrap();
        assert!(ask_ca_at.is_some());
    }

    #[test]
    fn the_loop_period_is_the_shortest_configured_check_interval() {
        // `checkIntervalSecs` (issue #746): one loop serves all sites, so the shortest interval counts.
        let dir = tempfile::TempDir::new().unwrap();
        let mut a = job(dir.path());
        let mut b = job(dir.path());
        assert_eq!(
            check_period(&[a.clone(), b.clone()]),
            Duration::from_secs(12 * 3600)
        );
        a.acme.check_interval_secs = Some(7200);
        assert_eq!(
            check_period(&[a.clone(), b.clone()]),
            Duration::from_secs(7200)
        );
        b.acme.check_interval_secs = Some(600);
        assert_eq!(check_period(&[a, b]), Duration::from_secs(600));
    }

    #[tokio::test]
    async fn without_usable_advice_the_ca_is_asked_at_every_check() {
        let dir = tempfile::TempDir::new().unwrap();
        issued_pair(dir.path(), 10);
        let ca = MockCa::start(Ari::Broken).await;
        let job = job_for(dir.path(), &ca).await;
        let mut ask_ca_at = None;
        for _ in 0..2 {
            assert!(matches!(
                renew_scheduled(&job, &Arc::new(DashMap::new()), &mut ask_ca_at).await,
                Ok(None)
            ));
        }
        assert!(ask_ca_at.is_none(), "an error is no hint to wait for");
        assert_eq!(ca.seen.lock().unwrap().ari_ids.len(), 2);
    }

    #[tokio::test]
    async fn a_failed_renewal_is_retried_at_the_next_check_not_after_the_hint() {
        let dir = tempfile::TempDir::new().unwrap();
        issued_pair(dir.path(), 10);
        let ca = MockCa::start(Ari::Window {
            start: now() - 7200,
            end: now() - 3600,
        })
        .await;
        ca.retry_after_secs
            .store(86_400, std::sync::atomic::Ordering::SeqCst);
        let job = job_for(dir.path(), &ca).await;
        let mut ask_ca_at = None;

        // the window is open, so an order is placed, and the mock CA refuses it
        assert!(
            renew_scheduled(&job, &Arc::new(DashMap::new()), &mut ask_ca_at)
                .await
                .is_err()
        );
        assert!(
            ask_ca_at.is_none(),
            "the next check asks again; the day-long hint must not delay the retry"
        );
    }
}
