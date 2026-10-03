//! Facade re-export — upstream health tracking (Peak EWMA, Outlier
//! Detection, active health-check/connection-warmup background tasks, the
//! half-open circuit breaker, and the runtime upstream-override registry)
//! moved to `crates/conduit-upstream::health` (issue #114/#142). Kept at
//! this path so `crate::proxy::health::...` call sites (routing, capacity
//! admission, slow-start ramp, the admin API, logging) keep compiling
//! unchanged.
//!
//! `spawn_health_checks`/`spawn_connection_warmup` changed shape as part of
//! this move: `conduit-upstream` doesn't (and must not) depend on
//! `AppConfig`/`ProxyConfig`/`ProxyRouteTarget`, which are still root-crate-
//! only types (a later migration phase — #143/#144 — extracts proxy routing
//! itself). Both functions now take an iterator of `(&UpstreamHealthCheck,
//! &[String])` pairs instead of `&AppConfig` directly — see
//! `admin/api.rs::health_check_routes`, the root-crate call site that
//! resolves `AppConfig` down to that shape before calling in (the same
//! pattern `conduit-hotreload`'s `build_watch_config` uses for its own
//! analogous problem, issue #114/#140).

pub use conduit_upstream::health::*;
