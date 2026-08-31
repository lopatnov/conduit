use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use pingora_core::apps::HttpServerOptions;
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
use crate::config::schema::SiteConfig;
#[cfg(feature = "redis")]
use crate::filter::rate_limit_redis::RedisRateLimiter;
use crate::proxy::service::{AppState, ConduitProxy};
#[cfg(feature = "acme")]
use crate::server::acme as acme_util;
use crate::server::tls as tls_util;
#[cfg(feature = "upload")]
use crate::upload::UploadService;

/// Maps a TCP port to `(cert_path, key_path, h2_enabled)` for TLS-enabled ports.
/// (cert_path, key_path, http2_enabled, optional_client_auth)
type TlsPortMap = HashMap<
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

/// Connect to Redis for rate limiting if any site has a `redis://` store configured.
///
/// A temporary single-threaded Tokio runtime is used for the async handshake so
/// this can run from the synchronous `run_server`.  Connection failures are logged
/// as warnings and the server falls back to the in-memory limiter.
#[cfg(feature = "redis")]
fn connect_redis_rate_limiter_if_configured(
    config: &AppConfig,
) -> anyhow::Result<Option<Arc<RedisRateLimiter>>> {
    let url_opt = config.sites.iter().find_map(|s| {
        s.rate_limit
            .as_ref()
            .and_then(|rl| rl.store.as_deref())
            .filter(|s| s.starts_with("redis://") || s.starts_with("rediss://"))
            .map(str::to_owned)
    });
    let Some(ref url) = url_opt else {
        return Ok(None);
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("cannot build tokio runtime for Redis: {e}"))?;
    match rt.block_on(RedisRateLimiter::connect(url)) {
        Ok(rrl) => {
            tracing::info!("Redis rate limiter connected to {url}");
            Ok(Some(Arc::new(rrl)))
        }
        Err(e) => {
            tracing::warn!("Redis rate limiter unavailable ({url}): {e} — using memory fallback");
            Ok(None)
        }
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
    register_tcp_proxy_services(&config, &mut server);

    // HTTP → HTTPS redirect services.
    register_http_redirect_services(&config, &state, &mut server);

    let admin = AdminApiService {
        state: state.clone(),
        bind: admin_bind,
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

/// Spawn a background thread that hot-swaps config from a live provider channel.
fn spawn_config_update_watcher(
    mut rx: tokio::sync::mpsc::Receiver<AppConfig>,
    state: Arc<AppState>,
) {
    std::thread::Builder::new()
        .name("config-update-watcher".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime for config-update-watcher");
            rt.block_on(async move {
                while let Some(new_cfg) = rx.recv().await {
                    tracing::info!(
                        sites = new_cfg.sites.len(),
                        "live config update received — hot-swapping"
                    );
                    // Connect any Redis-backed proxy cache URL this update
                    // introduced (issue #330) before the swap below -- same
                    // reasoning as the admin API's /reload handler.
                    #[cfg(all(feature = "cache", feature = "redis"))]
                    crate::proxy::cache_redis::connect_all(&new_cfg).await;
                    state.config.store(Arc::new(new_cfg));
                }
                tracing::warn!("config update channel closed; live updates stopped");
            });
        })
        .expect("failed to spawn config-update-watcher thread");
}

/// Build `HttpServerOptions` from site configs (h2c + keepalive limit).
fn build_http_server_options(sites: &[SiteConfig]) -> Option<HttpServerOptions> {
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
fn add_tls_listeners(
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

/// Register raw TCP proxy services for sites with `tcp` config.
#[cfg(feature = "tcp")]
fn register_tcp_proxy_services(config: &AppConfig, server: &mut Server) {
    for site in &config.sites {
        let Some(ref tcp_cfg) = site.tcp else {
            continue;
        };
        if tcp_cfg.targets.is_empty() {
            tracing::warn!("TCP site on port {:?} has no targets — skipped", site.port);
            continue;
        }
        let port = site.port.unwrap_or(80);
        let proxy = crate::proxy::tcp::TcpProxy::new(tcp_cfg);
        let mut tcp_svc = ListeningService::new(format!("Conduit TCP Proxy :{port}"), proxy);
        tcp_svc.add_tcp(&format!("0.0.0.0:{port}"));
        server.add_service(tcp_svc);
        tracing::info!(
            port,
            targets = tcp_cfg.targets.join(", "),
            "TCP proxy service registered"
        );
    }
}

/// Register HTTP → HTTPS redirect services for sites with `tls.httpRedirectPort`.
fn register_http_redirect_services(config: &AppConfig, state: &Arc<AppState>, server: &mut Server) {
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
}

/// Obtain ACME certificates for every site that has `tls.acme` configured.
///
/// Runs a dedicated single-threaded Tokio runtime so that the async ACME flow
/// can be driven from the synchronous `run_server` function.
///
/// Returns a map of `port → (cert_path, key_path)` for successfully obtained
/// certificates.  Sites whose procurement fails are logged and excluded from
/// the map (they fall back to plain TCP).
#[cfg(feature = "acme")]
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
