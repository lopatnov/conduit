use std::sync::Arc;

use async_trait::async_trait;
use pingora_core::server::ShutdownWatch;
use pingora_core::services::background::BackgroundService;

use crate::proxy::service::AppState;

pub mod server;
pub use server::run_upload_server;

/// Pingora `BackgroundService` that drives the Axum file-upload server.
///
/// The underlying `std::net::TcpListener` is pre-bound in [`run_server`] so
/// that the OS-assigned port is known before Pingora starts accepting proxy
/// traffic.  The listener is converted to a Tokio listener inside `start()`,
/// which runs on Pingora's async runtime.
pub struct UploadService {
    pub state: Arc<AppState>,
    /// Pre-bound standard listener.  Wrapped in a `Mutex<Option<…>>` so it
    /// can be taken exactly once when `start()` is called.
    listener: tokio::sync::Mutex<Option<std::net::TcpListener>>,
}

impl UploadService {
    pub fn new(state: Arc<AppState>, listener: std::net::TcpListener) -> Self {
        Self {
            state,
            listener: tokio::sync::Mutex::new(Some(listener)),
        }
    }
}

#[async_trait]
impl BackgroundService for UploadService {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        let std_listener = self
            .listener
            .lock()
            .await
            .take()
            .expect("UploadService::start called twice");

        let listener = match tokio::net::TcpListener::from_std(std_listener) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("upload server: failed to convert listener: {e}");
                return;
            }
        };

        if let Ok(addr) = listener.local_addr() {
            tracing::info!("upload server listening on http://{addr}");
        }

        axum::serve(listener, server::make_upload_router(self.state.clone()))
            .with_graceful_shutdown(async move {
                shutdown.changed().await.ok();
            })
            .await
            .ok();
    }
}
