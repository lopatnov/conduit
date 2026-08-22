//! Raw TCP passthrough proxy crate for conduit's feature-driven Cargo
//! workspace migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#131](https://github.com/lopatnov/conduit/issues/131)).
//!
//! ## Scope
//!
//! Owns [`TcpConfig`] (the `sites[].tcp` config struct) and the real
//! [`proxy::TcpProxy`] implementation (a Pingora `ServerApp` that relays
//! bytes bidirectionally between a client and a chosen upstream address).
//! `TcpConfig` is compiled into **every** conduit build — like
//! `AcmeConfig`/`OtlpConfig` — because `SiteConfig.tcp` is not itself
//! feature-gated (a config file that sets `tcp` without `--features tcp`
//! must still parse cleanly and get an explicit `feature_warnings()`
//! warning, not a silent-drop or a hard parse error). Only the real
//! [`proxy::TcpProxy`] is gated behind this crate's own `tcp` Cargo feature;
//! the root crate's `tcp` feature forwards into it via
//! `lopatnov-conduit-tcp/tcp`.
//!
//! `TcpProxy` implements Pingora's own `ServerApp` trait directly — not a
//! `conduit-core` chain trait (`RequestFilter`/`ResponseFilter`/
//! `LocalHandlerImpl`) — so per `CONTRIBUTING.md`'s crate extraction recipe
//! ("conduit-core dependency is opt-in, not automatic"), this crate has no
//! dependency on `lopatnov-conduit-core` at all.

pub mod config;
#[cfg(feature = "tcp")]
pub mod proxy;

pub use config::TcpConfig;
