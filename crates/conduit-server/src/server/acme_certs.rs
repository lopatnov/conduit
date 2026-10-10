use std::collections::HashMap;
use std::sync::Arc;

use crate::config::schema::AppConfig;
use crate::server::acme as acme_util;

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
    // Collect sites that need ACME.
    let acme_sites: Vec<(u16, &str, &crate::config::schema::AcmeConfig)> = config
        .sites
        .iter()
        .filter_map(|site| {
            let tls = site.tls.as_ref()?;
            let acme = tls.acme.as_ref()?;
            let domain = site.host.as_deref()?;
            let port = site.port.unwrap_or(443);
            Some((port, domain, acme))
        })
        .collect();

    if acme_sites.is_empty() {
        return Ok(HashMap::new());
    }

    // Create a dedicated Tokio runtime for ACME negotiation.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build ACME Tokio runtime: {e}"))?;

    let mut result = HashMap::new();

    for (port, domain, acme_cfg) in acme_sites {
        let storage = acme_cfg.storage.as_deref().unwrap_or("./certs");
        let storage_dir = std::path::Path::new(storage);
        // HTTP-01 challenge port: use httpRedirectPort if set, otherwise port 80.
        let challenge_port = config
            .sites
            .iter()
            .find(|s| s.host.as_deref() == Some(domain))
            .and_then(|s| s.tls.as_ref())
            .and_then(|t| t.http_redirect_port)
            .unwrap_or(80);

        match rt.block_on(acme_util::load_or_obtain_certificate(
            acme_cfg,
            domain,
            challenges.clone(),
            storage_dir,
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
                // The renewal background task is spawned in AdminApiService::start()
                // once Pingora's Tokio runtime is running — see admin/api.rs.
                tracing::info!(domain, port, "ACME certificate ready");
            }
            Err(e) => {
                tracing::error!(domain, port, error = %e, "ACME certificate procurement failed — site will serve plain HTTP");
            }
        }
    }

    Ok(result)
}
