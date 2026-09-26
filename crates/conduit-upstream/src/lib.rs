//! Upstream selection, health tracking, and load-balancing strategies for
//! conduit's feature-driven Cargo workspace migration (issue
//! [#114](https://github.com/lopatnov/conduit/issues/114), extracted in
//! [#142](https://github.com/lopatnov/conduit/issues/142)).
//!
//! ## Scope
//!
//! Owns everything moved from the root crate's `src/proxy/{upstream,strategy,
//! health}.rs`:
//!
//! - [`config`] — `LoadBalanceStrategy`/`ProxyTarget`/`WeightedTarget`/
//!   `UpstreamGroup`/`UpstreamHealthCheck`/`UpstreamTlsConfig`/
//!   `OutlierDetectionConfig` — the load-balancing/health-check config types.
//! - [`health`] — [`health::UpstreamRegistry`] (per-upstream health state,
//!   inflight connection counts, runtime upstream overrides), Peak EWMA
//!   latency tracking, Outlier Detection, active health-check/connection-
//!   warmup background tasks, and the half-open circuit breaker.
//! - [`strategy`] — [`strategy::LoadBalancingStrategy`] trait + all 8
//!   concrete strategy structs (`RoundRobin`, `Random`, `LeastConn`,
//!   `WeightedRoundRobin`, `HashBased`, `LeastResponseTime`, `P2cChoice`),
//!   dispatched via [`strategy::from_config`].
//! - [`targets`] — URL parsing helpers (`url_to_host_port`, `url_is_tls`,
//!   `url_host`, `fnv1a_hash`) and the plain-slice "pick" algorithms
//!   (`pick_round_robin`, `pick_random`, `pick_weighted_round_robin`,
//!   `pick_by_hash`, `pick_least_response_time`).
//!
//! ## Not an optional Cargo feature
//!
//! Unlike most sibling extractions, upstream selection/health tracking is
//! **always-compiled, core proxy functionality** — there is no
//! `--features upstream` to disable it, and every dependency in this crate's
//! `Cargo.toml` is mandatory, not `optional = true`. This crate has no
//! `[features]` table at all — the same always-on shape as
//! `conduit-ipfilter`/`conduit-cors`/`conduit-security-headers`/
//! `conduit-redirects`/`conduit-metrics` (`CLAUDE.md` decision #31), just for
//! a reason specific to this domain (there is no config-visible on/off
//! switch for load balancing itself) rather than that decision's "light
//! logic, no heavy third-party dependency" rationale.
//!
//! No dependency on `lopatnov-conduit-core` either — nothing here implements
//! `RequestFilter`/`ResponseFilter` (see `CONTRIBUTING.md`'s "conduit-core
//! dependency is opt-in, not automatic"); the Pingora `ProxyHttp` trait-
//! method bodies stay in the root crate's `request_phase.rs`/
//! `response_phase.rs`, calling into this crate's plain functions and the
//! [`health::UpstreamRegistry`] type (routing itself — `router.rs`/
//! `routes.rs`/`capacity.rs` — moved into `crates/conduit-proxy-http`, issue
//! #114/#143, which depends on this crate the same way).
//!
//! ## `ProxyConfig`/`ProxyRouteTarget`/`ProxyTarget`... wait, `ProxyTarget`
//! moved here — what didn't?
//!
//! [`config::ProxyTarget`]/[`config::WeightedTarget`] moved here because
//! [`config::UpstreamGroup`] (also in scope for this extraction) embeds
//! `Vec<ProxyTarget>` directly — they had to travel together. `ProxyConfig`/
//! `ProxyRouteTarget`/`ProxyRouteConfig` themselves were a different story
//! *at the time of this extraction*: `ProxyRouteConfig` is a large struct
//! that also embeds `CacheConfig`/`RetryConfig`/`ConnectionPoolConfig`/
//! `RateLimitConfig`/etc. — none of which belong in an upstream-selection
//! crate, and none of which were extracted yet. Moving `ProxyRouteTarget`/
//! `ProxyConfig` here to satisfy the four functions that used to consume
//! them (`target_urls`, `weighted_targets`, `target_urls_from_proxy`,
//! `strip_prefix_enabled`) would have forced `ProxyRouteConfig` to move too
//! — a genuine circular dependency with several other not-yet-extracted
//! crates, not something this extraction's own scope (issue #142) asked for
//! or should improvise around. Those four functions **stayed behind** in
//! the root crate's own `src/proxy/upstream.rs` at the time, right next to
//! a facade re-export of everything that *did* move.
//!
//! **This is now resolved** — `crates/conduit-proxy-http` (issue #114/#143,
//! Phase 5.2) moved `ProxyConfig`/`ProxyRouteTarget`/`ProxyRouteConfig`
//! (plus `CacheConfig`/`RetryConfig`/`ConnectionPoolConfig`/`RateLimitConfig`
//! links they need) out of the root crate, and with them 3 of the 4
//! deferred functions (`target_urls`/`weighted_targets`/
//! `target_urls_from_proxy`, now in that crate's own `targets` module).
//! `strip_prefix_enabled` turned out to be dead code (no real production
//! call site) and was deleted rather than moved. This module's doc comment
//! is kept as a historical record of the deferral, not a current claim.
//! This is the direct analog of `conduit-hotreload`'s `build_watch_config`
//! taking narrower `(Option<&HotReloadConfig>, Option<&StaticConfig>)` pairs
//! instead of `&AppConfig` (issue #114/#140): apply the same "narrower slice
//! instead of a root-only type" fix wherever a moved function's signature
//! would otherwise force a dependency this crate must not have.
//!
//! ## `AppConfig` isn't available here either
//!
//! [`health::spawn_health_checks`]/[`health::spawn_connection_warmup`] used
//! to take `&AppConfig` directly (to iterate `config.sites` and find every
//! `healthCheck`-configured route) — `AppConfig`/`ProxyConfig`/
//! `ProxyRouteTarget` are all still root-crate-only types for the same
//! reason described above, so both functions now take an iterator of
//! already-resolved `(&config::UpstreamHealthCheck, &[String])` pairs
//! instead. The root crate's own call site
//! (`admin/api.rs::spawn_upstream_probes`, which builds the slice with
//! `health_check_routes`) resolves `AppConfig` down to that
//! shape before calling in — the exact same pattern `conduit-hotreload`'s
//! `build_watch_config` uses for its own analogous problem (issue
//! #114/#140), applied here to a second function pair in the same
//! extraction rather than a different design.

pub mod config;
pub mod health;
pub mod strategy;
pub mod targets;

pub use config::{
    LoadBalanceStrategy, OutlierDetectionConfig, ProxyTarget, UpstreamGroup, UpstreamHealthCheck,
    UpstreamTlsConfig, WeightedTarget,
};
