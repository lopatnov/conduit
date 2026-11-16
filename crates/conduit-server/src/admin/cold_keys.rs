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

/// The `tls.clientAuth` rules that can decide an HTTP listener's client-certificate rule, by port, in config order: those
/// of the HTTP sites on the port with `acme` and of the first one with a complete manual cert and key. The listener takes
/// the rule of the first site that has a certificate when it is built (`classify_site_port` in `listeners.rs`); a manual
/// site always has one, so sites after it never matter (a second host with its own certificate stays a hot reload), while
/// an ACME site only maybe does (that is known at run time), so each ACME site before it is watched too.
pub(super) fn https_client_auth_by_port(
    cfg: &crate::config::schema::AppConfig,
) -> std::collections::BTreeMap<u16, Vec<Option<&crate::config::schema::TlsClientAuth>>> {
    let mut out: std::collections::BTreeMap<
        u16,
        Vec<Option<&crate::config::schema::TlsClientAuth>>,
    > = std::collections::BTreeMap::new();
    let mut decided = std::collections::BTreeSet::new();
    for site in cfg.sites.iter().filter(|s| s.tcp.is_none()) {
        let port = site
            .port
            .unwrap_or(if site.tls.is_some() { 443 } else { 80 });
        let rules = out.entry(port).or_default();
        let Some(t) = site.tls.as_ref() else {
            continue;
        };
        if decided.contains(&port) {
            continue;
        }
        let manual = t.acme.is_none() && t.cert.is_some() && t.key.is_some();
        if t.acme.is_some() || manual {
            rules.push(t.client_auth.as_ref());
        }
        if manual {
            decided.insert(port);
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
