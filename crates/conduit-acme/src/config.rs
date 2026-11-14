use std::time::Duration;

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
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AcmeConfig {
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
    /// The certificate profile to ask the CA for (RFC 9773-era `profile` field of a new order), for example `classic`,
    /// `tlsserver` or `shortlived` at Let's Encrypt. Absent: the CA's default (issue #594).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Seconds one request of an order may take (account setup, `newOrder`, `finalize`, certificate download). Absent:
    /// [`DEFAULT_ORDER_TIMEOUT_SECS`] (issue #746).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_timeout_secs: Option<u64>,
    /// Seconds the challenge validation (authorizations, challenge, polling until the order is ready) may take in all.
    /// Absent: [`DEFAULT_VALIDATION_TIMEOUT_SECS`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_timeout_secs: Option<u64>,
    /// Seconds the whole ACME Renewal Information probe may take (restoring the stored account and asking the CA); a
    /// single ARI request gets half of it. Absent: [`DEFAULT_ARI_TIMEOUT_SECS`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ari_timeout_secs: Option<u64>,
    /// The least seconds to wait before asking the CA about a certificate again, whatever its `Retry-After` says.
    /// Absent: [`DEFAULT_RETRY_AFTER_MIN_SECS`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_min_secs: Option<u64>,
    /// The most seconds to wait before asking the CA again, whatever its `Retry-After` says. Absent:
    /// [`DEFAULT_RETRY_AFTER_MAX_SECS`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_max_secs: Option<u64>,
    /// Seconds between two renewal checks of a certificate (the CA's `Retry-After` can bring a check earlier). The loop
    /// is shared by all sites, so with several values the shortest one counts. Absent:
    /// [`DEFAULT_CHECK_INTERVAL_SECS`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_interval_secs: Option<u64>,
}

/// One request of an order: instant-acme's HTTP client has no timeout of its own, so a CA that accepts the connection
/// and never answers would hold the sequential renewal loop (and start-up) forever.
pub const DEFAULT_ORDER_TIMEOUT_SECS: u64 = 60;
/// The validation phase of an order, in all.
pub const DEFAULT_VALIDATION_TIMEOUT_SECS: u64 = 180;
/// The whole ARI probe; a hung endpoint must not hold the renewal loop.
pub const DEFAULT_ARI_TIMEOUT_SECS: u64 = 30;
/// The CA's `Retry-After` is clamped so that a careless value can neither make the loop hammer the CA nor silence the
/// check (RFC 9773 section 4.3.2).
pub const DEFAULT_RETRY_AFTER_MIN_SECS: u64 = 5 * 60;
/// See [`DEFAULT_RETRY_AFTER_MIN_SECS`].
pub const DEFAULT_RETRY_AFTER_MAX_SECS: u64 = 24 * 3600;
/// How often the certificates are checked at most.
pub const DEFAULT_CHECK_INTERVAL_SECS: u64 = 12 * 3600;
/// The least `checkIntervalSecs` allowed (5 minutes, which is how seldom the check could run before the interval became
/// configurable): a check that finds a due certificate and fails is repeated after one interval, and a CA limits failed
/// orders (Let's Encrypt: about 5 failed validations an hour). The floor only rules out hammering the CA every few seconds;
/// an operator who sets the interval near it still risks that limit when a due certificate keeps failing.
pub const MIN_CHECK_INTERVAL_SECS: u64 = 5 * 60;
/// The least `retryAfterMinSecs` allowed (one minute): the CA's `Retry-After` is never taken as shorter than this when it is
/// asked about a certificate again.
pub const MIN_RETRY_AFTER_SECS: u64 = 60;
/// No configured duration may exceed this (one day), so that a typo (an extra zero, milliseconds for seconds) cannot
/// turn a limit into no limit.
pub const MAX_CONFIGURED_SECS: u64 = 24 * 3600;

/// The durations of an order and of the renewal loop, with the defaults filled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcmeTimings {
    /// One request of an order.
    pub order: Duration,
    /// The challenge validation.
    pub validation: Duration,
    /// The whole ARI probe.
    pub ari_probe: Duration,
    /// One ARI request (half of the probe).
    pub ari_ask: Duration,
    /// Least wait before asking the CA again.
    pub retry_after_min: Duration,
    /// Most wait before asking the CA again.
    pub retry_after_max: Duration,
    /// Time between two renewal checks.
    pub check_interval: Duration,
}

