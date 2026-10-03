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

// serde_json is only pulled in via this crate's `acme` feature (no bare
// `serde_json` feature exists — the `dep:serde_json` syntax in `[features]`
// suppresses the implicit one), so these tests need it explicitly enabled.
#[cfg(all(test, feature = "acme"))]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_parses_with_defaults() {
        let cfg: AcmeConfig = serde_json::from_str(r#"{ "email": "ops@example.com" }"#)
            .expect("minimal config with only email must parse");
        assert_eq!(cfg.email, "ops@example.com");
        assert_eq!(cfg.directory, None);
        assert_eq!(cfg.storage, None);
        assert_eq!(cfg.challenge, None);
    }

    #[test]
    fn full_config_round_trips_through_json() {
        let cfg = AcmeConfig {
            email: "ops@example.com".to_string(),
            directory: Some("https://acme.example.com/directory".to_string()),
            storage: Some("./certs".to_string()),
            challenge: Some("http-01".to_string()),
        };
        let json = serde_json::to_string(&cfg).expect("serialize");
        let round_tripped: AcmeConfig =
            serde_json::from_str(&json).expect("deserialize round-trip");
        assert_eq!(cfg, round_tripped);
    }

    /// Regression test for `#[serde(skip_serializing_if = "Option::is_none")]`
    /// on the three optional fields: a config-round-trip test alone would not
    /// catch this attribute being dropped, since `"field": null` round-trips
    /// back to `None` just as well as an absent key does. Assert the keys are
    /// actually missing from the serialized output, not just that they
    /// deserialize back to `None`.
    #[test]
    fn none_optional_fields_are_omitted_from_serialized_json() {
        let cfg = AcmeConfig {
            email: "ops@example.com".to_string(),
            directory: None,
            storage: None,
            challenge: None,
        };
        let json = serde_json::to_string(&cfg).expect("serialize");
        assert!(!json.contains("directory"), "got: {json}");
        assert!(!json.contains("storage"), "got: {json}");
        assert!(!json.contains("challenge"), "got: {json}");
    }
}
