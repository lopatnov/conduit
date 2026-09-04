//! Extracted into `crates/conduit-metrics` (issue #114/#140) — this is a
//! facade re-export so `crate::handler::metrics::{MetricsHandler,
//! handle_metrics}` keep resolving to the same items at the same location
//! for every existing call site/test. See that crate's `src/handler.rs` for
//! the implementation: the Prometheus `/metrics` text-exposition endpoint,
//! reading the process-wide default registry via `prometheus::gather()`.
//! `ConduitMetrics` (the metric-*registration* struct) stays in this crate
//! (destined for the future `conduit-runtime`, per issue #140's own scope
//! note).
//!
//! `MetricsConfig` (the schema type) moved too, but is re-exported at its
//! *original* location, `crate::config::schema` — see that module for the
//! facade.
pub use conduit_metrics::handler::{handle_metrics, MetricsHandler};
