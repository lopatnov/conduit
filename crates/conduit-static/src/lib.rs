//! Static-file serving + fallback-response crate for conduit's feature-driven
//! Cargo workspace migration (issue
//! [#114](https://github.com/lopatnov/conduit/issues/114), extracted in
//! [#139](https://github.com/lopatnov/conduit/issues/139)).
//!
//! ## Scope
//!
//! Owns [`StaticConfig`]/[`StaticOptions`] (the `sites[].static`/
//! `sites[].staticOptions` config) and [`FallbackConfig`]/[`FallbackRule`]
//! (the `sites[].fallback` config), together with the real serving logic:
//!
//! - [`handler`] — `StaticFileHandler`/`handle_static`: ETag/If-Modified-Since,
//!   byte-range serving, pre-compressed `.br`/`.gz` sibling serving, and
//!   on-the-fly streaming compression (moved from `src/handler/static_files.rs`).
//! - [`fallback`] — `FallbackHandler`/`handle_fallback`: 404/SPA-shell/
//!   custom-body responses with `byAccept` content negotiation (moved from
//!   `src/handler/fallback.rs`).
//! - [`roots`] — `resolve_static_roots`: resolves a site's `static` config
//!   (single path / multiple roots / path-prefix-mapped roots) against a
//!   request path (moved from `src/proxy/router.rs`).
//! - [`mime`] — `content_type`: extension-based Content-Type detection
//!   (moved from `src/util/mime.rs`, which itself re-exported
//!   `conduit_core::util::mime` — see "What moved out of `conduit-core`"
//!   below).
//!
//! `StaticConfig`/`StaticOptions`/`FallbackConfig`/`FallbackRule` are
//! compiled into **every** conduit build — like `FaultInjectionConfig`/
//! `JwtAuthConfig`/`CompressionConfig`/... — because `SiteConfig.static`/
//! `staticOptions`/`fallback` are not themselves feature-gated (a config
//! file that sets `static`/`fallback` without `--features static` must
//! still parse cleanly and get an explicit `feature_warnings()` warning,
//! not a silent-drop or a hard parse error). Only [`handler`], [`fallback`],
//! [`roots`], and [`mime`] are gated behind this crate's own `static` Cargo
//! feature; the root crate's `static` feature forwards into it via
//! `lopatnov-conduit-static/static`.
//!
//! ## Default-on (like `compression`, unlike every other extracted feature)
//!
//! `static` is the second extracted feature in the Conduit 2.0 migration
//! that stays **default-on** at the root crate — every other optional
//! feature besides `compression` defaults *off*. Issue #139 is explicit
//! about this: static-file serving and fallback (404/SPA-shell) responses
//! are baseline expectations for a reverse proxy/static-file server, so a
//! plain `cargo build` must keep serving them exactly like it did before
//! this extraction. Only `--no-default-features` (without re-adding
//! `static`) produces a build with no static-file/fallback capability at
//! all — see the root crate's `[features]` `default`/`static` entries.
//!
//! ## `compression` is a second, independent feature on this crate
//!
//! [`handler`]'s on-the-fly streaming compression (`stream_file_compressed`,
//! the chunk-by-chunk brotli/gzip/deflate encoder pipeline) moved here
//! together with the rest of `static_files.rs` — unlike `conduit-compression`
//! (#138), which only owns the negotiation logic, not the streaming
//! encoders. It stays gated behind this crate's own `compression` feature,
//! independent of `static`: pre-compressed `.br`/`.gz` sibling serving has
//! no dependency on it at all, and plain uncompressed serving works with
//! `static` alone. The root crate's `compression` feature forwards into
//! both `lopatnov-conduit-compression/compression` (negotiation types/logic)
//! and `lopatnov-conduit-static/compression` (the encoders used here).
//!
//! ## What moved out of `conduit-core`
//!
//! `conduit-core`'s own `util::mime` module (added during the Layer-0
//! extraction, #126) existed solely to back `src/util/mime.rs`'s facade,
//! whose only caller was `static_files.rs` — now [`mime`] in this crate.
//! Since nothing else in the workspace referenced `conduit_core::util::mime`,
//! it was removed from `conduit-core` entirely (along with `conduit-core`'s
//! own unconditional `mime_guess` dependency) rather than left behind as
//! dead weight — otherwise `mime_guess` would stay in the dependency tree
//! under `--no-default-features` regardless of this crate's own `static`
//! feature gate, defeating the point of gating it at all. A deliberate,
//! narrow API break in `conduit-core` (see `CONTRIBUTING.md`'s crate
//! extraction recipe: these member crates are internal plumbing, not a
//! semver-disciplined public API — `CLAUDE.md` decision #32).

pub mod config;
#[cfg(feature = "static")]
pub mod fallback;
#[cfg(feature = "static")]
pub mod handler;
#[cfg(feature = "static")]
pub mod mime;
#[cfg(feature = "static")]
pub mod roots;

pub use config::{FallbackConfig, FallbackRule, StaticConfig, StaticOptions};
