use serde::{Deserialize, Serialize};

/// ACME (Let's Encrypt) automatic certificate provisioning configuration
/// (`tls.acme` in `SiteConfig`).
///
/// Always parseable regardless of Cargo feature selection — like
/// `conduit_otlp::OtlpConfig` — so a config file that sets `tls.acme` without
/// `--features acme` still parses cleanly and gets an explicit
/// `feature_warnings()` warning instead of a silent drop or a hard parse
/// error. Only the actual ACME HTTP-01 flow ([`crate::flow`]) and challenge
/// handler ([`crate::challenge`]) are gated behind this crate's own `acme`
/// Cargo feature.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AcmeConfig {
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
}
