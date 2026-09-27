//! Facade over `conduit_server` (issue #147): the real `validate()`/`feature_warnings()` logic and its
//! auth/cross_site/proxy_loop/site/tls/warnings submodules moved into `crates/conduit-server`. Every item is
//! re-exported here at its original path, so no call site changed.
//!
//! What stays here on purpose: the sixteen `conduit_x::warnings::COMPILED == cfg!(feature = "x")` compile-time
//! asserts below. They check each feature-owning crate's `COMPILED` const against the ROOT crate's own Cargo
//! features (`cfg!()` always resolves against whichever crate physically compiles it) — moving them into
//! `conduit-server` would force that crate to mirror sixteen feature names it otherwise has no reason to depend
//! on (`otlp`/`wasm`/`rhai`/`jwt`/`forward-auth`/... — see `crates/conduit-server/src/lib.rs`'s own doc comment)
//! for zero runtime benefit, since the warning-dispatch functions themselves never read their own crate's
//! `cfg!()`. `tests.rs`/`golden_tests.rs`/`testdata/` stay here too, for the same reason: they pin
//! `validate()`'s/`feature_warnings()`'s output against THIS crate's actual compiled feature set.

pub use conduit_config_core::validation::{partition_by_severity, Severity, ValidationError};
pub use conduit_server::config::validate::{feature_warnings, sanitize_for_log, validate};

// The feature-off texts live in the crate that owns each feature (#316); each crate reports through its `COMPILED` whether *its*
// feature is on in this build. The root's feature of the same name must agree: if a crate's feature were on while the root's is
// off (or the reverse), a warning would silently vanish or appear where it should not. Turn any such drift into a build error.
const _: () = assert!(
    conduit_otlp::warnings::COMPILED == cfg!(feature = "otlp"),
    "`conduit-otlp`'s `otlp` feature and the root crate's `otlp` feature must be enabled together"
);
const _: () = assert!(
    conduit_middleware::warnings::WASM_COMPILED == cfg!(feature = "wasm"),
    "`conduit-middleware`'s `wasm` feature and the root crate's `wasm` feature must be enabled together"
);
const _: () = assert!(
    conduit_middleware::warnings::RHAI_COMPILED == cfg!(feature = "rhai"),
    "`conduit-middleware`'s `rhai` feature and the root crate's `rhai` feature must be enabled together"
);
const _: () = assert!(
    conduit_proxy_http::warnings::COMPILED == cfg!(feature = "proxy"),
    "`conduit-proxy-http`'s `proxy` feature and the root crate's `proxy` feature must be enabled together"
);
const _: () = assert!(
    conduit_auth_jwt::warnings::COMPILED == cfg!(feature = "jwt"),
    "`conduit-auth-jwt`'s `jwt` feature and the root crate's `jwt` feature must be enabled together"
);
const _: () = assert!(
    conduit_auth_forward::warnings::COMPILED == cfg!(feature = "forward-auth"),
    "`conduit-auth-forward`'s `forward-auth` feature and the root crate's `forward-auth` feature must be enabled together"
);
const _: () = assert!(
    conduit_acme::warnings::COMPILED == cfg!(feature = "acme"),
    "`conduit-acme`'s `acme` feature and the root crate's `acme` feature must be enabled together"
);
const _: () = assert!(
    conduit_tcp::warnings::COMPILED == cfg!(feature = "tcp"),
    "`conduit-tcp`'s `tcp` feature and the root crate's `tcp` feature must be enabled together"
);
const _: () = assert!(
    conduit_ratelimit::warnings::COMPILED == cfg!(feature = "redis"),
    "`conduit-ratelimit`'s `redis` feature and the root crate's `redis` feature must be enabled together"
);
const _: () = assert!(
    conduit_cache::warnings::COMPILED == cfg!(feature = "cache"),
    "`conduit-cache`'s `cache` feature and the root crate's `cache` feature must be enabled together"
);
const _: () = assert!(
    conduit_upload::warnings::COMPILED == cfg!(feature = "upload"),
    "`conduit-upload`'s `upload` feature and the root crate's `upload` feature must be enabled together"
);
const _: () = assert!(
    conduit_faults::warnings::COMPILED == cfg!(feature = "fault-injection"),
    "`conduit-faults`'s `fault-injection` feature and the root crate's `fault-injection` feature must be enabled together"
);
const _: () = assert!(
    conduit_auth_consumers::warnings::COMPILED == cfg!(feature = "consumers"),
    "`conduit-auth-consumers`'s `consumers` feature and the root crate's `consumers` feature must be enabled together"
);
const _: () = assert!(
    conduit_auth_consumers::warnings::JWT_COMPILED == cfg!(feature = "jwt"),
    "`conduit-auth-consumers`'s `jwt` feature and the root crate's `jwt` feature must be enabled together"
);
const _: () = assert!(
    conduit_compression::warnings::COMPILED == cfg!(feature = "compression"),
    "`conduit-compression`'s `compression` feature and the root crate's `compression` feature must be enabled together"
);
const _: () = assert!(
    conduit_static::warnings::COMPILED == cfg!(feature = "static"),
    "`conduit-static`'s `static` feature and the root crate's `static` feature must be enabled together"
);
const _: () = assert!(
    conduit_hotreload::warnings::COMPILED == cfg!(feature = "hotreload"),
    "`conduit-hotreload`'s `hotreload` feature and the root crate's `hotreload` feature must be enabled together"
);

#[cfg(test)]
mod golden_tests;
#[cfg(test)]
mod tests;
