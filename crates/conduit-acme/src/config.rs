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

    #[test]
    fn camel_case_field_names_are_accepted() {
        // #[serde(rename_all = "camelCase")] means the config-file key is
        // `directory`/`storage`/`challenge` as written here — no snake_case
        // aliasing exists, so a config using the plain field names must work.
        let cfg: AcmeConfig = serde_json::from_str(
            r#"{ "email": "a@b.com", "directory": "d", "storage": "s", "challenge": "c" }"#,
        )
        .expect("camelCase-named fields must parse");
        assert_eq!(cfg.directory.as_deref(), Some("d"));
        assert_eq!(cfg.storage.as_deref(), Some("s"));
        assert_eq!(cfg.challenge.as_deref(), Some("c"));
    }
}
