use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use pingora_core::server::configuration::{Opt, ServerConf};
use pingora_core::server::Server;
use pingora_core::services::background::background_service;
use pingora_core::services::listening::Service as ListeningService;
use pingora_proxy::{http_proxy_service, HttpProxy};

use crate::admin::api::AdminApiService;
// DEFAULT_ADMIN_BIND is still the default for CLI commands (conduit reload etc.)
// but is no longer a fallback for the server-side HTTP binding.
#[allow(unused_imports)]
use crate::config::defaults::{DEFAULT_ADMIN_BIND, DEFAULT_SHUTDOWN_TIMEOUT_SECS};
use crate::config::schema::{AppConfig, SiteConfig};
use conduit_runtime::proxy::service::{AppState, ConduitProxy};
#[cfg(feature = "upload")]
use conduit_runtime::upload::UploadService;

#[cfg(feature = "acme")]
use super::acme_certs::{obtain_acme_certs, plan_acme_renewals};
use super::config_watch::spawn_config_update_watcher;
use super::listeners::{add_tls_listeners, build_http_server_options, classify_ports, TlsPortMap};
use super::proxy_protocol::{HttpProxyWithProxyProtocol, PreTlsProxyProtocol};
#[cfg(feature = "redis")]
use super::redis_bootstrap::connect_redis_rate_limiter_if_configured;

