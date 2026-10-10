use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::schema::{AcmeConfig, AppConfig};
use crate::server::acme as acme_util;
use acme_util::{ChallengeSource, RenewalJob};

/// One site that wants an ACME certificate, with everything derived the same
/// way for startup issuance and for renewal.
struct AcmeSite<'a> {
    /// The site's TLS port (where the certificate is served).
    port: u16,
    domain: &'a str,
    /// Local port the CA's HTTP-01 request must reach: this site's own
    /// `tls.httpRedirectPort`, else 80.
    challenge_port: u16,
    storage_dir: PathBuf,
    acme: &'a AcmeConfig,
}

fn acme_sites(config: &AppConfig) -> Vec<AcmeSite<'_>> {
    config
        .sites
        .iter()
        .filter_map(|site| {
            let tls = site.tls.as_ref()?;
            let acme = tls.acme.as_ref()?;
            let domain = site.host.as_deref()?;
            Some(AcmeSite {
                port: site.port.unwrap_or(443),
                domain,
                challenge_port: tls.http_redirect_port.unwrap_or(80),
                storage_dir: PathBuf::from(acme.storage.as_deref().unwrap_or("./certs")),
                acme,
            })
        })
        .collect()
}

/// Obtain ACME certificates for every site that has `tls.acme` configured.
///
/// Runs a dedicated single-threaded Tokio runtime so that the async ACME flow
/// can be driven from the synchronous `run_server` function.
///
/// Returns a map of `port → (cert_path, key_path)` for successfully obtained
/// certificates.  Sites whose procurement fails are logged and excluded from
/// the map (they fall back to plain TCP).
pub(super) fn obtain_acme_certs(
    config: &AppConfig,
    challenges: &Arc<dashmap::DashMap<String, String>>,
) -> anyhow::Result<HashMap<u16, (String, String)>> {
    let sites = acme_sites(config);

    if sites.is_empty() {
        return Ok(HashMap::new());
    }

    // Create a dedicated Tokio runtime for ACME negotiation.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build ACME Tokio runtime: {e}"))?;

    let mut result = HashMap::new();

    for site in sites {
        let AcmeSite {
            port,
            domain,
            challenge_port,
            storage_dir,
            acme,
        } = site;

        match rt.block_on(acme_util::load_or_obtain_certificate(
            acme,
            domain,
            challenges.clone(),
            &storage_dir,
            challenge_port,
        )) {
            Ok(paths) => {
                result.insert(
                    port,
                    (
                        paths.cert.to_string_lossy().into_owned(),
                        paths.key.to_string_lossy().into_owned(),
                    ),
                );
                // Renewal runs as one background loop, planned from the bound
                // listeners — see `plan_acme_renewals` and `run_server`.
                tracing::info!(domain, port, "ACME certificate ready");
            }
            Err(e) => {
                tracing::error!(domain, port, error = %e, "ACME certificate procurement failed — site will serve plain HTTP");
            }
        }
    }

    Ok(result)
}

