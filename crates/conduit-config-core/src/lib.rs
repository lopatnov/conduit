//! Layer-0 crate for conduit's feature-driven Cargo workspace migration
//! (issue [#114](https://github.com/lopatnov/conduit/issues/114), extracted
//! in [#127](https://github.com/lopatnov/conduit/issues/127)).
//!
//! ## Invariant: zero schema knowledge
//!
//! This crate owns the config-loading *mechanism* — env interpolation,
//! version probing, JSON/YAML deserialization, validation-error shape, and
//! the file-watching `Provider`/`FileProvider` abstraction — generic over
//! the config payload type `C`/`T`. It must never name a concrete schema
//! type (`AppConfig`, `SiteConfig`, `ConfigFile`) or a
//! `#[cfg(feature = "…")]`. Unlike `conduit-core` (which depends on
//! `pingora` + `std`), this crate's dependency profile is the serde stack
//! plus `notify`/`tokio` — still always compiled in, since every build
//! needs to load a config file.
//!
//! `ConfigFile`/`normalize()` deliberately stay in the root crate: their
//! 3-variant untagged shape is a *schema* decision (`CLAUDE.md`
//! Архитектурное решение #4), not a parsing mechanism, and belongs with
//! `AppConfig`/`SiteConfig` when those move in Phase 3.

pub mod env;
pub mod format;
pub mod parse;
pub mod provider;
pub mod validation;
