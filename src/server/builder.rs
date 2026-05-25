use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pingora_core::server::configuration::Opt;
use pingora_core::server::Server;
use pingora_core::services::background::background_service;
use pingora_proxy::http_proxy_service;

use crate::admin::api::AdminApiService;
use crate::config::defaults::DEFAULT_ADMIN_BIND;
use crate::config::schema::{AppConfig, SiteConfig};
use crate::proxy::service::{AppState, ConduitProxy};
use crate::server::{acme as acme_util, tls as tls_util};

/// Maps a TCP port to `(cert_path, key_path, h2_enabled)` for TLS-enabled ports.
type TlsPortMap = HashMap<u16, (String, String, bool)>;

/// Classify each site's port into either a TLS entry (cert, key, h2-enabled)
/// or a plain-TCP entry.
///
/// ACME sites are initially absent from both maps — their TLS entry is added
/// after certificate procurement in [`run_server`].
///
/// Returns `(port_tls, port_plain)`.
fn classify_ports(
    sites: &[SiteConfig],
    acme_certs: &HashMap<u16, (String, String)>,
) -> (TlsPortMap, HashSet<u16>) {
    let mut port_tls: TlsPortMap = HashMap::new();
    let mut port_plain: HashSet<u16> = HashSet::new();

    if sites.is_empty() {
        port_plain.insert(8080);
        return (port_tls, port_plain);
    }

    for site in sites {
        let port = site
            .port
            .unwrap_or(if site.tls.is_some() { 443 } else { 80 });
        let enable_h2 = site.http2.is_some();

        if let Some(tls_cfg) = &site.tls {
            if tls_cfg.acme.is_some() {
                // Use the cert/key obtained by the ACME flow, if available.
                if let Some((cert, key)) = acme_certs.get(&port) {
                    port_tls
                        .entry(port)
                        .or_insert_with(|| (cert.clone(), key.clone(), enable_h2));
                } else {
                    // ACME failed — fall back to plain TCP so the port is reachable.
                    port_plain.insert(port);
                }
            } else if let (Some(cert), Some(key)) = (&tls_cfg.cert, &tls_cfg.key) {
                port_tls
                    .entry(port)
                    .or_insert_with(|| (cert.clone(), key.clone(), enable_h2));
            } else {
                // Incomplete TLS config (no cert/key and no ACME) → plain TCP.
                port_plain.insert(port);
            }
        } else {
            port_plain.insert(port);
        }
    }

    (port_tls, port_plain)
}

pub fn run_server(config: AppConfig, config_path: PathBuf) -> anyhow::Result<()> {
    // Install the ring crypto provider for rustls before any TLS initialization.
    // This is a no-op if another provider was already installed (e.g., in tests).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let admin_bind = config
        .global
        .as_ref()
        .and_then(|g| g.admin.as_ref())
        .and_then(|a| a.bind.as_deref())
        .unwrap_or(DEFAULT_ADMIN_BIND)
        .to_owned();

    // Create AppState early so acme_challenges can be shared with the ACME flow.
    let state = Arc::new(AppState::new(config.clone(), config_path));

    // ── Phase 3.1: ACME certificate procurement ──────────────────────────────
    // For each site that uses `tls.acme`, obtain (or load a cached) certificate
    // before Pingora starts.  A dedicated Tokio runtime is used for the async
    // ACME negotiation so this can run from the synchronous `run_server`.
    let acme_certs = obtain_acme_certs(&config, &state.acme_challenges)?;

    let opt = Opt {
        upgrade: false,
        daemon: false,
        nocapture: false,
        test: false,
        conf: None,
    };
    let mut server = Server::new(Some(opt))?;
    server.bootstrap();

    // ── Proxy service ────────────────────────────────────────────────────────
    let proxy = ConduitProxy {
        state: state.clone(),
    };
    let mut proxy_service = http_proxy_service(&server.configuration, proxy);

    let (port_tls, port_plain) = classify_ports(&config.sites, &acme_certs);

    // Add TLS listeners.
    for (port, (cert, key, enable_h2)) in &port_tls {
        let addr = format!("0.0.0.0:{port}");
        let settings = tls_util::make_tls_settings(cert, key, *enable_h2)
            .map_err(|e| anyhow::anyhow!("TLS setup failed for port {port}: {e}"))?;
        proxy_service.add_tls_with_settings(&addr, None, settings);
    }

    // Add plain TCP listeners for ports that are not TLS.
    for port in &port_plain {
        if !port_tls.contains_key(port) {
            proxy_service.add_tcp(&format!("0.0.0.0:{port}"));
        }
    }

    server.add_service(proxy_service);

    // ── HTTP → HTTPS redirect services ───────────────────────────────────────
    // For each site that has `tls.httpRedirectPort`, spin up a tiny Pingora
    // service that 308-redirects to the HTTPS equivalent.  The redirect service
    // also serves ACME HTTP-01 challenges so that certificate renewal works
    // without a separate listener.
    for site in &config.sites {
        let tls_port = site
            .port
            .unwrap_or(if site.tls.is_some() { 443 } else { 80 });
        if let Some(http_port) = site.tls.as_ref().and_then(|t| t.http_redirect_port) {
            use crate::server::redirect::RedirectProxy;
            let redirect = RedirectProxy::new(tls_port, state.acme_challenges.clone());
            let mut redirect_svc = http_proxy_service(&server.configuration, redirect);
            redirect_svc.add_tcp(&format!("0.0.0.0:{http_port}"));
            server.add_service(redirect_svc);
        }
    }

    // ── Admin API background service ─────────────────────────────────────────
    let admin = AdminApiService {
        state,
        bind: admin_bind,
    };
    server.add_service(background_service("admin-api", admin));

    server.run_forever()
}

/// Obtain ACME certificates for every site that has `tls.acme` configured.
///
/// Runs a dedicated single-threaded Tokio runtime so that the async ACME flow
/// can be driven from the synchronous `run_server` function.
///
/// Returns a map of `port → (cert_path, key_path)` for successfully obtained
/// certificates.  Sites whose procurement fails are logged and excluded from
/// the map (they fall back to plain TCP).
fn obtain_acme_certs(
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
        let storage_dir = Path::new(storage);
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
                // Spawn certificate renewal task (needs a running Tokio runtime —
                // it will be started by Pingora's server.run_forever()).
                // We schedule it in the admin service's start() instead.
                // For now, store the acme config for later pickup.
                tracing::info!(domain, port, "ACME certificate ready");
            }
            Err(e) => {
                tracing::error!(domain, port, error = %e, "ACME certificate procurement failed — site will serve plain HTTP");
            }
        }
    }

    Ok(result)
}
