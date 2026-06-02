#[cfg(feature = "acme")]
pub mod acme_challenge;
pub mod fallback;
pub mod health;
pub mod hot_reload;
pub mod metrics;
pub mod response;
pub mod static_files;

use async_trait::async_trait;
use pingora_core::Result;
use pingora_proxy::Session;

/// A local (non-proxied) request handler.
///
/// ## Adding a new handler
///
/// 1. Create a module under `src/handler/` with the core async function.
/// 2. Add a struct that implements `LocalHandlerImpl` — holds all data the
///    handler needs (extracted from `RequestCtx` and `AppState` at dispatch time).
/// 3. Add a new variant to `LocalHandler` in `proxy/ctx.rs`.
/// 4. Route to it from `proxy/router.rs`.
/// 5. Add a match arm to `ConduitProxy::build_handler` in `proxy/service.rs`.
///
/// No other changes to `dispatch_local` are required.
#[async_trait]
pub trait LocalHandlerImpl: Send + Sync {
    /// Execute the handler, writing a complete response to `session`.
    async fn handle(&mut self, session: &mut Session) -> Result<()>;
}
