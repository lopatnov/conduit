//! The Admin API (issue #114/#146): the axum server, bearer-token authentication and the endpoint handlers, moved out
//! of the root crate's `src/admin/api.rs`.
//!
//! **What is here and what is not.** Eleven of the twelve endpoints live in [`api`]: `GET /status`, `POST /shutdown`,
//! `GET /upstreams`, `POST /upstreams/{add,remove,weight}`, `GET /rate-limits`, `DELETE /cache/purge`,
//! `POST`/`DELETE /ip-deny` and `POST /certs/reload`. `POST /reload` stays in the root crate — it needs the root's
//! config validation — and joins the router through [`api::build_router`]'s `extra` parameter, which is merged
//! **before** the bearer-token layer: it is the only way to add a route, so a route contributed from outside cannot
//! skip authentication. The root also keeps `AdminApiService`, the process's background supervisor (rate-limit
//! cleanup, health probes, the hot-reload watcher), which runs even when no admin server is configured.
//!
//! **Why one crate.** Every build compiles every Layer-1 crate already (`conduit-config` embeds their config types),
//! and the handlers need `AppState`, a Layer-3 type, so the endpoints cannot sit in the feature crates; only
//! `/cache/purge` depends on a feature at all — see the design note on issue #146.

pub mod api;

/// The features this crate is compiled with, mirrored by a compile-time assert in the root crate
/// (`src/admin/api.rs`): a root build whose `cache` feature does not reach this crate would answer 501 on
/// `DELETE /cache/purge` in a build that has a cache, or claim `purged: false` against a store nothing writes to.
pub mod features {
    pub const CACHE: bool = cfg!(feature = "cache");
}

// Aliases for the paths the moved code used in the root crate, so the bodies stay byte-identical (same device as
// `conduit-runtime`).
mod config {
    pub(crate) use conduit_config::schema;
}

#[cfg(test)]
mod filter {
    pub(crate) use conduit_runtime::filter::rate_limit;
}

mod proxy {
    #[cfg(feature = "cache")]
    pub(crate) use conduit_runtime::proxy::cache;
    pub(crate) use conduit_runtime::proxy::{health, service};
}