impl Default for AcmeTimings {
    fn default() -> Self {
        AcmeConfig::default().timings()
    }
}

impl AcmeConfig {
    /// The durations, each the configured value or its default. Call [`AcmeConfig::problems`] first: values it
    /// rejects (zero, too large, inverted clamps) are used as written.
    #[must_use]
    pub fn timings(&self) -> AcmeTimings {
        let secs = |v: Option<u64>, default: u64| Duration::from_secs(v.unwrap_or(default));
        let ari_probe = secs(self.ari_timeout_secs, DEFAULT_ARI_TIMEOUT_SECS);
        AcmeTimings {
            order: secs(self.order_timeout_secs, DEFAULT_ORDER_TIMEOUT_SECS),
            validation: secs(
                self.validation_timeout_secs,
                DEFAULT_VALIDATION_TIMEOUT_SECS,
            ),
            ari_probe,
            ari_ask: ari_probe / 2,
            retry_after_min: secs(self.retry_after_min_secs, DEFAULT_RETRY_AFTER_MIN_SECS),
            retry_after_max: secs(self.retry_after_max_secs, DEFAULT_RETRY_AFTER_MAX_SECS),
            check_interval: secs(self.check_interval_secs, DEFAULT_CHECK_INTERVAL_SECS),
        }
    }

    /// What is wrong with the configured durations, as `(field, message)` pairs: a value below its least (1 second,
    /// [`MIN_CHECK_INTERVAL_SECS`] for the check interval, [`MIN_RETRY_AFTER_SECS`] for the Retry-After minimum) or above [`MAX_CONFIGURED_SECS`], and `retryAfterMinSecs` above `retryAfterMaxSecs` (with the defaults filled in, so
    /// that raising only the minimum past the default maximum is caught too).
    #[must_use]
    pub fn problems(&self) -> Vec<(&'static str, String)> {
        // (field, value, the least it may be)
        let fields = [
            ("orderTimeoutSecs", self.order_timeout_secs, 1),
            ("validationTimeoutSecs", self.validation_timeout_secs, 1),
            ("ariTimeoutSecs", self.ari_timeout_secs, 1),
            (
                "retryAfterMinSecs",
                self.retry_after_min_secs,
                MIN_RETRY_AFTER_SECS,
            ),
            ("retryAfterMaxSecs", self.retry_after_max_secs, 1),
            (
                "checkIntervalSecs",
                self.check_interval_secs,
                MIN_CHECK_INTERVAL_SECS,
            ),
        ];
        let mut out = Vec::new();
        for (name, value, least) in fields {
            match value {
                Some(v) if v < least => out.push((
                    name,
                    format!(
                        "tls.acme.{name} is {v} s, below the limit of {least} s: leave it out to use the default"
                    ),
                )),
                Some(v) if v > MAX_CONFIGURED_SECS => out.push((
                    name,
                    format!(
                        "tls.acme.{name} is {v} s, above the limit of {MAX_CONFIGURED_SECS} s (one day): \
                         a value that large is more likely a typo than a choice"
                    ),
                )),
                _ => {}
            }
        }
        let t = self.timings();
        if t.retry_after_min > t.retry_after_max {
            out.push((
                "retryAfterMinSecs",
                format!(
                    "tls.acme.retryAfterMinSecs ({} s) must not exceed retryAfterMaxSecs ({} s; the default when not set)",
                    t.retry_after_min.as_secs(),
                    t.retry_after_max.as_secs()
                ),
            ));
        }
        out
    }
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
    fn the_durations_default_to_the_values_that_were_fixed_in_code() {
        let t = AcmeConfig::default().timings();
        assert_eq!(t.order, Duration::from_secs(60));
        assert_eq!(t.validation, Duration::from_secs(180));
        assert_eq!(t.ari_probe, Duration::from_secs(30));
        assert_eq!(t.ari_ask, Duration::from_secs(15));
        assert_eq!(t.retry_after_min, Duration::from_secs(300));
        assert_eq!(t.retry_after_max, Duration::from_secs(86_400));
        assert_eq!(t.check_interval, Duration::from_secs(43_200));
        assert!(AcmeConfig::default().problems().is_empty());
    }

