//! CORS (Cross-Origin Resource Sharing) crate for conduit's feature-driven
//! Cargo workspace migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#136](https://github.com/lopatnov/conduit/issues/136)).
//!
//! ## Scope
//!
//! Owns [`CorsConfig`]/[`CorsOptions`] (the `sites[].cors` config struct),
//! the pure CORS-header logic (`cors` module — preflight detection, Private
//! Network Access opt-in, response-header construction), and the real
//! `guard::CorsPreflight` request filter — a chain guard that answers CORS
//! preflight (`OPTIONS`) requests directly, before the request ever reaches
//! auth or the upstream proxy.
//!
//! ## Always-on, no Cargo feature (unlike most other #114 extractions)
//!
//! Like `conduit-ipfilter` (its sibling in this same extraction, #136), CORS
//! is **not** gated behind any Cargo feature — see `CLAUDE.md` architectural
//! decision #31 (2026-08-23): `ipFilter`/`cors`/`securityHeaders` stay
//! always-on/default-on because gating them buys almost no binary-size
//! benefit while adding a real "forgot the flag" risk. This crate has **no
//! `[features]` table**, and every dependency below — including
//! `lopatnov-conduit-core` for `guard::CorsPreflight` — is a mandatory,
//! non-optional dependency, mirroring `conduit-config-core`'s (#127)
//! unconditional dependency style.
//!
//! `guard::CorsPreflight` implements `conduit-core`'s
//! [`RequestFilter`](conduit_core::filter::chain::RequestFilter) chain trait
//! directly, so per `CONTRIBUTING.md`'s crate extraction recipe this crate
//! depends on `lopatnov-conduit-core`. Chain assembly and guard ordering
//! stay in the root crate's `src/filter/chain.rs` (`CLAUDE.md` decision
//! #20) — this crate exports only the filter implementation and its
//! constructor inputs ([`CorsConfig`]/[`CorsOptions`]), never a chain
//! position.
//!
//! Non-preflight CORS response headers (`cors::response_headers`) are
//! applied later, in the root crate's response-header assembly
//! (`src/proxy/request_phase.rs`) — not by this crate's guard, which only
//! handles the `OPTIONS` preflight itself.

pub mod config;
pub mod cors;
pub mod guard;

pub use config::{CorsConfig, CorsOptions};
