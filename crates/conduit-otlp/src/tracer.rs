//! OpenTelemetry OTLP tracer-provider lifecycle.
//!
//! Call [`init_tracer`] once at server startup to install the global tracer
//! provider.  All spans created via `opentelemetry::global::tracer("conduit")`
//! after this point are batched and exported to the configured OTLP endpoint.
//!
//! Call [`shutdown_tracer`] during graceful shutdown to flush in-flight spans.
//!
//! This module always compiles — the real exporter wiring is gated behind
//! this crate's own `otlp` Cargo feature (see `Cargo.toml`), so a build
//! without `--features otlp` still gets working no-op stubs instead of a
//! missing symbol. The root crate's per-request span creation/finishing
//! (`RequestCtx.otel_span`, `src/proxy/request_phase.rs`,
//! `src/proxy/logging_phase.rs`) stays in the root crate — this crate has no
//! knowledge of `Session`/`RequestCtx` (see `CONTRIBUTING.md`'s crate
//! extraction recipe: "conduit-core dependency is opt-in, not automatic").

use crate::config::OtlpConfig;

/// No-op stub when this crate's `otlp` feature is disabled — zero overhead.
#[cfg(not(feature = "otlp"))]
pub fn init_tracer(_cfg: &OtlpConfig) -> anyhow::Result<()> {
    tracing::debug!("OpenTelemetry OTLP disabled (compile with --features otlp to enable)");
    Ok(())
}

#[cfg(not(feature = "otlp"))]
pub fn shutdown_tracer() {}

#[cfg(feature = "otlp")]
pub use otlp_impl::{init_tracer, shutdown_tracer};

// ── Implementation ────────────────────────────────────────────────────────────

#[cfg(feature = "otlp")]
mod otlp_impl {
    use std::sync::OnceLock;
    use std::time::Duration;

    use opentelemetry::global;
    use opentelemetry::KeyValue;
    use opentelemetry_otlp::{SpanExporter, WithExportConfig};
    use opentelemetry_sdk::{
        trace::{RandomIdGenerator, Sampler, SdkTracerProvider, TracerProviderBuilder},
        Resource,
    };

    use super::OtlpConfig;

    /// The installed tracer provider, retained so [`shutdown_tracer`] can flush it.
    static TRACER_PROVIDER: OnceLock<SdkTracerProvider> = OnceLock::new();

    /// Initialise and install the global OTLP tracer provider.
    pub fn init_tracer(cfg: &OtlpConfig) -> anyhow::Result<()> {
        let service_name = cfg.service_name.as_deref().unwrap_or("conduit").to_owned();
        let timeout = Duration::from_millis(cfg.timeout_ms.unwrap_or(5_000));
        let sample_rate = cfg.sample_rate.unwrap_or(1.0).clamp(0.0, 1.0);

        let resource = Resource::builder()
            .with_attributes(vec![
                KeyValue::new("service.name", service_name),
                KeyValue::new("telemetry.sdk.language", "rust"),
            ])
            .build();

        // Build OTLP gRPC exporter (opentelemetry-otlp 0.32 API).
        let exporter = SpanExporter::builder()
            .with_tonic()
            .with_endpoint(&cfg.endpoint)
            .with_timeout(timeout)
            .build()
            .map_err(|e| anyhow::anyhow!("OTLP exporter: {e}"))?;

        // `with_sampler` is generic over T: ShouldSample, so each Sampler
        // variant is a different concrete type — we can't store them in a Box.
        // Build three separate provider instances and pick one.
        fn base(
            exporter: opentelemetry_otlp::SpanExporter,
            resource: Resource,
        ) -> TracerProviderBuilder {
            SdkTracerProvider::builder()
                .with_batch_exporter(exporter)
                .with_id_generator(RandomIdGenerator::default())
                .with_max_events_per_span(64)
                .with_max_attributes_per_span(32)
                .with_resource(resource)
        }

        let provider = if (sample_rate - 1.0).abs() < f64::EPSILON {
            base(exporter, resource)
                .with_sampler(Sampler::AlwaysOn)
                .build()
        } else if sample_rate == 0.0 {
            base(exporter, resource)
                .with_sampler(Sampler::AlwaysOff)
                .build()
        } else {
            base(exporter, resource)
                .with_sampler(Sampler::TraceIdRatioBased(sample_rate))
                .build()
        };

        global::set_tracer_provider(provider.clone());
        let _ = TRACER_PROVIDER.set(provider);

        tracing::info!(
            endpoint = %cfg.endpoint,
            sample_rate,
            "OpenTelemetry OTLP tracing enabled"
        );
        Ok(())
    }

    /// Flush buffered spans and shut down the tracer provider.
    pub fn shutdown_tracer() {
        if let Some(provider) = TRACER_PROVIDER.get() {
            if let Err(e) = provider.shutdown() {
                tracing::warn!("OpenTelemetry tracer shutdown error: {e}");
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use serial_test::serial;

        use super::*;

        /// `SpanExporter::builder()...build()` constructs a lazy tonic gRPC
        /// channel — it does not connect eagerly, so `init_tracer` is fully
        /// testable without a live OTLP collector (the actual network call
        /// only happens on the first batched export, which these tests never
        /// trigger). `#[serial]` because `init_tracer`/`shutdown_tracer`
        /// touch a process-wide global (`opentelemetry::global`'s tracer
        /// provider) — see `.claude/skills/testing/SKILL.md` idiom 3.
        fn cfg(sample_rate: Option<f64>) -> OtlpConfig {
            OtlpConfig {
                endpoint: "http://localhost:4317".to_owned(),
                service_name: Some("conduit-test".to_owned()),
                sample_rate,
                timeout_ms: Some(1_000),
            }
        }

        #[tokio::test]
        #[serial]
        async fn init_tracer_always_on_sampler_succeeds() {
            assert!(init_tracer(&cfg(Some(1.0))).is_ok());
        }

        #[tokio::test]
        #[serial]
        async fn init_tracer_always_off_sampler_succeeds() {
            assert!(init_tracer(&cfg(Some(0.0))).is_ok());
        }

        #[tokio::test]
        #[serial]
        async fn init_tracer_ratio_based_sampler_succeeds() {
            assert!(init_tracer(&cfg(Some(0.5))).is_ok());
        }

        #[tokio::test]
        #[serial]
        async fn init_tracer_defaults_service_name_and_sample_rate() {
            let cfg = OtlpConfig {
                endpoint: "http://localhost:4317".to_owned(),
                service_name: None,
                sample_rate: None,
                timeout_ms: None,
            };
            assert!(init_tracer(&cfg).is_ok());
        }

        #[tokio::test]
        #[serial]
        async fn shutdown_tracer_after_init_does_not_panic() {
            init_tracer(&cfg(Some(1.0))).expect("init_tracer");
            shutdown_tracer();
        }

        #[tokio::test]
        #[serial]
        async fn shutdown_tracer_without_init_does_not_panic() {
            // Only meaningful in isolation; other #[serial] tests in this
            // module may have already set TRACER_PROVIDER by the time this
            // runs, but shutdown_tracer() must be safe to call regardless.
            shutdown_tracer();
        }
    }
}
