//! ACME (Let's Encrypt) auto-TLS crate for conduit's feature-driven Cargo
//! workspace migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#130](https://github.com/lopatnov/conduit/issues/130)).
//!
//! ## Scope
//!
//! Owns [`AcmeConfig`] (the `tls.acme` config struct), the ACME HTTP-01
//! certificate flow ([`flow`] — account management, order/challenge
//! negotiation, renewal), and the HTTP-01 challenge-response handler
//! ([`challenge`]). This crate is compiled into **every** conduit build —
//! like `conduit-core`/`conduit-config-core`/`conduit-otlp` — because
//! `TlsConfig.acme` is not itself feature-gated (a config file that sets
//! `tls.acme` without `--features acme` must still parse cleanly and get an
//! explicit `feature_warnings()` warning, not a silent-drop or a hard parse
//! error). Only the real ACME flow and challenge handler are gated behind
//! this crate's own `acme` Cargo feature; the root crate's `acme` feature
//! forwards into it via `lopatnov-conduit-acme/acme`.
//!
//! ## What deliberately stays in the root crate
//!
//! `AppState.acme_challenges: Arc<DashMap<String, String>>` (the shared
//! challenge-token store) stays in the root crate's `src/proxy/service.rs` —
//! it's a plain third-party `DashMap` type used unconditionally by
//! `RedirectProxy` (`src/server/redirect.rs`, always compiled) as well as by
//! this crate's own ACME flow, so there is nothing ACME-specific to extract
//! for it. Likewise, `obtain_acme_certs()` (`src/server/builder.rs`) and the
//! `HandlerKind::AcmeChallenge` dispatch arm (`src/proxy/request_phase.rs`)
//! stay in root — they need `AppConfig`/`RequestCtx`, which this crate has no
//! knowledge of (see `CONTRIBUTING.md`'s crate extraction recipe:
//! "conduit-core dependency is opt-in, not automatic" — here it *is* needed,
//! but only by [`challenge`], which implements `conduit_core::handler::LocalHandlerImpl`
//! and therefore needs `&mut Session`).

#[cfg(feature = "acme")]
pub mod challenge;
pub mod config;
#[cfg(feature = "acme")]
pub mod flow;

pub use config::AcmeConfig;