/// Bind a loopback TCP listener for the upload server if any site has `upload` configured.
///
/// Uses `std::net::TcpListener` (synchronous) so it can run before the Pingora async runtime
/// starts.  The listener is converted to Tokio inside `UploadService::start()`.
///
/// Returns `(addr, listener)` — both `None` when no site needs an upload server.
fn bind_upload_listener_if_needed(
    config: &AppConfig,
) -> anyhow::Result<(Option<std::net::SocketAddr>, Option<std::net::TcpListener>)> {
    #[cfg(not(feature = "upload"))]
    {
        // When upload feature is disabled, warn if any site configures upload.
        if config.sites.iter().any(|s| s.upload.is_some()) {
            tracing::warn!(
                "One or more sites configure 'upload' but Conduit was compiled without \
                 --features upload — file upload is disabled."
            );
        }
        Ok((None, None))
    }
    #[cfg(feature = "upload")]
    {
        if !config.sites.iter().any(|s| s.upload.is_some()) {
            return Ok((None, None));
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|e| anyhow::anyhow!("failed to bind upload server: {e}"))?;
        let addr = listener
            .local_addr()
            .map_err(|e| anyhow::anyhow!("upload listener local_addr: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| anyhow::anyhow!("upload listener set_nonblocking: {e}"))?;
        Ok((Some(addr), Some(listener)))
    }
}

/// Build the Pingora `ServerConf` from `global.workers` (issue #226) and
/// `global.shutdownTimeoutSecs` (issue #489).
///
/// Previously `global.workers` was parsed and tracked as a cold-reload
/// field but never threaded into the actual server construction, so it had
/// zero effect regardless of what an operator set. `ServerConf::threads` is
/// "how many threads each service gets" (pingora-core's own doc comment);
/// falls back to `ServerConf::default().threads` when unset, matching
/// pingora's own default and this field's pre-existing (accidental) effect.
fn build_server_conf(config: &AppConfig) -> ServerConf {
    ServerConf {
        threads: config
            .global
            .as_ref()
            .and_then(|g| g.workers)
            .unwrap_or_else(|| ServerConf::default().threads),
        // Pingora sleeps this whole period on a graceful terminate before it shuts the
        // runtimes down; it does not end early once connections drain (issue #489).
        grace_period_seconds: Some(
            config
                .global
                .as_ref()
                .and_then(|g| g.shutdown_timeout_secs)
                .unwrap_or(DEFAULT_SHUTDOWN_TIMEOUT_SECS),
        ),
        ..Default::default()
    }
}

/// Refuse a configuration that has hard validation errors. [`run_server`] calls this first, so that a
/// caller other than the CLI (which validates and logs the warnings itself) cannot start the server on a
/// config `validate` rejects: for a `wasm` middleware whose engine cannot be built, that would be a
/// server on which every plugin lets the request through (issue #626, PR #631 review).
fn refuse_invalid_config(config: &AppConfig) -> anyhow::Result<()> {
    let (_warnings, hard_errors) =
        crate::config::validate::partition_by_severity(crate::config::validate::validate(config));
    if hard_errors.is_empty() {
        return Ok(());
    }
    let messages: Vec<String> = hard_errors
        .iter()
        .map(|e| format!("{}: {}", e.path, e.message))
        .collect();
    anyhow::bail!("config validation errors:\n{}", messages.join("\n"))
}

/// Install the process-wide rustls crypto provider before creating any TLS clients or listeners.
///
/// Conduit selects `ring`; if another provider is already installed, rustls keeps that provider.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn build_http_proxy_service<A>(
    inner_proxy: A,
    sites: &[SiteConfig],
    port_tls: &TlsPortMap,
    port_plain: &HashSet<u16>,
) -> anyhow::Result<ListeningService<HttpProxyWithProxyProtocol<A>>> {
    // Read PROXY headers at the right transport boundary: before TLS handshakes, or before plain HTTP parsing (issues
    // #42 and #757).
    let (tls_proxy_protocol, http_proxy_protocol) = PreTlsProxyProtocol::for_sites(
        sites,
        |p| port_tls.contains_key(&p),
        |p| port_plain.contains(&p) && !port_tls.contains_key(&p),
    )?;
    let mut proxy_service = ListeningService::new(
        "Conduit HTTP Proxy".to_owned(),
        HttpProxyWithProxyProtocol::new(inner_proxy, http_proxy_protocol.clone()),
    );
    if let Some(hook) = tls_proxy_protocol {
        tracing::info!(ports = ?hook.ports(), "PROXY protocol required on HTTPS ports");
        proxy_service.endpoints().set_pre_tls_callback(hook);
    }
    if let Some(hook) = http_proxy_protocol {
        tracing::info!(ports = ?hook.ports(), "PROXY protocol required on plain HTTP ports");
    }
    add_tls_listeners(&mut proxy_service, port_tls)?;

    // Add plain TCP listeners for ports that are not TLS.
    for port in port_plain {
        if !port_tls.contains_key(port) {
            proxy_service.add_tcp(&format!("0.0.0.0:{port}"));
        }
    }

    Ok(proxy_service)
}

/// Start the Conduit server.
///
/// - `config` — initial [`AppConfig`] to serve
/// - `config_path` — path used by `POST /reload`; pass [`PathBuf::new()`]
///   when configuration comes from a live provider (e.g. Kubernetes)
/// - `config_updates` — optional live-update channel; when `Some`, a background
///   thread watches the receiver and hot-swaps the config on every received
///   [`AppConfig`] (used by the Kubernetes provider)
pub fn run_server(
    config: AppConfig,
    config_path: PathBuf,
    config_updates: Option<tokio::sync::mpsc::Receiver<AppConfig>>,
) -> anyhow::Result<()> {
    refuse_invalid_config(&config)?;

    install_crypto_provider();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        config = %config_path.display(),
        sites = config.sites.len(),
        "conduit starting"
    );

    // Initialise OpenTelemetry OTLP tracing if configured.
    if let Some(otlp_cfg) = config.global.as_ref().and_then(|g| g.otlp.as_ref()) {
        if let Err(e) = crate::server::otel::init_tracer(otlp_cfg) {
            tracing::warn!(error = %e, "failed to initialise OTLP tracing — continuing without traces");
        }
    }

    // Only bind the Admin HTTP server when global.admin is explicitly configured.
    let admin_bind: Option<String> = config
        .global
        .as_ref()
        .and_then(|g| g.admin.as_ref())
        .and_then(|a| a.bind.as_deref())
        .map(str::to_owned);

    // Bind the upload server listener before creating AppState.
    #[cfg_attr(not(feature = "upload"), allow(unused_variables))]
    let (upload_addr, upload_std_listener) = bind_upload_listener_if_needed(&config)?;

    // Create AppState.
    let state = {
        #[cfg(feature = "redis")]
        {
            let redis_rl = connect_redis_rate_limiter_if_configured(&config)?;
            Arc::new(AppState::new_with_redis(
                config.clone(),
                config_path,
                upload_addr,
                redis_rl,
            ))
        }
        #[cfg(not(feature = "redis"))]
        Arc::new(AppState::new(config.clone(), config_path, upload_addr))
    };

    // Spawn a background thread to hot-swap config from a live provider channel.
    if let Some(rx) = config_updates {
        spawn_config_update_watcher(rx, state.clone());
    }

    // Phase 3.1: ACME certificate procurement.
    #[cfg(feature = "acme")]
    let acme_certs = obtain_acme_certs(&config, &state.acme_challenges)?;
    #[cfg(not(feature = "acme"))]
    let acme_certs: std::collections::HashMap<u16, (String, String)> =
        std::collections::HashMap::new();

    let opt = Opt {
        upgrade: false,
        daemon: false,
        nocapture: false,
        test: false,
        conf: None,
    };
    let server_conf = build_server_conf(&config);
    let mut server = Server::new_with_opt_and_conf(Some(opt), server_conf);
    server.bootstrap();

    let proxy = ConduitProxy {
        state: state.clone(),
    };

    let server_options = build_http_server_options(&config.sites);

    // Create HttpProxy with options. A thin outer app reads PROXY headers on plain HTTP ports before Pingora parses them.
    let mut inner_proxy = HttpProxy::new(proxy, server.configuration.clone());
    inner_proxy.server_options = server_options;
    inner_proxy.handle_init_modules();

    let (port_tls, port_plain) = classify_ports(&config.sites, &acme_certs);
    let proxy_service =
        build_http_proxy_service(inner_proxy, &config.sites, &port_tls, &port_plain)?;
    server.add_service(proxy_service);

    // Raw TCP proxy services.
    #[cfg(feature = "tcp")]
    let tcp_ports = register_tcp_proxy_services(&config, &mut server)?;
    #[cfg(not(feature = "tcp"))]
    let tcp_ports: Vec<u16> = Vec::new();

    // HTTP → HTTPS redirect services.
    let redirect_ports = register_http_redirect_services(&config, &state, &mut server);

    // ACME renewal is planned from what was actually bound. Plain proxy and
    // redirect listeners answer `/.well-known/acme-challenge/` from the shared
    // challenge map; TLS and raw TCP listeners cannot answer the CA at all.
    #[cfg(feature = "acme")]
    let acme_renewals = {
        let token_ports: std::collections::HashSet<u16> = port_plain
            .iter()
            .copied()
            .filter(|p| !port_tls.contains_key(p))
            .chain(redirect_ports.iter().copied())
            .collect();
        let other_ports: std::collections::HashSet<u16> = port_tls
            .keys()
            .copied()
            .chain(tcp_ports.iter().copied())
            .collect();
        plan_acme_renewals(&config, &token_ports, &other_ports)
    };
    #[cfg(not(feature = "acme"))]
    let _ = (&redirect_ports, &tcp_ports);

    let admin = AdminApiService {
        state: state.clone(),
        bind: admin_bind,
        #[cfg(feature = "acme")]
        acme_renewals,
    };
    server.add_service(background_service("admin-api", admin));

    // Upload server background service.
    #[cfg(feature = "upload")]
    if let Some(std_listener) = upload_std_listener {
        let upload_svc = UploadService::new(state, std_listener);
        server.add_service(background_service("upload-server", upload_svc));
    }

    server.run_forever()
}

