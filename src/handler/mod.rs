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
/// 1. Create a module under `src/handler/` and write the core async function.
/// 2. Expose a struct that implements `LocalHandlerImpl` — this allows the
///    handler to be tested in isolation and called polymorphically.
/// 3. Add a new variant to `LocalHandler` in `proxy/ctx.rs`.
/// 4. Route to it from `proxy/router.rs`.
/// 5. Dispatch it in `proxy/service.rs:dispatch_local()`.
///
/// `LocalHandlerImpl` provides a stable interface for testing handlers
/// independently of the full proxy pipeline. The runtime dispatch in
/// `dispatch_local` currently goes through the `HandlerKind` match; a full
/// registry migration is tracked in GitHub issue #14 (**note:** the actual
/// issue # may differ — search for "handler registry" in the issue tracker).
#[async_trait]
pub trait LocalHandlerImpl: Send + Sync {
    /// Execute the handler, writing a complete response to `session`.
    async fn handle(&self, session: &mut Session) -> Result<()>;
}
