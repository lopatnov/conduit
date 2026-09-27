//! Parity asserts for `crates/conduit-server` (issue #147).
//!
//! `run_server()`, the Admin API supervisor/`POST /reload`, config validation and the providers all moved into
//! `crates/conduit-server` — see `src/cli/serve.rs`/`src/cli/validate.rs` (which call
//! `conduit_server::server::builder::run_server`/`conduit_server::config::validate` directly, no facade needed
//! since both are in `crates/conduit-cli`, a sibling of this crate) and `src/config/validate/mod.rs` (the
//! `config::validate` facade). This file has no re-exports of its own — it only holds the ten
//! `conduit_server::features::X == cfg!(feature = "x")` compile-time asserts, the same device
//! `src/proxy/service.rs` uses for `conduit-runtime`: a root feature that forwards into `conduit-server` but
//! whose forward is ever missed would compile fine and silently drop behaviour (health checks that never start,
//! a Redis connection that never happens, a background loop that never spawns) while `feature_warnings()` stays
//! quiet because the root believes the feature is on.
const _: () = assert!(
    conduit_server::features::PROXY == cfg!(feature = "proxy"),
    "`conduit-server`'s `proxy` feature and the root crate's `proxy` feature must be enabled together"
);
const _: () = assert!(
    conduit_server::features::REDIS == cfg!(feature = "redis"),
    "`conduit-server`'s `redis` feature and the root crate's `redis` feature must be enabled together"
);
const _: () = assert!(
    conduit_server::features::CONSUMERS == cfg!(feature = "consumers"),
    "`conduit-server`'s `consumers` feature and the root crate's `consumers` feature must be enabled together"
);
const _: () = assert!(
    conduit_server::features::CACHE == cfg!(feature = "cache"),
    "`conduit-server`'s `cache` feature and the root crate's `cache` feature must be enabled together"
);
const _: () = assert!(
    conduit_server::features::ACME == cfg!(feature = "acme"),
    "`conduit-server`'s `acme` feature and the root crate's `acme` feature must be enabled together"
);
const _: () = assert!(
    conduit_server::features::TCP == cfg!(feature = "tcp"),
    "`conduit-server`'s `tcp` feature and the root crate's `tcp` feature must be enabled together"
);
const _: () = assert!(
    conduit_server::features::UPLOAD == cfg!(feature = "upload"),
    "`conduit-server`'s `upload` feature and the root crate's `upload` feature must be enabled together"
);
const _: () = assert!(
    conduit_server::features::HOTRELOAD == cfg!(feature = "hotreload"),
    "`conduit-server`'s `hotreload` feature and the root crate's `hotreload` feature must be enabled together"
);
const _: () = assert!(
    conduit_server::features::TOKIO_METRICS == cfg!(feature = "tokio-metrics"),
    "`conduit-server`'s `tokio-metrics` feature and the root crate's `tokio-metrics` feature must be enabled together"
);
const _: () = assert!(
    conduit_server::features::KUBERNETES == cfg!(feature = "kubernetes"),
    "`conduit-server`'s `kubernetes` feature and the root crate's `kubernetes` feature must be enabled together"
);