/// Register raw TCP proxy services for sites with `tcp` config.
#[cfg(feature = "tcp")]
///
/// Returns the ports that were bound.
fn register_tcp_proxy_services(
    config: &AppConfig,
    server: &mut Server,
) -> anyhow::Result<Vec<u16>> {
    let mut bound = Vec::new();
    for site in &config.sites {
        let Some(ref tcp_cfg) = site.tcp else {
            continue;
        };
        if tcp_cfg.targets.is_empty() {
            tracing::warn!("TCP site on port {:?} has no targets — skipped", site.port);
            continue;
        }
        let port = site.port.unwrap_or(80);
        let mut proxy = conduit_tcp::proxy::TcpProxy::new(tcp_cfg).with_port(port);
        let addr = format!("0.0.0.0:{port}");
        // Written before the service takes the proxy: it needs the settings only to bind the listener.
        let tls_settings = tcp_tls_settings(site, port)?;
        let terminates_tls = tls_settings.is_some();
        // With TLS the header comes before the handshake, so the pre-TLS hook reads it (and the relay does not);
        // without TLS the relay reads it.
        let hook = match (&site.proxy_protocol, terminates_tls) {
            (Some(pp), true) => Some(PreTlsProxyProtocol::for_tcp_site(port, pp)?),
            (Some(pp), false) => {
                proxy = proxy.with_proxy_protocol(pp);
                None
            }
            (None, _) => None,
        };
        let mut tcp_svc = ListeningService::new(format!("Conduit TCP Proxy :{port}"), proxy);
        match tls_settings {
            Some(settings) => tcp_svc.add_tls_with_settings(&addr, None, settings),
            None => tcp_svc.add_tcp(&addr),
        }
        if let Some(hook) = hook {
            tcp_svc.endpoints().set_pre_tls_callback(hook);
        }
        server.add_service(tcp_svc);
        bound.push(port);
        tracing::info!(
            port,
            targets = tcp_cfg.targets.join(", "),
            tls = terminates_tls,
            client_cert_required = site
                .tls
                .as_ref()
                .and_then(|t| t.client_auth.as_ref())
                .is_some_and(|c| !c.optional),
            proxy_protocol = site.proxy_protocol.is_some(),
            "TCP proxy service registered"
        );
    }
    Ok(bound)
}

