//! Middleware chain crate for conduit's feature-driven Cargo workspace
//! migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#141](https://github.com/lopatnov/conduit/issues/141)).
//!
//! Owns [`MiddlewareEntry`] (the `sites[].middleware[]` config struct,
//! `src/config.rs`), the real `guard::MiddlewareGuard` (request-phase
//! Rhai/WASM dispatcher, `src/guard.rs`), and the real
//! `response::MiddlewareResponseFilter` (response-phase Rhai/WASM dispatcher,
//! `src/response.rs`). `MiddlewareEntry` is compiled into **every** conduit
//! build — like `AcmeConfig`/`OtlpConfig`/`FaultInjectionConfig` — because
//! `SiteConfig.middleware` is not itself feature-gated (a config file that
//! sets `middleware` without `--features rhai`/`wasm` must still parse
//! cleanly and get an explicit `feature_warnings()` warning for the `wasm`
//! entry type, not a silent-drop or a hard parse error; unknown `r#type`
//! values are rejected at config-validation time regardless).
//!
//! ## No `MiddlewarePlugin` trait/registry (deliberate, #114/#141/#392)
//!
//! `guard::MiddlewareGuard::apply` and `response::MiddlewareResponseFilter::apply`
//! both dispatch on `entry.r#type` with a closed `match` — exactly the
//! pre-extraction shape, moved verbatim. Issue #141's own original body
//! described a `MiddlewarePlugin` trait that was never actually built; that
//! description was corrected on the issue itself during this extraction's
//! planning, with issue #392 tracking whether a real plugin-registry
//! abstraction is worth adding later as a separate, deliberate design
//! question — not something to introduce as a side effect of a mechanical
//! crate move. Reasons a `match` is still the right call today: Rhai and WASM
//! aren't interface-compatible (different host APIs, different failure
//! modes), there's no third backend on the horizon to justify the
//! indirection, and no other extraction in this migration has introduced a
//! new abstraction mid-move.
//!
//! ## Depends on two sibling crates, both optional (#114/#141)
//!
//! Unlike most single-feature extractions, this crate's own `rhai`/`wasm`
//! Cargo features gate two *other* new crates as optional path-dependencies:
//!
//! - `rhai` → [`lopatnov-conduit-script-rhai`](../conduit_script_rhai/index.html)
//!   (`run_script`/`run_script_response`)
//! - `wasm` → [`lopatnov-conduit-plugin-wasm`](../conduit_plugin_wasm/index.html)
//!   (`run_wasm`/`run_wasm_response`), plus `base64` for the WASM
//!   response-body-override header hack in `response::apply_response_mutations`
//!   (a real, separately-filed pre-existing bug — issue #391 — moved here
//!   verbatim, not fixed as part of this extraction)
//!
//! The root crate's own `rhai`/`wasm` features simply forward into this
//! crate's features (`lopatnov-conduit-middleware/rhai`,
//! `lopatnov-conduit-middleware/wasm`) — this is the only crate that calls
//! into either sibling, so neither is a *direct* root-crate dependency
//! anymore.
//!
//! `tracing`/`serde_json`/`bytes`/`tokio` are mandatory dependencies, not
//! gated behind `any(rhai, wasm)`: the `#[cfg(not(feature = "wasm"))]
//! "wasm" => tracing::warn!(...)` arm in `guard::MiddlewareGuard::apply` is
//! live precisely when `wasm` is OFF (gating `tracing` behind `wasm` would
//! break the no-feature build); `MiddlewareEntry.config: Option<serde_json::
//! Value>` is always compiled (same pattern as `FallbackConfig.body`); and
//! `bytes`/`tokio` are already unconditionally in every build's dependency
//! tree via pingora/tokio, so gating them here buys nothing.
//!
//! ## Coupling check (#114/#141)
//!
//! None of `MiddlewareGuard`, `MiddlewareResponseFilter`, or either sibling
//! crate touch the root crate's `RequestCtx`/`SiteConfig`/`AppConfig`
//! directly — `MiddlewareGuard`'s fields are primitives (a `Vec<MiddlewareEntry>`
//! together with some `String`s and a `HashMap`), and `apply` only uses
//! `conduit-core`'s `FilterContext` and `handler::response::write_response`
//! (both Layer-0).
//! `MiddlewareResponseFilter::apply` binds its `req_ctx: &dyn ResponseCtx`
//! parameter as `_req_ctx` and never reads it — this crate's own test module
//! uses a local zero-field `DummyCtx` stub instead of the root crate's
//! `RequestCtx` (which isn't reachable from here at all).

pub mod config;
pub mod guard;
pub mod response;

pub use config::MiddlewareEntry;
