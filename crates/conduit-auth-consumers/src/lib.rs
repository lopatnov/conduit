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
//! consumer, apply a per-consumer rate limit, inject headers. The middle
//! step needs the root crate's `RateLimiter`/`TokenBucket`
//! (`src/filter/rate_limit.rs`) — itself not yet extracted (that's
//! [#137](https://github.com/lopatnov/conduit/issues/137),
//! `conduit-ratelimit`). A Layer-1 feature crate depending on a
//! not-yet-extracted piece of the root crate would be exactly the
//! premature/circular coupling the workspace split exists to avoid — so
//! only the self-contained, side-effect-free *identification* step
//! ([`identify::identify_consumer`], which takes `&ConsumersConfig` and
//! `&Session` and touches no rate limiter) moves here. `ConsumersGuard`
//! itself keeps living in `src/filter/chain.rs`, now calling
//! [`identify::identify_consumer`] instead of its own private copy.
//!
//! ## `Consumer::rate_limit`'s type (see `config::RateLimitConfig`'s own doc
//! comment for the full reasoning)
//!
//! `Consumer.rate_limit` uses a small, deliberately duplicated local
//! [`config::RateLimitConfig`] rather than the root crate's own
//! `crate::config::schema::RateLimitConfig` — the same "can't depend
//! backwards into the root crate, and the shared type hasn't been
//! extracted yet" constraint as the guard split above, applied to a plain
//! data struct instead of behavior. Tracked for consolidation once #137
//! lands.

pub mod config;
#[cfg(feature = "consumers")]
pub mod identify;

pub use config::{
    Consumer, ConsumerBasicAuth, ConsumerJwtConfig, ConsumersConfig, ConsumersSharedJwtConfig,
    RateLimitConfig,
};
#[cfg(feature = "consumers")]
pub use identify::identify_consumer;
