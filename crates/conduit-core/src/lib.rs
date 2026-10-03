//! Layer-0 crate for conduit's feature-driven Cargo workspace migration
//! (issue [#114](https://github.com/lopatnov/conduit/issues/114), extracted
//! in [#126](https://github.com/lopatnov/conduit/issues/126)).
//!
//! ## Invariant: zero config knowledge
//!
//! Every type and function in this crate must compile without knowing
//! anything about conduit's config schema (`AppConfig`, `SiteConfig`, or any
//! per-feature config struct) or about any Cargo feature flag. This crate is
//! compiled into **every** conduit build, no matter which features are
//! enabled — it is the honest floor the whole workspace migration exists to
//! keep as small as possible.
//!
//! Concretely: this crate owns the abstract *vocabulary* (traits, outcome
//! enums, narrow context types) that Layer-1+ crates implement against. It
//! does **not** own concrete filter/handler implementations, chain assembly
//! that reads config (`ResponseFilterChain::build`, `FilterChain`), or
//! anything that names a config-schema type. Adding a `use` of
//! `crate::config::…` (or an equivalent from a dependent crate) or a
//! `#[cfg(feature = "…")]` anywhere in this crate is a review-blocker.
//!
//! See `src/filter/chain.rs` / `src/filter/response_chain.rs` in the root
//! crate for where the config-aware chain assembly and concrete guards
//! actually live — they `pub use` the vocabulary from here.

pub mod filter;
pub mod handler;
pub mod util;
