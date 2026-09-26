//! Response-shaping and per-site service blocks: compression, response time, security
//! headers, CORS, hot reload, the site health endpoint, header transforms, upload, metrics.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

// ── Compression ────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-compression` (issue #114/#138) — this is a
/// facade re-export so `crate::config::schema::{CompressionConfig,
/// CompressionOptions}` keep resolving to the same types at the same
/// location for every existing call site/test.
///
/// `false` | `true` | `{ "algorithms": ["br", "gzip"], "level": 6, "minBytes": 1024 }`
pub use conduit_compression::{CompressionConfig, CompressionOptions};

// ── Response time ──────────────────────────────────────────────────────────

/// `false` | `true` | `{ "digits": 3 }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ResponseTimeConfig {
    Enabled(bool),
    Options(ResponseTimeOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResponseTimeOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digits: Option<u8>,
}

// ── Security headers ───────────────────────────────────────────────────────

/// Extracted into `crates/conduit-security-headers` (issue #114/#136) — this
/// is a facade re-export so `crate::config::schema::SecurityHeadersConfig`
/// keeps resolving to the same type at the same location for every existing
/// call site/test.
pub use conduit_security_headers::SecurityHeadersConfig;
/// Extracted into `crates/conduit-security-headers` (issue #114/#136) — see
/// the [`SecurityHeadersConfig`] re-export above.
pub use conduit_security_headers::SecurityHeadersOptions;

// ── CORS ───────────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-cors` (issue #114/#136) — this is a facade
/// re-export so `crate::config::schema::CorsConfig` keeps resolving to the
/// same type at the same location for every existing call site/test.
pub use conduit_cors::CorsConfig;
/// Extracted into `crates/conduit-cors` (issue #114/#136) — see the
/// [`CorsConfig`] re-export above.
pub use conduit_cors::CorsOptions;

// ── Hot reload ─────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-hotreload` (issue #114/#140) — this is a
/// facade re-export so `crate::config::schema::{HotReloadConfig,
/// HotReloadOptions}` keep resolving to the same types at the same location
/// for every existing call site/test.
pub use conduit_hotreload::HotReloadConfig;
/// Extracted into `crates/conduit-hotreload` (issue #114/#140) — see the
/// [`HotReloadConfig`] re-export above.
pub use conduit_hotreload::HotReloadOptions;

// ── Health check (site-level endpoint) ────────────────────────────────────

/// `false` | `true` | `{ "path": "/__health__", "includeUpstreams": true }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum HealthCheckConfig {
    Enabled(bool),
    Options(HealthCheckOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_upstreams: Option<bool>,
}

// ── Header transform ───────────────────────────────────────────────────────

/// Static header injection / removal for requests or responses.
///
/// Applied unconditionally to every request (request transform) or every
/// upstream response (response transform) for the site.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct HeaderTransformConfig {
    /// Headers to add or overwrite.  The value is the literal string to set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_headers: Option<IndexMap<String, String>>,
    /// Header names to remove.  Case-insensitive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove_headers: Option<Vec<String>>,
}

// ── Upload ─────────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-upload` (issue #114/#131) — this is a
/// facade re-export so `crate::config::schema::UploadConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_upload::UploadConfig;

// ── Metrics ────────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-metrics` (issue #114/#140) — this is a
/// facade re-export so `crate::config::schema::MetricsConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_metrics::MetricsConfig;