/// Plan the background renewal jobs (#491) from the listeners that were
/// actually bound — not from the config alone, which says what was *asked*.
///
/// - `token_ports`: listeners that answer plain HTTP and serve
///   `/.well-known/acme-challenge/{token}` from the shared challenge map (the
///   `httpRedirectPort` redirect service, or a site's plain proxy listener).
///   A challenge port in this set is [`ChallengeSource::Shared`]: binding it a
///   second time would fail with `EADDRINUSE` on every attempt.
/// - `other_ports`: listeners that cannot answer the CA (TLS listeners, raw TCP
///   proxies). The job is skipped with an error — the CA's request would reach
///   something that does not speak HTTP-01.
/// - any other port is free, so the loop may bind it itself
///   ([`ChallengeSource::Bind`]), exactly as startup issuance does.
///
/// One job per distinct `(domain, storage dir)`.
///
/// In `Shared` mode the CA's request goes through the site listener's guard
/// chain: an `ipFilter` or dynamic deny list that does not admit the CA can
/// still answer 403 (the ACME token is served after `IpGuard`, by design).
pub(super) fn plan_acme_renewals(
    config: &AppConfig,
    token_ports: &HashSet<u16>,
    other_ports: &HashSet<u16>,
) -> Vec<RenewalJob> {
    let mut seen = HashSet::new();
    let mut jobs = Vec::new();
    for site in acme_sites(config) {
        if !seen.insert((site.domain, site.storage_dir.clone())) {
            continue;
        }
        let source = if token_ports.contains(&site.challenge_port) {
            ChallengeSource::Shared
        } else if other_ports.contains(&site.challenge_port) {
            tracing::error!(
                domain = site.domain,
                port = site.challenge_port,
                "ACME renewal disabled: the HTTP-01 challenge port is used by a TLS or TCP \
                 listener that cannot answer the CA — set tls.httpRedirectPort to a plain \
                 HTTP port"
            );
            continue;
        } else {
            ChallengeSource::Bind(site.challenge_port)
        };
        jobs.push(RenewalJob {
            acme: site.acme.clone(),
            domain: site.domain.to_owned(),
            storage_dir: site.storage_dir,
            source,
        });
    }
    jobs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::from_str as parse_config;

    fn cfg(json: &str) -> AppConfig {
        parse_config(json).expect("parse")
    }

    fn set(ports: &[u16]) -> HashSet<u16> {
        ports.iter().copied().collect()
    }

    fn acme_site(host: &str, tls_extra: &str) -> String {
        format!(r#"{{"host":"{host}","tls":{{{tls_extra}"acme":{{"email":"o@example.com"}}}}}}"#)
    }

    fn plan(json: &str, token: &[u16], other: &[u16]) -> Vec<RenewalJob> {
        plan_acme_renewals(&cfg(json), &set(token), &set(other))
    }

    #[test]
    fn a_redirect_listener_that_serves_tokens_is_shared_not_rebound() {
        // The redirect proxy owns :8080 in the running process — binding it
        // again from the renewal loop would fail with EADDRINUSE.
        let json = format!(
            r#"{{"sites":[{}]}}"#,
            acme_site("a.example.com", r#""httpRedirectPort":8080,"#)
        );
        let jobs = plan(&json, &[8080], &[]);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].source, ChallengeSource::Shared);
    }

    #[test]
    fn a_free_challenge_port_is_bound_by_the_loop() {
        let json = format!(r#"{{"sites":[{}]}}"#, acme_site("a.example.com", ""));
        let jobs = plan(&json, &[], &[]);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].source, ChallengeSource::Bind(80));
        assert_eq!(jobs[0].domain, "a.example.com");
        assert_eq!(jobs[0].storage_dir, PathBuf::from("./certs"));
    }

    #[test]
    fn a_plain_site_on_the_challenge_port_serves_the_token() {
        let json = format!(
            r#"{{"sites":[{},{{"host":"plain.example.com","port":80}}]}}"#,
            acme_site("a.example.com", "")
        );
        let jobs = plan(&json, &[80], &[]);
        assert_eq!(jobs[0].source, ChallengeSource::Shared);
    }

    #[test]
    fn a_tls_or_tcp_listener_on_the_challenge_port_disables_the_job() {
        // A TLS listener, or a raw TCP proxy, cannot answer the CA's plain
        // HTTP request: "shared" would silently fail every 12 h, so say so once
        // at startup instead.
        let json = format!(r#"{{"sites":[{}]}}"#, acme_site("a.example.com", ""));
        assert!(plan(&json, &[], &[80]).is_empty());
    }

    #[test]
    fn one_job_per_domain_and_storage() {
        let json = format!(
            r#"{{"sites":[{},{}]}}"#,
            acme_site("a.example.com", ""),
            acme_site("a.example.com", "")
        );
        assert_eq!(plan(&json, &[], &[]).len(), 1);
    }

    #[test]
    fn each_site_uses_its_own_redirect_port() {
        // Startup used to look up "the first site with this host", which can
        // be a different site; the challenge port is the ACME site's own.
        let json = format!(
            r#"{{"sites":[{},{}]}}"#,
            acme_site("a.example.com", r#""httpRedirectPort":8080,"#),
            acme_site("b.example.com", r#""httpRedirectPort":8081,"#),
        );
        let jobs = plan(&json, &[8080], &[8081]);
        assert_eq!(jobs.len(), 1, "b's port cannot serve tokens: skipped");
        assert_eq!(jobs[0].domain, "a.example.com");
    }
    // ── obtain_acme_certs ────────────────────────────────────────────────────

    fn acme_config_json(host: &str, port: u16, storage: &std::path::Path) -> String {
        serde_json::json!({
            "sites": [{
                "host": host,
                "port": port,
                "tls": { "acme": {
                    "email": "o@example.com",
                    "directory": "http://127.0.0.1:1/directory",
                    "storage": storage.to_str().unwrap(),
                } }
            }]
        })
        .to_string()
    }

    /// Startup issuance with the CA down: a valid cached pair is used, so the
    /// site keeps its TLS listener instead of falling back to plain HTTP.
    #[test]
    fn startup_uses_the_cached_pair_when_the_ca_is_unreachable() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tempfile::TempDir::new().unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["a.example.com".to_owned()]).unwrap();
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(5);
        let cert = params.self_signed(&key).unwrap();
        std::fs::write(dir.path().join("a.example.com.crt.pem"), cert.pem()).unwrap();
        std::fs::write(
            dir.path().join("a.example.com.key.pem"),
            key.serialize_pem(),
        )
        .unwrap();

        let config = cfg(&acme_config_json("a.example.com", 8443, dir.path()));
        let certs =
            obtain_acme_certs(&config, &Arc::new(dashmap::DashMap::new())).expect("startup");

        let (cert_path, key_path) = certs.get(&8443).expect("the site keeps its TLS entry");
        assert!(cert_path.ends_with("a.example.com.crt.pem"));
        assert!(key_path.ends_with("a.example.com.key.pem"));
    }

    #[test]
    fn startup_without_acme_sites_does_nothing() {
        let config = cfg(r#"{"sites":[{"port":8080}]}"#);
        let certs = obtain_acme_certs(&config, &Arc::new(dashmap::DashMap::new())).unwrap();
        assert!(certs.is_empty());
    }

    #[test]
    fn startup_survives_a_failed_issuance_without_a_cache() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tempfile::TempDir::new().unwrap();
        let config = cfg(&acme_config_json("b.example.com", 8444, dir.path()));

        let certs = obtain_acme_certs(&config, &Arc::new(dashmap::DashMap::new())).unwrap();

        assert!(certs.is_empty(), "the site falls back to plain HTTP");
    }
}
