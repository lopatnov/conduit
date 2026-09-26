//! Proxy target resolution (routing, load-balancing dispatch, retry, sticky
//! sessions) for conduit's feature-driven Cargo workspace migration (issue
//! [#114](https://github.com/lopatnov/conduit/issues/114), extracted in
//! [#143](https://github.com/lopatnov/conduit/issues/143) — PR B of a 3-PR
//! sequence, following PR A1 (issue #418, grouped `RequestCtx`'s proxy
//! fields into [`state::ProxyReqState`]) and PR A2 (issue #419, phase-split
//! `router.rs`/`routes.rs` and introduced the [`outcome`] boundary types).
//!
//! ## Scope
//!
//! Owns everything moved from the root crate's `src/proxy/routing/*.rs`
//! (PR A1/A2), plus `src/proxy/{capacity,slow_start}.rs`, plus the `routes[]`
//! array matching logic that used to live in the root crate's
//! `src/proxy/routes.rs`, plus 10 proxy-related config types out of
//! `src/config/schema.rs` (`config`), plus 3 of the 4 upstream-target-list
//! helpers that stayed behind in the root crate's `src/proxy/upstream.rs`
//! since #142 specifically because moving them required `ProxyRouteTarget`/
//! `ProxyConfig` to move too (`targets`) — that circular-dependency deferral
//! is exactly what this PR resolves. See `crates/conduit-upstream/src/
//! lib.rs`'s own doc comment for the "not extracted yet" note this PR makes
//! stale.
//!
//! - [`config`] — `ProxyConfig`/`ProxyRouteTarget`/`ProxyRouteConfig`/
//!   `StickyConfig`/`RewriteRule`/`ProxyTimeout`/`ConnectionPoolConfig`/
//!   `RetryConfig`/`RouteConfig`/`MatchConfig`.
//! - [`state`] — `ProxyReqState`/`RetryState`/`RouteRateLimit` (PR A1).
//! - [`outcome`] — `ProxyOutcome`/`ProxyResolution`/`ProxyUpstream`, the
//!   narrower return type replacing `RouteResolution` for every function in
//!   this crate (PR A2) — see its own doc comment for why. Also owns a
//!   crate-private `url_to_proxy_upstream` helper with the same URL-parsing
//!   body as the root crate's own `router::url_to_proxy_upstream` but
//!   returning `ProxyUpstream` directly instead of `UpstreamTarget` — see
//!   this module's own doc comment for why the small duplication is
//!   deliberate.
//! - [`options`] — `RouteOptions`/`ProxyCtx` config-extraction types.
//! - [`resolve`] — the `proxy` map (`site.proxy` as a `Routes` map) target
//!   resolution orchestrator + helpers.
//! - `peer_pick` — candidate-pool building + peer/retry selection for
//!   `resolve`'s orchestrator, split into its own file purely to keep
//!   `resolve.rs` under the 400-production-line soft limit.
//! - `groups` — two-level (grouped) upstream routing.
//! - [`sticky`] — sticky-session resolution + HMAC helpers.
//! - `retry` — retry-state construction.
//! - `routes_resolve` — `routes[]` array target resolution (`routes`'s own
//!   equivalent of `resolve`, deliberately not sharing helpers with it — see
//!   its own doc comment).
//! - [`routes`] — `routes[]` array *matching* (path glob/method/header/
//!   query/cookie predicates), moved here from the root crate's own
//!   `src/proxy/routes.rs` because [`config::RouteConfig`]/
//!   [`config::MatchConfig`] moved here in this same PR.
//! - `capacity`/`slow_start` — per-upstream connection-capacity admission
//!   (issue #156) and the slow-start traffic ramp (issue #157). Private
//!   modules: nothing outside the routing/resolution code above ever called
//!   either directly (confirmed by grep before this move — the one
//!   root-crate mention was a doc-comment cross-reference, not a real
//!   dependency), so there is no reason for either to be part of this
//!   crate's public API.
//!
//! ## `dispatch.rs` deliberately did NOT move here
//!
//! PR A2's `src/proxy/routing/dispatch.rs` bundled two genuinely unrelated
//! concerns into one file purely to keep it out of `router.rs`/`resolve.rs`
//! (its own doc comment already called this out: "orthogonal to
//! proxy-target resolution... so they get their own file"):
//! `dispatch::parse_rfc9218_priority` (pure string parsing, zero config-type
//! dependency) and site/local-path dispatch helpers (`find_site_idx`,
//! `is_health_path`, `metrics_token`, `is_hot_reload_js_path`,
//! `is_hot_reload_sse_path`) that all take `&AppConfig`/`Option<&SiteConfig>`
//! directly. `AppConfig`/`SiteConfig` are still root-crate-only types (a
//! separate, not-yet-started config-schema-decomposition track — issues
//! #314/#315/#316/#222 — not this PR's scope), and `is_health_path` also
//! needs `HealthCheckConfig`, itself not extracted anywhere yet. Moving
//! either half into this crate would have created a genuine circular
//! dependency (this crate needing types defined in the root crate, which
//! depends on this crate) — the exact same class of problem
//! `conduit-upstream`'s own lib.rs doc comment already describes for
//! `ProxyRouteConfig`. Grepped every call site before deciding: none of the
//! files that DID move into this crate ever called any `dispatch::`
//! function — only `router.rs` did, and it still does, from
//! `src/proxy/dispatch.rs` (relocated within the root crate, content
//! unchanged, `parse_rfc9218_priority` included).

// ── Always compiled ──────────────────────────────────────────────────────────
// Config types, per-request state, boundary types, `routes[]` *matching* and
// the target-URL-list helpers must exist in every build, `proxy` or not: the
// root crate's `SiteConfig` embeds the config types, `feature_warnings()` and
// `validate()` walk them, and `routes[].static` needs `RouteConfig`/
// `MatchConfig` plus the matcher (issue #144).
pub mod config;
pub mod options;
pub mod outcome;
pub mod routes;
pub mod state;
pub mod targets;
pub mod validate;
pub mod warnings;

// ── Gated behind `proxy` (issue #144) ────────────────────────────────────────
// The proxy-target *resolution* engine. With the feature off, `routes::
// match_routes` reports a matched route that has a `proxy` action as
// `ProxyOutcome::Unresolved` (see its own doc comment) instead of calling
// into any of this.
#[cfg(feature = "proxy")]
mod capacity;
#[cfg(feature = "proxy")]
mod groups;
#[cfg(feature = "proxy")]
mod peer_pick;
#[cfg(feature = "proxy")]
pub mod resolve;
#[cfg(feature = "proxy")]
mod retry;
#[cfg(feature = "proxy")]
mod routes_resolve;
#[cfg(feature = "proxy")]
mod slow_start;
#[cfg(feature = "proxy")]
pub mod sticky;
