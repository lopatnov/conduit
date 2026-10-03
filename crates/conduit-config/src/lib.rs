//! Layer-2 config schema for conduit's feature-driven Cargo workspace (issue
//! [#114](https://github.com/lopatnov/conduit/issues/114), extracted in
//! [#222](https://github.com/lopatnov/conduit/issues/222)).
//!
//! - [`schema`] — `AppConfig`, `SiteConfig` and every type they contain: the root-owned ones
//!   (`GlobalConfig`, `TlsConfig`, `LoggingConfig`, ...) are defined here, the rest are re-exported
//!   from the Layer-1 crate that owns the feature, so there is one place to name every config type.
//!   Field order within a struct is the serialisation key order and variant order within a
//!   `#[serde(untagged)]` enum is the order serde tries them in (decisions #4, #6, #7 in `CLAUDE.md`),
//!   so neither may change.
//! - [`parse`] — `load_config`/`from_str`/`from_yaml`/`normalize`: `ConfigFile` (`Full` → `Sites` →
//!   `Single`) parsed through `conduit-config-core`'s generic loader and normalised into an `AppConfig`.
//!
//! No item is gated by a Cargo feature: relocating the schema must not change what parses (gating fields
//! per feature is a behaviour change and a separate step). What stays in the root crate: `validate`
//! (`validate()`/`feature_warnings()` and the cross-site checks), the file/Kubernetes providers, the
//! per-feature defaults in `defaults.rs`, and `rate_limit_scan`.

pub mod parse;
pub mod schema;
