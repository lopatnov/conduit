//! Facade over `conduit_runtime` (issue #145): request routing (`route_request` and its result types) moved into `crates/conduit-runtime`.
//! Every item is re-exported here at its original path, so no call site changed.

#[cfg(feature = "static")]
pub use conduit_runtime::proxy::router::resolve_static_roots;
pub use conduit_runtime::proxy::router::{
    parse_rfc9218_priority, route_request, url_to_proxy_upstream, RouteResolution, RouteResultAlias,
};
