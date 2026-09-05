//! Security-headers crate for conduit's feature-driven Cargo workspace
//! migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#136](https://github.com/lopatnov/conduit/issues/136)).
//!
//! ## Scope
//!
//! Owns [`SecurityHeadersConfig`]/[`SecurityHeadersOptions`] (the
//! `sites[].securityHeaders` config struct), the pure header-construction
//! and Host-allowlist logic (`security_headers` module —
//! `header_entries`/`is_host_allowed`), and the real
//! `guard::AllowedHostsGuard` request filter — a chain guard that rejects
//! requests whose `Host` header doesn't match `securityHeaders.allowedHosts`
//! (or, as a default-safe fallback, the site's own `host:` value).
//!
//! ## Always-on, no Cargo feature (unlike most other #114 extractions)
//!
//! Like `conduit-ipfilter`/`conduit-cors` (its siblings in this same
//! extraction, #136), security headers are **not** gated behind any Cargo
//! feature — see `CLAUDE.md` architectural decision #31 (2026-08-23):
//! `ipFilter`/`cors`/`securityHeaders` stay always-on/default-on because
//! gating them buys almost no binary-size benefit while adding a real
//! "forgot the flag" risk for a security-relevant guard. This crate has
//! **no `[features]` table**, and every dependency below — including
//! `lopatnov-conduit-core` for `guard::AllowedHostsGuard` — is a mandatory,
//! non-optional dependency, mirroring `conduit-config-core`'s (#127)
//! unconditional dependency style.
//!
//! `guard::AllowedHostsGuard` implements `conduit-core`'s
//! [`RequestFilter`](conduit_core::filter::chain::RequestFilter) chain trait
//! directly, so per `CONTRIBUTING.md`'s crate extraction recipe this crate
//! depends on `lopatnov-conduit-core`. Chain assembly and guard ordering
//! stay in the root crate's `src/filter/chain.rs` (`CLAUDE.md` decision
//! #20) — this crate exports only the filter implementation and its
//! constructor inputs ([`SecurityHeadersConfig`]/[`SecurityHeadersOptions`]),
//! never a chain position.
//!
//! Non-Host-validation security response headers
//! (`security_headers::header_entries`) are applied later, in the root
//! crate's response-header assembly (`src/proxy/request_phase.rs`) — not by
//! this crate's guard, which only handles the `Host` allowlist check.

pub mod config;
pub mod guard;
pub mod security_headers;

pub use config::{SecurityHeadersConfig, SecurityHeadersOptions};
