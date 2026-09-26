//! ACME (Let's Encrypt) auto-TLS facade.
//!
//! Extracted into `crates/conduit-acme` (issue #114/#130) — this file is a
//! thin re-export so `crate::server::acme::*` call sites
//! (`src/server/builder.rs`) don't need to change. Compiled only when the
//! `acme` feature is enabled — see the `#[cfg(feature = "acme")] pub mod
//! acme;` gate in `src/server/mod.rs`.
pub use conduit_acme::flow::{
    cert_expires_within_days, load_or_obtain_certificate, spawn_renewal_task, AcmeCertPaths,
};
