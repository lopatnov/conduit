//! Configured URL redirects crate for conduit's feature-driven Cargo
//! workspace migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#140](https://github.com/lopatnov/conduit/issues/140)).
//!
//! ## Scope
//!
//! Owns [`RedirectRule`] (the `sites[].redirects[]` config struct — `from`/
//! `to`/`status`), the pure matching logic (`redirects` module —
//! `apply_redirects`, moved from `src/filter/redirects.rs`), and the real
//! `guard::RedirectGuard` chain guard — answers a request with a 3xx
//! redirect when a configured rule matches, before the request ever reaches
//! auth or the upstream proxy.
//!
//! ## Always-on, no Cargo feature (like `conduit-cors`/`conduit-ipfilter`/
//! `conduit-security-headers`/`conduit-metrics`)
//!
//! Per `CLAUDE.md` architectural decision #31 (2026-08-23), `redirects` (like
//! its siblings above) stays always-on/default-on — it's light logic with no
//! heavy third-party dependency, so gating it behind a Cargo feature would
//! buy almost no binary-size benefit while adding a real "forgot the flag,
//! redirects silently stopped firing" risk. This crate has **no `[features]`
//! table**, and every dependency — including `lopatnov-conduit-core` for
//! `guard::RedirectGuard` — is a mandatory, non-optional dependency,
//! mirroring `conduit-config-core`'s (#127) unconditional dependency style.
//!
//! `guard::RedirectGuard` implements `conduit-core`'s
//! [`RequestFilter`](conduit_core::filter::chain::RequestFilter) chain trait
//! directly, so per `CONTRIBUTING.md`'s crate extraction recipe this crate
//! depends on `lopatnov-conduit-core`. Chain assembly and guard ordering
//! stay in the root crate's `src/filter/chain.rs` (`CLAUDE.md` decision
//! #20) — this crate exports only the filter implementation and its
//! constructor input ([`RedirectRule`]/`redirects::apply_redirects`), never a
//! chain position. The root crate resolves `apply_redirects(rules, path)`
//! into `RedirectGuard { result }` once, post-routing, and pushes the guard
//! into the chain (see `src/proxy/request_phase.rs`).

pub mod config;
pub mod guard;
pub mod redirects;

pub use config::RedirectRule;
