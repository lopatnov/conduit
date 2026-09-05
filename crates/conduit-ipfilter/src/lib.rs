//! IP allow/deny filtering crate for conduit's feature-driven Cargo
//! workspace migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#136](https://github.com/lopatnov/conduit/issues/136)).
//!
//! ## Scope
//!
//! Owns [`IpFilterConfig`] (the `sites[].ipFilter` config struct), the pure
//! CIDR/exact-match filtering logic (`ip_filter` module — allow/deny rule
//! matching, `X-Forwarded-For` parsing with rightmost-entry trust, dynamic
//! deny-list lookup), and the real `guard::IpGuard` request filter — a chain
//! guard that rejects requests whose client IP fails the allow/deny rules
//! (including the runtime deny-list managed via Admin API `POST /ip-deny`).
//!
//! ## Always-on, no Cargo feature (unlike most other #114 extractions)
//!
//! Unlike `conduit-faults`/`conduit-auth-jwt`/`conduit-acme`/etc., ipFilter
//! is **not** gated behind any Cargo feature at all — see `CLAUDE.md`
//! architectural decision #31 (2026-08-23): `ipFilter`/`cors`/
//! `securityHeaders` stay always-on/default-on because gating them buys
//! almost no binary-size benefit (this is light logic with no heavy
//! third-party dependency) while adding a real "forgot the flag, silently
//! stopped filtering" risk for a security-relevant guard. Concretely: this
//! crate has **no `[features]` table**, and every dependency below —
//! including `lopatnov-conduit-core` for `guard::IpGuard` — is a mandatory,
//! non-optional dependency. This mirrors `conduit-config-core`'s (#127)
//! unconditional dependency style rather than the "config always-on, guard
//! feature-gated" split used by the config-carrying extractions above.
//!
//! `guard::IpGuard` implements `conduit-core`'s
//! [`RequestFilter`](conduit_core::filter::chain::RequestFilter) chain trait
//! directly, so per `CONTRIBUTING.md`'s crate extraction recipe ("conduit-core
//! dependency is opt-in, not automatic") this crate depends on
//! `lopatnov-conduit-core` — unconditionally here, since there's no feature
//! to gate it behind (every prior guard-shaped extraction gated this same
//! dependency behind its own feature; this is the first that can't). Chain
//! assembly and guard ordering stay in the root crate's
//! `src/filter/chain.rs` (`CLAUDE.md` decision #20) — this crate exports
//! only the filter implementation and its constructor input
//! ([`IpFilterConfig`]), never a chain position.

pub mod config;
pub mod guard;
pub mod ip_filter;

pub use config::IpFilterConfig;
