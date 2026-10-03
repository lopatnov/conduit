//! ACME HTTP-01 challenge-response handler facade.
//!
//! Extracted into `crates/conduit-acme` (issue #114/#130) — this file is a
//! thin re-export so `crate::handler::acme_challenge::*` call sites
//! (`src/proxy/request_phase.rs`) don't need to change. Compiled only when
//! the `acme` feature is enabled — see the `#[cfg(feature = "acme")] pub mod
//! acme_challenge;` gate in `src/handler/mod.rs`.
pub use conduit_acme::challenge::{handle_acme_challenge, AcmeChallengeHandler};
