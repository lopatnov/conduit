//! Facade over `conduit_runtime` (issue #145): the access log moved into `crates/conduit-runtime`.
//! Every item is re-exported here at its original path, so no call site changed.

pub use conduit_runtime::filter::logging::{write_access_log, AccessLogContext};
