use std::collections::HashSet;
use std::sync::Arc;

use pingora_core::server::configuration::Opt;
use pingora_core::server::Server;
use pingora_core::services::background::background_service;
use pingora_proxy::http_proxy_service;

use crate::admin::api::AdminApiService;
use crate::config::defaults::DEFAULT_ADMIN_BIND;
use crate::config::schema::AppConfig;
use crate::proxy::service::{AppState, ConduitProxy};

pub fn run_server(config: AppConfig) -> anyhow::Result<()> {
    let admin_bind = config
        .global
        .as_ref()
        .and_then(|g| g.admin.as_ref())
        .and_then(|a| a.bind.as_deref())
        .unwrap_or(DEFAULT_ADMIN_BIND)
        .to_owned();

    let listen_addrs: Vec<String> = if config.sites.is_empty() {
        vec!["0.0.0.0:8080".to_owned()]
    } else {
        config
            .sites
            .iter()
            .map(|s| {
                let port = s.port.unwrap_or(if s.tls.is_some() { 443 } else { 80 });
                format!("0.0.0.0:{port}")
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    };

    let state = Arc::new(AppState::new(config));

    let opt = Opt {
        upgrade: false,
        daemon: false,
        nocapture: false,
        test: false,
        conf: None,
    };
    let mut server = Server::new(Some(opt))?;
    server.bootstrap();

    // Proxy service
    let proxy = ConduitProxy {
        state: state.clone(),
    };
    let mut proxy_service = http_proxy_service(&server.configuration, proxy);
    for addr in &listen_addrs {
        proxy_service.add_tcp(addr);
    }
    server.add_service(proxy_service);

    // Admin API background service
    let admin = AdminApiService {
        state,
        bind: admin_bind,
    };
    server.add_service(background_service("admin-api", admin));

    server.run_forever()
}
