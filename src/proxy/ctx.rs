//! Facade over `conduit_runtime` (issue #145): the per-request state (`RequestCtx` and its sub-structs) moved into `crates/conduit-runtime`.
//! Every item is re-exported here at its original path, so no call site changed.

pub use conduit_runtime::proxy::ctx::{
    AcceptEncoding, LocalHandler, ProxyReqState, RequestCtx, RetryState, RouteRateLimit,
    UpstreamTarget,
};
