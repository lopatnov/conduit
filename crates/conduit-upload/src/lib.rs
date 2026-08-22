//! File-upload handler crate for conduit's feature-driven Cargo workspace
//! migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#131](https://github.com/lopatnov/conduit/issues/131)).
//!
//! ## Scope
//!
//! Owns [`UploadConfig`] (the `sites[].upload` config struct) and the real
//! Axum-based upload server ([`server`] — `UploadService`,
//! `make_upload_router`, `run_upload_server`, `upload_handler`).
//! `UploadConfig` is compiled into **every** conduit build — like
//! `AcmeConfig`/`OtlpConfig`/`TcpConfig` — because `SiteConfig.upload` is not
//! itself feature-gated (a config file that sets `upload` without
//! `--features upload` must still parse cleanly and get an explicit
//! `feature_warnings()` warning, not a silent-drop or a hard parse error).
//! Only the real server is gated behind this crate's own `upload` Cargo
//! feature; the root crate's `upload` feature forwards into it via
//! `lopatnov-conduit-upload/upload`.
//!
//! `UploadService` implements Pingora's own `BackgroundService` trait
//! directly — not a `conduit-core` chain trait (`RequestFilter`/
//! `ResponseFilter`/`LocalHandlerImpl`) — so per `CONTRIBUTING.md`'s crate
//! extraction recipe ("conduit-core dependency is opt-in, not automatic"),
//! this crate has no dependency on `lopatnov-conduit-core` at all.
//!
//! ## Decoupling from the root crate's `AppState`
//!
//! The pre-extraction code took/held `Arc<AppState>` — the root crate's big
//! application-state struct — which would create a circular dependency if
//! moved here verbatim. Instead, [`server::UploadConfigSource`] captures the
//! *one* thing this crate actually needs (looking up the active
//! [`UploadConfig`] for a given site index), and [`server::UploadService`],
//! [`server::make_upload_router`], and [`server::run_upload_server`] are all
//! generic over it. The root crate implements the trait for its own
//! `AppState` and re-exports a concrete `UploadService` type alias bound to
//! it — see `CONTRIBUTING.md`'s crate-extraction recipe,
//! "Generic-in-crate, bound-by-type-alias-in-root".

pub mod config;
#[cfg(feature = "upload")]
pub mod server;

pub use config::UploadConfig;
