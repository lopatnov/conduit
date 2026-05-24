use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pingora_core::server::configuration::Opt;
use pingora_core::server::Server;
use pingora_core::services::background::background_service;
use pingora_proxy::http_proxy_service;

use crate::admin::api::AdminApiService;
use crate::config::defaults::DEFAULT_ADMIN_BIND;
use crate::config::schema::{AppConfig, SiteConfig};
use crate::proxy::service::{AppState, ConduitProxy};
use crate::server::tls as tls_util;

/// Maps a TCP port to `(cert_path, key_path, h2_enabled)` for TLS-enabled ports.
type TlsPortMap = HashMap<u16, (String, String, bool)>;

/// Classify each site's port into either a TLS entry (cert, key, h2-enabled)
/// or a plain-TCP entry.
///
/// When multiple sites share a port the first TLS config wins.  Ports with
/// ACME or incomplete TLS configuration fall back to plain TCP.
///
/// Returns `(port_tls, port_plain)`.
fn classify_ports(sites: &[SiteConfig]) -> (TlsPortMap, HashSet<u16>) {
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
                // Auto-TLS via ACME — implemented in Phase 3.1.
                // Fall back to plain TCP for now so the port is at least reachable.
                port_plain.insert(port);
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

pub fn run_server(config: AppConfig) -> anyhow::Result<()> {
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

    let state = Arc::new(AppState::new(config.clone()));

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

    let (port_tls, port_plain) = classify_ports(&config.sites);

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
    // service that unconditionally 308-redirects to the HTTPS equivalent.
    for site in &config.sites {
        let tls_port = site
            .port
            .unwrap_or(if site.tls.is_some() { 443 } else { 80 });
        if let Some(http_port) = site.tls.as_ref().and_then(|t| t.http_redirect_port) {
            use crate::server::redirect::RedirectProxy;
            let redirect = RedirectProxy::new(tls_port);
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
