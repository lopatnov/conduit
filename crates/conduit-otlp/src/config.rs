use serde::{Deserialize, Serialize};

/// OpenTelemetry OTLP exporter configuration.
///
/// Requires `--features otlp`.  When the `otlp` feature is disabled the
/// config field is still accepted (parsed without error) but silently ignored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OtlpConfig {
    /// OTLP gRPC endpoint.  Examples:
    /// - `"http://localhost:4317"` (local collector)
    /// - `"https://api.honeycomb.io:443"` (Honeycomb)
    pub endpoint: String,
    /// Service name reported in traces.  Defaults to `"conduit"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// Fraction of traces to export (0.0 = none, 1.0 = all).
    /// Defaults to `1.0` (100 %).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<f64>,
    /// Export timeout in milliseconds.  Defaults to `5000`.
    #[serde(rename = "timeoutMs", skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}
