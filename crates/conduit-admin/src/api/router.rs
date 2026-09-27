//! The Admin API router: every endpoint behind the bearer-token layer, and the constant-time token comparison.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::routing::{delete, get, post};
use axum::Router;
use std::sync::Arc;

use conduit_runtime::proxy::service::AppState;

use super::cache_purge::cache_purge_handler;
use super::certs::certs_reload_handler;
use super::ip_deny::{ip_deny_add_handler, ip_deny_remove_handler};
use super::rate_limits::rate_limits_handler;
use super::status::{shutdown_handler, status_handler};
use super::upstreams::{
    upstreams_add_handler, upstreams_handler, upstreams_remove_handler, upstreams_weight_handler,
};

/// Constant-time byte-slice equality to prevent timing attacks on Bearer tokens.
///
/// A naive `a == b` short-circuits on the first differing byte, leaking how
/// many leading bytes the attacker guessed correctly.  This function always
/// inspects every byte of both slices regardless of where they diverge.
pub(super) fn subtle_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    // Length check is intentionally NOT constant-time: a different length
    // does not help an attacker who must guess the full token anyway, and
    // this avoids allocating a padded buffer.
    a.len() == b.len() && a.ct_eq(b).into()
}

/// Build the Admin API router: the built-in endpoints plus `extra`, all behind the bearer-token layer.
///
/// `extra` is merged **before** that layer is applied, and this is the only way to add a route, so a
/// route contributed from outside cannot skip authentication.
pub fn build_router(state: Arc<AppState>, extra: Router<Arc<AppState>>) -> Router {
    // Build the protected routes first.
    let protected = Router::new()
        .route("/status", get(status_handler))
        .route("/shutdown", post(shutdown_handler))
        .route("/upstreams", get(upstreams_handler))
        .route("/upstreams/add", post(upstreams_add_handler))
        .route("/upstreams/remove", post(upstreams_remove_handler))
        .route("/upstreams/weight", post(upstreams_weight_handler))
        .route("/cache/purge", delete(cache_purge_handler))
        .route("/rate-limits", get(rate_limits_handler))
        .route("/ip-deny", post(ip_deny_add_handler))
        .route("/ip-deny", delete(ip_deny_remove_handler))
        .route("/certs/reload", post(certs_reload_handler))
        .merge(extra);

    // Wrap with bearer-token auth middleware.
    // Read the token from the live config on every request so that POST /reload
    // can add, remove, or rotate the admin token without a process restart.
    let auth_state = state.clone();
    protected
        .layer(axum::middleware::from_fn(
            move |request: Request<Body>, next: Next| {
                let state = auth_state.clone();
                async move {
                    // Read the current token from the live (ArcSwap) config.
                    let required_token = state
                        .config
                        .load()
                        .global
                        .as_ref()
                        .and_then(|g| g.admin.as_ref())
                        .and_then(|a| a.token.clone());

                    let Some(token) = required_token else {
                        // No token configured — allow all requests.
                        return Ok(next.run(request).await);
                    };

                    let auth = request
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    let provided = auth.strip_prefix("Bearer ").map(str::trim).unwrap_or("");
                    // Constant-time comparison prevents timing-based brute force.
                    if subtle_eq(provided.as_bytes(), token.as_bytes()) {
                        Ok(next.run(request).await)
                    } else {
                        Err(StatusCode::UNAUTHORIZED)
                    }
                }
            },
        ))
        .with_state(state)
}
