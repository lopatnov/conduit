//! Layer-3 request pipeline for conduit's feature-driven Cargo workspace (issue
//! [#114](https://github.com/lopatnov/conduit/issues/114), extracted in
//! [#145](https://github.com/lopatnov/conduit/issues/145)).
//!
//! What runs for every request: the Pingora `ProxyHttp` implementation (`proxy::service`, whose methods are
//! one-line delegators into the phase modules), the per-request state (`proxy::ctx`), the request/response/
//! logging phases (`proxy::request`, `response_phase`, `logging_phase`), the guard chain and the response
//! chain (`filter::chain`, `filter::response_chain`), the access log (`filter::logging`) and the local
//! handlers. `AppState` — what the admin API, the server bootstrap and hot reload read and swap — lives here
//! too, and so does its `UploadConfigSource` impl (the orphan rule: a trait impl must sit in the crate that
//! owns the type).
//!
//! The root crate keeps admin, server, CLI, `config::validate` and the providers, and re-exports every item of
//! this crate at its original path (`crate::proxy::service::AppState` and so on), so no call site changed.
//!
//! ## Layout
//!
//! The module tree mirrors the root's on purpose (`proxy/`, `filter/`, `handler/`, `upload/`): the moved files
//! are byte-identical to what they were, and the small alias modules below (`config`, `util`, and the
//! re-exports in each `mod.rs`) make their `crate::…` paths resolve here exactly as they did there. Test names
//! are unchanged too, so a before/after `cargo test -- --list` is a plain union.
//!
//! ## Features
//!
//! Fourteen root features gate code in this crate; each one is declared here with the same forwards as in the
//! root, and [`features`] exposes them as constants so the root can assert at compile time that the two agree
//! (a root feature that is on while this crate's is off would compile and quietly drop a guard).

pub mod filter;
pub mod handler;
pub mod proxy;
#[cfg(feature = "upload")]
pub mod upload;

/// This crate's own view of the 14 features it hosts, for the root's parity asserts.
pub mod features {
    pub const PROXY: bool = cfg!(feature = "proxy");
    pub const COMPRESSION: bool = cfg!(feature = "compression");
    pub const STATIC: bool = cfg!(feature = "static");
    pub const HOTRELOAD: bool = cfg!(feature = "hotreload");
    pub const JWT: bool = cfg!(feature = "jwt");
    pub const CONSUMERS: bool = cfg!(feature = "consumers");
    pub const FORWARD_AUTH: bool = cfg!(feature = "forward-auth");
    pub const UPLOAD: bool = cfg!(feature = "upload");
    pub const REDIS: bool = cfg!(feature = "redis");
    pub const CACHE: bool = cfg!(feature = "cache");
    pub const ACME: bool = cfg!(feature = "acme");
    pub const FAULT_INJECTION: bool = cfg!(feature = "fault-injection");
    pub const OTLP: bool = cfg!(feature = "otlp");
    pub const TOKIO_METRICS: bool = cfg!(feature = "tokio-metrics");
}

// Aliases for the paths the moved files used in the root crate.
mod config {
    #[cfg(test)]
    pub(crate) use conduit_config::parse;
    pub(crate) use conduit_config::schema;
}

mod util {
    pub(crate) use conduit_core::util::log_writer;

    pub(crate) mod jwt_template {
        pub(crate) use conduit_auth_jwt::template::expand_jwt_templates;
    }
}
