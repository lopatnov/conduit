//! Facade re-export — the load-balancing strategy trait and concrete
//! implementations moved to `crates/conduit-upstream::strategy` (issue
//! #114/#142). Kept at this path so `crate::proxy::strategy::...` call sites
//! (routing, per-upstream connection-capacity admission, tests) keep
//! compiling unchanged.

pub use conduit_upstream::strategy::*;
