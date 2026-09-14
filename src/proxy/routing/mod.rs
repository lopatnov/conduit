//! Per-request proxy-routing state, and (as of issue #143 PR A2) the
//! phase-split proxy-target resolution logic itself, grouped ahead of the
//! eventual `conduit-proxy-http` crate extraction (issue #143 PR B).
//!
//! - [`state`] — `ProxyReqState`/`RetryState`/`RouteRateLimit` (PR A1).
//! - [`outcome`] — `ProxyOutcome`/`ProxyResolution`/`ProxyUpstream`, the
//!   narrower return type replacing `RouteResolution` for every function in
//!   this module (PR A2) — see its own doc comment for why.
//! - [`options`] — `RouteOptions`/`ProxyCtx` config-extraction types.
//! - [`dispatch`] — site/local-path dispatch helpers (health, metrics,
//!   hot-reload, ACME, `find_site_idx`, RFC 9218 `Priority` parsing).
//! - [`resolve`] — the `proxy` map (`site.proxy` as a `Routes` map) target
//!   resolution orchestrator + helpers.
//! - [`peer_pick`] — candidate-pool building + peer/retry selection for
//!   `resolve`'s orchestrator, split into its own file purely to keep
//!   `resolve.rs` under the 400-production-line soft limit.
//! - [`groups`] — two-level (grouped) upstream routing.
//! - [`sticky`] — sticky-session resolution + HMAC helpers.
//! - [`retry`] — retry-state construction.
//! - [`routes_resolve`] — `routes[]` array target resolution (routes.rs's
//!   own equivalent of `resolve`, deliberately not sharing helpers with it —
//!   see its own doc comment).
//!
//! `router.rs`/`routes.rs` remain the root-crate entry points: they build
//! the final `RouteResolution` (which embeds `UpstreamTarget::Local
//! (LocalHandler)`, root-crate-only vocabulary) from whatever
//! `ProxyResolution` these modules hand back.

pub mod dispatch;
pub mod groups;
pub mod options;
pub mod outcome;
pub mod peer_pick;
pub mod resolve;
pub mod retry;
pub mod routes_resolve;
pub mod state;
pub mod sticky;