    #[test]
    fn configured_durations_are_read_from_camel_case_keys() {
        let cfg: AcmeConfig = serde_json::from_str(
            r#"{ "email": "ops@example.com", "orderTimeoutSecs": 5, "validationTimeoutSecs": 6,
                 "ariTimeoutSecs": 8, "retryAfterMinSecs": 90, "retryAfterMaxSecs": 100,
                 "checkIntervalSecs": 310 }"#,
        )
        .unwrap();
        let t = cfg.timings();
        assert_eq!(
            [
                t.order,
                t.validation,
                t.ari_probe,
                t.ari_ask,
                t.retry_after_min,
                t.retry_after_max,
                t.check_interval
            ]
            .map(|d| d.as_secs()),
            [5, 6, 8, 4, 90, 100, 310]
        );
        assert!(cfg.problems().is_empty());
    }

    #[test]
    fn a_zero_or_oversized_duration_is_a_problem_and_so_is_an_inverted_clamp() {
        let one = |f: fn(&mut AcmeConfig)| {
            let mut cfg = AcmeConfig::default();
            f(&mut cfg);
            cfg.problems()
                .into_iter()
                .map(|(n, _)| n)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            one(|c| c.order_timeout_secs = Some(0)),
            ["orderTimeoutSecs"]
        );
        assert_eq!(
            one(|c| c.validation_timeout_secs = Some(86_401)),
            ["validationTimeoutSecs"]
        );
        assert_eq!(one(|c| c.ari_timeout_secs = Some(0)), ["ariTimeoutSecs"]);
        assert_eq!(
            one(|c| c.check_interval_secs = Some(u64::MAX)),
            ["checkIntervalSecs"]
        );
        // the limit itself is allowed
        assert!(one(|c| c.check_interval_secs = Some(86_400)).is_empty());
        // a CA must not be polled every few seconds: 5 minutes is the least for the check interval, one minute for the
        // Retry-After minimum
        assert_eq!(
            one(|c| c.check_interval_secs = Some(299)),
            ["checkIntervalSecs"]
        );
        assert_eq!(
            one(|c| c.check_interval_secs = Some(1)),
            ["checkIntervalSecs"]
        );
        assert!(one(|c| c.check_interval_secs = Some(300)).is_empty());
        assert_eq!(
            one(|c| c.retry_after_min_secs = Some(59)),
            ["retryAfterMinSecs"]
        );
        assert!(one(|c| c.retry_after_min_secs = Some(60)).is_empty());
        // the other limits may be short
        assert!(one(|c| c.order_timeout_secs = Some(1)).is_empty());
        // inverted, with both set and with only the minimum raised past the default maximum
        assert_eq!(
            one(|c| {
                c.retry_after_min_secs = Some(600);
                c.retry_after_max_secs = Some(60);
            }),
            ["retryAfterMinSecs"]
        );
        assert!(one(|c| c.retry_after_min_secs = Some(86_400)).is_empty());
        assert_eq!(
            one(|c| c.retry_after_max_secs = Some(60)),
            ["retryAfterMinSecs"]
        );
        // equal is fine
        assert!(one(|c| {
            c.retry_after_min_secs = Some(60);
            c.retry_after_max_secs = Some(60);
        })
        .is_empty());
    }

    #[test]
    fn full_config_round_trips_through_json() {
        let cfg = AcmeConfig {
            email: "ops@example.com".to_string(),
            directory: Some("https://acme.example.com/directory".to_string()),
            storage: Some("./certs".to_string()),
            challenge: Some("http-01".to_string()),
            profile: Some("tlsserver".to_string()),
            ..Default::default()
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
            profile: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).expect("serialize");
        assert!(!json.contains("directory"), "got: {json}");
        assert!(!json.contains("storage"), "got: {json}");
        assert!(!json.contains("challenge"), "got: {json}");
        assert!(!json.contains("profile"), "got: {json}");
    }
}
