//! Top-level and global config: `ConfigFile`, `AppConfig`, `GlobalConfig`, `AdminConfig`.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fmt;

use super::{Redacted, SiteConfig};

// ── Top-level entry point ──────────────────────────────────────────────────

/// Serde tries variants top-to-bottom. Order is critical — do not reorder.
/// Single is a catch-all because all SiteConfig fields are Option.
/// SiteConfig is large (~1.7 KiB), so Single is boxed to keep the enum small; Full is boxed too since
/// `AppConfig` grew the `extra` maps (issue #681).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ConfigFile {
    Full(Box<AppConfig>),    // { "global": {...}, "sites": [...] }
    Sites(Vec<SiteConfig>),  // [{...}, {...}]
    Single(Box<SiteConfig>), // { "port": 8080, ... }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global: Option<GlobalConfig>,
    pub sites: Vec<SiteConfig>,
    /// Keys at the top of a full configuration that no field above names: a typo (`globl:` would drop every
    /// setting of `global`, the admin token included). Reported by `validate::feature_warnings()` (issue #681).
    #[serde(flatten)]
    pub extra: IndexMap<String, serde_json::Value>,
}

// ── Global config ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GlobalConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workers: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backlog: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shutdown_timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin: Option<AdminConfig>,
    /// OpenTelemetry OTLP tracing configuration.
    ///
    /// When set, Conduit exports distributed traces to the configured OTLP
    /// endpoint (Grafana Tempo, Jaeger, Honeycomb, OpenTelemetry Collector).
    /// Requires `--features otlp` at compile time.
    ///
    /// ```json
    /// { "global": { "otlp": { "endpoint": "http://otel-collector:4317",
    ///                         "serviceName": "conduit-gateway" } } }
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otlp: Option<OtlpConfig>,
    /// Keys under `global` that no field names (`workerz`, `shutdownTimeoutSec`): ignored, and reported by
    /// `validate::feature_warnings()` (issue #681).
    #[serde(flatten)]
    pub extra: IndexMap<String, serde_json::Value>,
}

/// OpenTelemetry OTLP exporter configuration.
///
/// Requires `--features otlp`.  When the `otlp` feature is disabled the
/// config field is still accepted (parsed without error) but silently ignored.
///
/// Extracted into `crates/conduit-otlp` (issue #114/#129) — this is a facade
/// re-export so `crate::config::schema::OtlpConfig` keeps resolving to the
/// same type at the same location for every existing call site/test.
pub use conduit_otlp::OtlpConfig;

#[derive(Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AdminConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>,
    /// Optional token for Admin API authentication.
    ///
    /// When set, every Admin API request must include
    /// `Authorization: Bearer <token>`.  Requests without the correct
    /// token receive `401 Unauthorized`.
    ///
    /// Useful in cloud/Kubernetes environments where the admin API is
    /// exposed beyond loopback (not recommended — prefer loopback + VPN).
    ///
    /// ```json
    /// { "admin": { "bind": "0.0.0.0:2019", "token": "$ADMIN_TOKEN" } }
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Keys under `global.admin` that no field names (`tokn`: the Admin API would run without a token): ignored,
    /// and reported by `validate::feature_warnings()` (issue #681).
    #[serde(flatten)]
    pub extra: IndexMap<String, serde_json::Value>,
}

impl fmt::Debug for AdminConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdminConfig")
            .field("bind", &self.bind)
            .field("token", &self.token.as_ref().map(|_| Redacted))
            .finish()
    }
}
