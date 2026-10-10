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
use crate::config::defaults::DEFAULT_ADMIN_BIND;
use crate::config::schema::AppConfig;
use conduit_runtime::proxy::service::{AppState, ConduitProxy};
#[cfg(feature = "upload")]
use conduit_runtime::upload::UploadService;

#[cfg(feature = "acme")]
use super::acme_certs::{obtain_acme_certs, plan_acme_renewals};
use super::config_watch::spawn_config_update_watcher;
use super::listeners::{add_tls_listeners, build_http_server_options, classify_ports};
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

/// Build the Pingora `ServerConf` from `global.workers` (issue #226).
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
        ..Default::default()
    }
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
    // Install the ring crypto provider for rustls before any TLS initialization.
    // This is a no-op if another provider was already installed (e.g., in tests).
    let _ = rustls::crypto::ring::default_provider().install_default();

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

    // Create HttpProxy with options, then wrap in a listening service.
    let mut inner_proxy = HttpProxy::new(proxy, server.configuration.clone());
    inner_proxy.server_options = server_options;
    inner_proxy.handle_init_modules();
    let mut proxy_service = ListeningService::new("Conduit HTTP Proxy".to_owned(), inner_proxy);

    let (port_tls, port_plain) = classify_ports(&config.sites, &acme_certs);
    add_tls_listeners(&mut proxy_service, &port_tls)?;

    // Add plain TCP listeners for ports that are not TLS.
    for port in &port_plain {
        if !port_tls.contains_key(port) {
            proxy_service.add_tcp(&format!("0.0.0.0:{port}"));
        }
    }

    server.add_service(proxy_service);

    // Raw TCP proxy services.
    #[cfg(feature = "tcp")]
    let tcp_ports = register_tcp_proxy_services(&config, &mut server);
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
fn register_tcp_proxy_services(config: &AppConfig, server: &mut Server) -> Vec<u16> {
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
        let proxy = conduit_tcp::proxy::TcpProxy::new(tcp_cfg);
        let mut tcp_svc = ListeningService::new(format!("Conduit TCP Proxy :{port}"), proxy);
        tcp_svc.add_tcp(&format!("0.0.0.0:{port}"));
        server.add_service(tcp_svc);
        bound.push(port);
        tracing::info!(
            port,
            targets = tcp_cfg.targets.join(", "),
            "TCP proxy service registered"
        );
    }
    bound
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
            use crate::server::redirect::RedirectProxy;
            let redirect = RedirectProxy::new(tls_port, state.acme_challenges.clone());
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

    #[test]
    fn build_server_conf_defaults_when_global_absent() {
        let config = AppConfig::default();
        assert_eq!(
            build_server_conf(&config).threads,
            ServerConf::default().threads
        );
    }
}
