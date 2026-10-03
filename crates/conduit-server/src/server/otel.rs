//! OpenTelemetry OTLP initialisation facade.
//!
//! Extracted into `crates/conduit-otlp` (issue #114/#129) — this file is a
//! thin re-export so `crate::server::otel::init_tracer`/`shutdown_tracer`
//! call sites (`src/server/builder.rs`) don't need to change.
//!
//! Call [`init_tracer`] once at server startup to install the global tracer
//! provider.  All spans created via `opentelemetry::global::tracer("conduit")`
//! after this point are batched and exported to the configured OTLP endpoint.
//!
//! Call [`shutdown_tracer`] during graceful shutdown to flush in-flight spans.
pub use conduit_otlp::tracer::{init_tracer, shutdown_tracer};
