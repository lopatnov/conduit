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

/// The `tls.clientAuth` rule of every HTTP listener, by port: that of the first site on the port with a complete manual
/// certificate and key, the site whose certificate the listener serves (as `bound_listeners` in `api.rs` decides it). The
/// rule is loaded into the listener once, when it is built.
pub(super) fn https_client_auth_by_port(
    cfg: &crate::config::schema::AppConfig,
) -> std::collections::BTreeMap<u16, Option<&crate::config::schema::TlsClientAuth>> {
    let mut out: std::collections::BTreeMap<u16, Option<&crate::config::schema::TlsClientAuth>> =
        std::collections::BTreeMap::new();
    let mut decided = std::collections::BTreeSet::new();
    for site in cfg.sites.iter().filter(|s| s.tcp.is_none()) {
        let port = site
            .port
            .unwrap_or(if site.tls.is_some() { 443 } else { 80 });
        out.entry(port).or_insert(None);
        if decided.contains(&port) {
            continue;
        }
        if let Some(t) = site.tls.as_ref() {
            if t.cert.is_some() && t.key.is_some() {
                out.insert(port, t.client_auth.as_ref());
                decided.insert(port);
            }
        }
    }
    out
}

/// The `tls` block of every TCP site that has one (a TCP site that terminates TLS), by the port it listens on: the
/// certificate, the key and the client-certificate rule are loaded once, when the listener is built.
pub(super) fn tcp_tls_by_port(
    cfg: &crate::config::schema::AppConfig,
) -> std::collections::BTreeMap<u16, &crate::config::schema::TlsConfig> {
    let mut out = std::collections::BTreeMap::new();
    for site in &cfg.sites {
        if let (Some(_), Some(tls)) = (site.tcp.as_ref(), site.tls.as_ref()) {
            out.entry(site.port.unwrap_or(80)).or_insert(tls);
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
