//! The keyed views of a config that `detect_cold_changes` (in `api.rs`) compares between the running and the reloaded
//! config: one entry per port or host, so a block added, removed or changed on a site is seen as one change.

/// The `tcp` block of every TCP site, by the port it listens on (the first site wins for a repeated port).
pub(super) fn tcp_by_port(
    cfg: &crate::config::schema::AppConfig,
) -> std::collections::BTreeMap<u16, &crate::config::schema::TcpConfig> {
    let mut out = std::collections::BTreeMap::new();
    for site in &cfg.sites {
        if let Some(tcp) = site.tcp.as_ref() {
            out.entry(site.port.unwrap_or(80)).or_insert(tcp);
        }
    }
    out
}

/// The `proxyProtocol` block of every site that has one, by the port it listens on (the first site wins for a repeated
/// port; validation requires sites that share a port to agree).
pub(super) fn proxy_protocol_by_port(
    cfg: &crate::config::schema::AppConfig,
) -> std::collections::BTreeMap<u16, &crate::config::schema::ProxyProtocolConfig> {
    let mut out = std::collections::BTreeMap::new();
    for site in &cfg.sites {
        if let Some(pp) = site.proxy_protocol.as_ref() {
            let port = site
                .port
                .unwrap_or(if site.tls.is_some() { 443 } else { 80 });
            out.entry(port).or_insert(pp);
        }
    }
    out
}

/// The `tls.acme` block of every site that has one, by the site's `host` (the first site wins for a repeated host).
pub(super) fn acme_by_host(
    cfg: &crate::config::schema::AppConfig,
) -> std::collections::BTreeMap<String, &crate::config::schema::AcmeConfig> {
    let mut out = std::collections::BTreeMap::new();
    for site in &cfg.sites {
        if let (Some(host), Some(acme)) = (
            site.host.as_ref(),
            site.tls.as_ref().and_then(|t| t.acme.as_ref()),
        ) {
            out.entry(host.clone()).or_insert(acme);
        }
    }
    out
}
