use conduit_config_core::redact::Redacted;
use serde::{Deserialize, Serialize};
use std::fmt;

// ── Metrics ────────────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetricsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl fmt::Debug for MetricsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MetricsConfig")
            .field("path", &self.path)
            .field("token", &self.token.as_ref().map(|_| Redacted))
            .finish()
    }
}

#[cfg(test)]
mod redaction_tests {
    use super::*;

    #[test]
    fn metrics_config_token_is_redacted() {
        let cfg = MetricsConfig {
            path: Some("/metrics".to_string()),
            token: Some("super-secret-metrics-token".to_string()),
        };
        let debug = format!("{cfg:?}");
        assert!(
            !debug.contains("super-secret-metrics-token"),
            "token leaked into Debug output: {debug}"
        );
        assert!(debug.contains("[REDACTED]"), "got: {debug}");
        assert!(
            debug.contains("/metrics"),
            "non-secret field must still print: {debug}"
        );
    }

    #[test]
    fn metrics_config_absent_token_shows_none() {
        let cfg = MetricsConfig {
            path: None,
            token: None,
        };
        let debug = format!("{cfg:?}");
        assert!(debug.contains("None"), "got: {debug}");
        assert!(!debug.contains("[REDACTED]"), "got: {debug}");
    }
}
