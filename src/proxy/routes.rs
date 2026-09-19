//! Facade re-export — `routes[]` array matching (path glob/method/header/
//! query/cookie predicates) moved to `crates/conduit-proxy-http::routes`
//! (issue #114/#143), alongside `RouteConfig`/`MatchConfig`, its two config
//! types. Kept at this path so `crate::proxy::routes::{RouteMatch,
//! match_routes, glob_match, ...}` call sites (`router.rs`) keep compiling
//! unchanged. Its unit tests moved with it — see
//! `crates/conduit-proxy-http/src/routes.rs`.

pub use conduit_proxy_http::routes::*;
