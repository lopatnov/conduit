//! Consumer-model authentication crate for conduit's feature-driven Cargo
//! workspace migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#134](https://github.com/lopatnov/conduit/issues/134)).
//!
//! ## Scope
//!
//! Owns [`config::ConsumersConfig`] and its nested types (always compiled —
//! see below) and the real consumer-*identification* logic (`identify`
//! module, gated behind this crate's own `consumers` feature): matching an
//! incoming request against configured consumers by API key, Basic Auth,
//! per-consumer JWT (V2), or shared JWT (V3).
//!
//! `ConsumersConfig` is compiled into **every** conduit build — like
//! `AcmeConfig`/`OtlpConfig`/`TcpConfig`/`UploadConfig`/`FaultInjectionConfig`/
//! `JwtAuthConfig`/`ForwardAuthConfig` — because `SiteConfig.consumers` is
//! not itself feature-gated (a config file that sets `consumers` without
//! `--features consumers` must still parse cleanly and get an explicit
//! `feature_warnings()` warning, not a silent-drop or a hard parse error).
//!
//! ## Partial extraction — `ConsumersGuard` stays in the root crate
//!
//! Unlike every other #114 extraction so far (`conduit-faults` #132,
//! `conduit-auth-jwt` #133, `conduit-auth-forward` #134's sibling crate),
//! this crate does **not** own the `RequestFilter` guard. `ConsumersGuard`
//! (`src/filter/chain.rs` in the root crate) does three things: identify the
//! consumer, apply a per-consumer rate limit, inject headers. It's a
//! `Session`-coupled request-chain guard — same category as `IpGuard`/
//! `CorsPreflight`, which also stay in the root crate even though their
//! *config* types moved out (`conduit-ipfilter`/`conduit-cors`, #136): chain
//! assembly and guard ordering stay in the root crate per `CLAUDE.md`
//! decision #20, regardless of where the types a guard carries live. So
//! only the self-contained, side-effect-free *identification* step
//! ([`identify::identify_consumer`], which takes `&ConsumersConfig` and
//! `&Session` and touches no rate limiter) moves here. `ConsumersGuard`
//! itself keeps living in `src/filter/chain.rs`, now calling
//! [`identify::identify_consumer`] instead of its own private copy, and
//! (as of #114/#137 slice 1) `conduit_ratelimit::check_key_for`
//! for admission instead of a hand-rolled, uncapped bucket insertion.
//!
//! ## `Consumer::rate_limit`'s type
//!
//! `Consumer.rate_limit` re-exports [`config::RateLimitConfig`] from
//! `lopatnov-conduit-ratelimit` (issue #114/#137, slice 1) — the same type
//! the root crate's `crate::config::schema::RateLimitConfig` also
//! re-exports. Before #137 slice 1 this was a deliberately duplicated local
//! struct (a Layer-1 crate couldn't depend on a type living in the root
//! crate that depends on *it*, and the shared type hadn't been extracted
//! yet) — that duplication, and the SonarCloud "Duplicated Lines on New
//! Code" finding it caused, is resolved now that the type has its own
//! Layer-0 crate both sides depend on.

pub mod config;
#[cfg(feature = "consumers")]
pub mod identify;

pub use config::{
    Consumer, ConsumerBasicAuth, ConsumerJwtConfig, ConsumersConfig, ConsumersSharedJwtConfig,
    RateLimitConfig,
};
#[cfg(feature = "consumers")]
pub use identify::identify_consumer;
