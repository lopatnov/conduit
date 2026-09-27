//! Binding and serving the Admin API.

use axum::Router;
use pingora_core::server::ShutdownWatch;
use std::sync::Arc;
use tokio::net::TcpListener;

use conduit_runtime::proxy::service::AppState;

use super::router::build_router;

/// Bind the Admin HTTP server on `bind_addr` and serve it until `shutdown` fires.
///
/// `extra` is the routes contributed by the layer above this module — see [`build_router`].
pub async fn serve(
    state: Arc<AppState>,
    bind_addr: &str,
    extra: Router<Arc<AppState>>,
    mut shutdown: ShutdownWatch,
) {
    let app = build_router(state, extra);
    let listener = match TcpListener::bind(bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("admin API failed to bind {bind_addr}: {e}");
            return;
        }
    };
    let addr = listener.local_addr().ok();
    if let Some(addr) = addr {
        tracing::info!("admin API listening on http://{addr}");
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.changed().await.ok();
        })
        .await
        .ok();
}
