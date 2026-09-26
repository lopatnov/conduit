//! Facade over `conduit_runtime` (issue #145): the health-check handler moved into `crates/conduit-runtime`.
//! Every item is re-exported here at its original path, so no call site changed.

pub use conduit_runtime::handler::health::{handle_health, HealthHandler, UpstreamHealthInfo};