/// The TLS settings of a `tcp` site that terminates TLS (`tls` with a certificate and key), or `None` for a plain one.
///
/// A site with a `tls` block and no certificate and key (or one with `acme`) is refused at validation; the error here is
/// the second guard, so a port that was meant to be TLS can never come up as plaintext.
#[cfg(feature = "tcp")]
fn tcp_tls_settings(
    site: &crate::config::schema::SiteConfig,
    port: u16,
) -> anyhow::Result<Option<pingora_core::listeners::tls::TlsSettings>> {
    use super::tls as tls_util;
    let Some(tls) = &site.tls else {
        return Ok(None);
    };
    let (Some(cert), Some(key)) = (&tls.cert, &tls.key) else {
        anyhow::bail!(
            "tcp site on port {port} has a tls block without cert and key: not serving it as plaintext"
        );
    };
    let settings = match &tls.client_auth {
        Some(ca) => tls_util::make_tls_settings_with_client_auth(cert, key, false, ca),
        None => tls_util::make_tls_settings(cert, key, false),
    }
    .map_err(|e| anyhow::anyhow!("TLS setup failed for tcp site on port {port}: {e}"))?;
    Ok(Some(settings))
}

/// Register HTTP → HTTPS redirect services for sites with `tls.httpRedirectPort`.
///
/// Returns the ports that were bound.
fn register_http_redirect_services(
    config: &AppConfig,
    state: &Arc<AppState>,
    server: &mut Server,
) -> Vec<u16> {
    let mut bound = Vec::new();
    for site in &config.sites {
        let tls_port = site
            .port
            .unwrap_or(if site.tls.is_some() { 443 } else { 80 });
        if let Some(http_port) = site.tls.as_ref().and_then(|t| t.http_redirect_port) {
            let redirect = crate::server::redirect::RedirectProxy::for_sites(
                tls_port,
                http_port,
                &config.sites,
                state.acme_challenges.clone(),
            );
            let mut redirect_svc = http_proxy_service(&server.configuration, redirect);
            redirect_svc.add_tcp(&format!("0.0.0.0:{http_port}"));
            server.add_service(redirect_svc);
            bound.push(http_port);
        }
    }
    bound
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::GlobalConfig;

    #[test]
    fn builds_plain_http_proxy_service_with_its_proxy_protocol_policy() {
        let config = crate::config::from_str(
            r#"[{ "port": 8080, "proxyProtocol": { "trustedSources": ["10.0.0.0/8"] } }]"#,
        )
        .expect("valid HTTP PROXY protocol config");
        let (port_tls, port_plain) =
            classify_ports(&config.sites, &std::collections::HashMap::new());

        let mut service = build_http_proxy_service((), &config.sites, &port_tls, &port_plain)
            .expect("build HTTP service");

        assert_eq!(service.endpoints().addresses(), vec!["0.0.0.0:8080"]);
    }

    #[test]
    fn a_config_with_hard_validation_errors_is_refused_and_a_valid_one_is_not() {
        let duplicate = crate::config::from_str(r#"[{ "port": 8080 }, { "port": 8080 }]"#).unwrap();
        let err = refuse_invalid_config(&duplicate).unwrap_err().to_string();
        assert!(err.starts_with("config validation errors:"), "{err}");
        assert!(err.contains("Duplicate"), "{err}");

        let fine = crate::config::from_str(r#"[{ "port": 8080 }, { "port": 8081 }]"#).unwrap();
        assert!(refuse_invalid_config(&fine).is_ok());
    }

    /// Regression test for #226: `global.workers` must actually reach
    /// `ServerConf.threads`, not just round-trip through parsing/validation.
    #[test]
    fn build_server_conf_uses_configured_workers() {
        let config = AppConfig {
            global: Some(GlobalConfig {
                workers: Some(8),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(build_server_conf(&config).threads, 8);
    }

    #[test]
    fn build_server_conf_applies_shutdown_timeout_secs() {
        let config = AppConfig {
            global: Some(GlobalConfig {
                shutdown_timeout_secs: Some(7),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(build_server_conf(&config).grace_period_seconds, Some(7));
        assert_eq!(
            build_server_conf(&AppConfig::default()).grace_period_seconds,
            Some(DEFAULT_SHUTDOWN_TIMEOUT_SECS)
        );
    }

    #[test]
    fn build_server_conf_defaults_when_workers_unset() {
        let config = AppConfig {
            global: Some(GlobalConfig::default()),
            ..Default::default()
        };
        assert_eq!(
            build_server_conf(&config).threads,
            ServerConf::default().threads
        );
    }

    /// A self-signed certificate and key written under `dir`; returns their paths.
    #[cfg(feature = "tcp")]
    fn write_cert(dir: &tempfile::TempDir) -> (String, String) {
        let key_pair = rcgen::KeyPair::generate().expect("keygen");
        let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .expect("params")
            .self_signed(&key_pair)
            .expect("self-signed cert");
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        std::fs::write(&cert_path, cert.pem()).expect("write cert");
        std::fs::write(&key_path, key_pair.serialize_pem()).expect("write key");
        (
            cert_path.to_string_lossy().into_owned(),
            key_path.to_string_lossy().into_owned(),
        )
    }

    #[cfg(feature = "tcp")]
    #[test]
    fn builds_https_proxy_service_with_a_tls_proxy_protocol_callback() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = write_cert(&dir);
        let config = crate::config::from_str(
            &serde_json::json!([{
                "port": 8443,
                "tls": { "cert": cert, "key": key },
                "proxyProtocol": { "trustedSources": ["10.0.0.0/8"] }
            }])
            .to_string(),
        )
        .expect("valid HTTPS PROXY protocol config");
        let (port_tls, port_plain) =
            classify_ports(&config.sites, &std::collections::HashMap::new());
        let mut service = build_http_proxy_service((), &config.sites, &port_tls, &port_plain)
            .expect("build HTTPS service");

        assert_eq!(service.endpoints().addresses(), vec!["0.0.0.0:8443"]);
    }

    #[cfg(feature = "tcp")]
    fn sites(value: serde_json::Value) -> AppConfig {
        crate::config::from_str(&value.to_string()).expect("a valid test config")
    }

    #[cfg(feature = "tcp")]
    #[test]
    fn a_tcp_site_terminates_tls_only_with_a_certificate_and_key() {
        // The client-certificate verifier needs a process-default crypto provider; install it here so the test also
        // passes when run alone or first, as the sibling TLS tests do.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = write_cert(&dir);
        let site = |tls: serde_json::Value| {
            sites(serde_json::json!([{ "port": 9000, "tcp": { "targets": ["a:1"] }, "tls": tls }]))
                .sites[0]
                .clone()
        };
        let plain = sites(serde_json::json!([{ "port": 9000, "tcp": { "targets": ["a:1"] } }]));
        assert!(tcp_tls_settings(&plain.sites[0], 9000).unwrap().is_none());

        let with_cert = site(serde_json::json!({ "cert": cert, "key": key }));
        assert!(tcp_tls_settings(&with_cert, 9000).unwrap().is_some());

        // the certificate doubles as the CA that signs the client certificates
        let with_ca =
            site(serde_json::json!({ "cert": cert, "key": key, "clientAuth": { "ca": cert } }));
        assert!(tcp_tls_settings(&with_ca, 9000).unwrap().is_some());

        // a tls block that cannot be served must never come up as plaintext
        let no_pair = site(serde_json::json!({}));
        let err = tcp_tls_settings(&no_pair, 9000)
            .err()
            .expect("refused")
            .to_string();
        assert!(err.contains("not serving it as plaintext"), "{err}");

        // Pingora reads the certificate and key when the listener is built, but the CA of `clientAuth` right here
        let missing_ca = site(
            serde_json::json!({ "cert": cert, "key": key, "clientAuth": { "ca": "/nonexistent/ca.pem" } }),
        );
        let err = tcp_tls_settings(&missing_ca, 9000)
            .err()
            .expect("refused")
            .to_string();
        assert!(
            err.contains("TLS setup failed for tcp site on port 9000"),
            "{err}"
        );
    }

    #[cfg(feature = "tcp")]
    #[test]
    fn tcp_services_are_registered_for_plain_tls_and_proxy_protocol_sites() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = write_cert(&dir);
        let pp = serde_json::json!({ "trustedSources": ["10.0.0.0/8"] });
        let config = sites(serde_json::json!([
            { "port": 9000, "tcp": { "targets": ["a:1"] } },
            { "port": 9001, "tcp": { "targets": ["a:1"] }, "proxyProtocol": pp },
            { "port": 9002, "tcp": { "targets": ["a:1"] }, "tls": { "cert": cert, "key": key } },
            { "port": 9003, "tcp": { "targets": ["a:1"] }, "tls": { "cert": cert, "key": key,
                "clientAuth": { "ca": cert } }, "proxyProtocol": pp },
            { "port": 9004, "tcp": { "targets": [] } },
            { "port": 9005 }
        ]));
        let mut server = Server::new(None).expect("a server that is not run");
        let bound = register_tcp_proxy_services(&config, &mut server).unwrap();
        // the site without targets is skipped, the HTTP site is not a tcp one
        assert_eq!(bound, vec![9000, 9001, 9002, 9003]);

        // the policy of a TLS site with proxyProtocol is found by its own port; port 0 cannot be
        let hook = PreTlsProxyProtocol::for_tcp_site(
            9003,
            config.sites[3].proxy_protocol.as_ref().unwrap(),
        )
        .unwrap();
        assert_eq!(hook.ports(), vec![9003]);
        let err =
            PreTlsProxyProtocol::for_tcp_site(0, config.sites[3].proxy_protocol.as_ref().unwrap())
                .err()
                .expect("port 0 is refused")
                .to_string();
        assert!(err.contains("port 0"), "{err}");
    }

    #[cfg(feature = "tcp")]
    #[test]
    fn a_tcp_site_with_an_unusable_tls_block_stops_the_registration() {
        let config = sites(serde_json::json!([
            { "port": 9000, "tcp": { "targets": ["a:1"] }, "tls": {} }
        ]));
        let mut server = Server::new(None).expect("a server that is not run");
        assert!(register_tcp_proxy_services(&config, &mut server).is_err());
    }

    #[test]
    fn build_server_conf_defaults_when_global_absent() {
        let config = AppConfig::default();
        assert_eq!(
            build_server_conf(&config).threads,
            ServerConf::default().threads
        );
    }
}
