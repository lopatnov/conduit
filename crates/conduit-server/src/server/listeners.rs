use std::collections::{HashMap, HashSet};

use pingora_core::apps::HttpServerOptions;
use pingora_core::services::listening::Service as ListeningService;
use pingora_proxy::HttpProxy;

use crate::config::schema::SiteConfig;
use crate::server::tls as tls_util;
use conduit_runtime::proxy::service::ConduitProxy;

/// Maps a TCP port to `(cert_path, key_path, h2_enabled)` for TLS-enabled ports.
/// (cert_path, key_path, http2_enabled, optional_client_auth)
pub(super) type TlsPortMap = HashMap<
    u16,
    (
        String,
        String,
        bool,
        Option<crate::config::schema::TlsClientAuth>,
    ),
>;

/// Classify each site's port into either a TLS entry (cert, key, h2-enabled)
/// or a plain-TCP entry.
///
/// ACME sites are initially absent from both maps — their TLS entry is added
/// after certificate procurement in [`super::builder::run_server`].
///
/// Returns `(port_tls, port_plain)`.
pub(super) fn classify_ports(
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
        classify_site_port(site, acme_certs, &mut port_tls, &mut port_plain);
    }

    (port_tls, port_plain)
}

/// Classify one site's port, inserting it into either `port_tls` or `port_plain`.
fn classify_site_port(
    site: &SiteConfig,
    acme_certs: &HashMap<u16, (String, String)>,
    port_tls: &mut TlsPortMap,
    port_plain: &mut HashSet<u16>,
) {
    // TCP-proxy sites manage their own listeners — skip HTTP port classification.
    if site.tcp.is_some() {
        return;
    }

    let port = site
        .port
        .unwrap_or(if site.tls.is_some() { 443 } else { 80 });
    let enable_h2 = site.http2.is_some();

    let Some(tls_cfg) = &site.tls else {
        port_plain.insert(port);
        return;
    };

    let client_auth = tls_cfg.client_auth.clone();
    if tls_cfg.acme.is_some() {
        // Use the cert/key obtained by the ACME flow, if available.
        if let Some((cert, key)) = acme_certs.get(&port) {
            port_tls
                .entry(port)
                .or_insert_with(|| (cert.clone(), key.clone(), enable_h2, client_auth));
        } else {
            // ACME failed — fall back to plain TCP so the port is reachable.
            port_plain.insert(port);
        }
    } else if let (Some(cert), Some(key)) = (&tls_cfg.cert, &tls_cfg.key) {
        port_tls
            .entry(port)
            .or_insert_with(|| (cert.clone(), key.clone(), enable_h2, client_auth));
    } else {
        // Incomplete TLS config (no cert/key and no ACME) → plain TCP.
        port_plain.insert(port);
    }
}

/// Build `HttpServerOptions` from site configs (h2c + keepalive limit).
pub(super) fn build_http_server_options(sites: &[SiteConfig]) -> Option<HttpServerOptions> {
    let h2c = sites.iter().any(|s| {
        s.http2
            .as_ref()
            .and_then(|h| match h {
                crate::config::schema::Http2Config {
                    h2c: Some(true), ..
                } => Some(true),
                _ => None,
            })
            .unwrap_or(false)
    });
    let keepalive_request_limit: Option<u32> = sites
        .iter()
        .filter_map(|s| s.limits.as_ref()?.keepalive_request_limit)
        .min();

    if h2c || keepalive_request_limit.is_some() {
        let mut opts = HttpServerOptions::default();
        opts.h2c = h2c;
        opts.keepalive_request_limit = keepalive_request_limit;
        Some(opts)
    } else {
        None
    }
}

/// Add TLS listeners to the proxy service for every TLS-enabled port.
pub(super) fn add_tls_listeners(
    proxy_service: &mut ListeningService<HttpProxy<ConduitProxy>>,
    port_tls: &TlsPortMap,
) -> anyhow::Result<()> {
    for (port, (cert, key, enable_h2, client_auth)) in port_tls {
        let addr = format!("0.0.0.0:{port}");
        let settings = if let Some(ref ca_cfg) = client_auth {
            tls_util::make_tls_settings_with_client_auth(cert, key, *enable_h2, ca_cfg)
        } else {
            tls_util::make_tls_settings(cert, key, *enable_h2)
        }
        .map_err(|e| anyhow::anyhow!("TLS setup failed for port {port}: {e}"))?;
        proxy_service.add_tls_with_settings(&addr, None, settings);
    }
    Ok(())
}
