//! Browser hot-reload crate for conduit's feature-driven Cargo workspace
//! migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#140](https://github.com/lopatnov/conduit/issues/140)).
//!
//! ## Scope
//!
//! Owns [`HotReloadConfig`]/[`HotReloadOptions`] (the `sites[].hotReload`
//! config) and the real hot-reload support:
//!
//! - [`handler`] — `HotReloadJsHandler`/`HotReloadSseHandler`: serves
//!   `/__hot-reload__/client.js` (the browser-side reconnecting `EventSource`
//!   snippet) and `/__hot-reload__` (the Server-Sent Events stream itself),
//!   moved from `src/handler/hot_reload.rs`.
//! - [`watcher`] — `build_watch_config`/`run_file_watcher`: the `notify`-backed
//!   filesystem watcher that debounces file-change events and broadcasts a
//!   reload signal to every connected SSE stream, moved from the same file.
//!
//! `HotReloadConfig`/`HotReloadOptions` are compiled into **every** conduit
//! build — like `FaultInjectionConfig`/`CompressionConfig`/`StaticConfig`/... —
//! because `SiteConfig.hotReload` is not itself feature-gated (a config file
//! that sets `hotReload` without `--features hotreload` must still parse
//! cleanly and get an explicit `feature_warnings()` warning, not a
//! silent-drop or a hard parse error). Only [`handler`] and [`watcher`] are
//! gated behind this crate's own `hotreload` Cargo feature; the root crate's
//! `hotreload` feature forwards into it via
//! `lopatnov-conduit-hotreload/hotreload`.
//!
//! ## Default-on, and genuinely gated (unlike `ipfilter`/`cors`/...)
//!
//! `hotreload` is the third extracted feature in the Conduit 2.0 migration
//! that stays **default-on** at the root crate (after `compression` #138 and
//! `static` #139) — a plain `cargo build` must keep serving
//! `/__hot-reload__`/`/__hot-reload__/client.js` and watching configured
//! directories exactly like it did before this extraction. Unlike
//! `ipFilter`/`cors`/`securityHeaders`/`redirects`/`metrics` (`CLAUDE.md`
//! decision #31, always-on with no Cargo feature at all — light logic, no
//! heavy third-party dependency), `hotreload` is one of the two features
//! decision #31 explicitly calls out as *genuinely* worth gating for real:
//! it pulls in `notify` and its platform-specific filesystem-watcher
//! backend. `--no-default-features` (without re-adding `hotreload`) drops
//! this crate's own real logic. **`notify` itself does not actually leave
//! the dependency tree even then** — `conduit-config-core`'s unrelated,
//! always-on config-file-reload watcher (`FileProvider`'s auto-reload mode,
//! pre-existing since #127) has its own unconditional `notify` dependency.
//! Gating this crate's own copy is still correct for feature-correctness
//! (verified by `cargo hack --each-feature`) and matches every other
//! extraction's recipe, just isn't a source of `notify` footprint savings
//! by itself given the other, unrelated always-on user.
//!
//! ## `AppConfig`/`SiteConfig` aren't available here
//!
//! `watcher::build_watch_config` needs each site's `hotReload` **and**
//! `static` config to decide which directories to watch — but unlike
//! `resolve_static_roots` (`conduit-static`, #139), which only ever needed a
//! single already-resolved `StaticConfig`, the pre-extraction
//! `build_watch_config` iterated `AppConfig.sites` directly. `AppConfig`/
//! `SiteConfig` themselves are still root-crate-only types (a later
//! migration phase), so this crate's version takes an iterator of
//! `(Option<&HotReloadConfig>, Option<&conduit_static::StaticConfig>)` pairs
//! instead — one per site — with the root crate's own call site
//! (`admin/api.rs`) mapping `config.sites` into that shape. All the
//! aggregation/dedup logic itself (the actual thing worth moving) stays
//! here, unchanged.

pub mod config;
#[cfg(feature = "hotreload")]
pub mod handler;
#[cfg(feature = "hotreload")]
pub mod watcher;

pub use config::{HotReloadConfig, HotReloadOptions};
