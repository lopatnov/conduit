//! Golden tests for `validate()` and `feature_warnings()` (issue #316, slice S0).
//!
//! The output of both functions depends on which of 16 Cargo features are compiled in, and a silently dropped
//! warning is exactly the failure a refactor of this module can cause. So the exact ordered output for one
//! kitchen-sink config (`testdata/kitchen_sink.json`) is recorded once, on the code as it was BEFORE #316 moved
//! anything, and every later slice must reproduce it byte for byte in every feature combination.
//!
//! The golden files hold one line per emitted message, in emission order:
//!
//! ```text
//! [cond,cond] <text>
//! ```
//!
//! where each `cond` is a root feature name that must be on (`jwt`) or off (`!jwt`); an empty `[]` means "always".
//! The test keeps the lines whose conditions hold for the features this build was compiled with and compares the
//! whole ordered list with the real output. Only the 16 known feature names are accepted: a typo panics instead of
//! silently matching nothing.
//!
//! Regenerating the baseline (only when the behaviour is meant to change, never inside a refactor): run
//! `dump_actual_output` (`--ignored --nocapture`) in several feature sets and merge the dumps with the script
//! described in the PR that introduced this file.

use super::{feature_warnings, validate};
use crate::config::from_str;

/// One site per feature-gated field, so every feature-off warning and the cross-site checks fire.
const KITCHEN_SINK: &str = include_str!("testdata/kitchen_sink.json");
/// 82 single-site configs taken from the inputs of the unit tests in `tests.rs` (one per site, own port), so a single
/// `validate()` call exercises every validator: tls, redirects, cors, ipFilter, limits, rateLimit, upload, metrics,
/// middleware, fallback, consumers, jwt, forwardAuth, tcp, proxy/routes/groups/rewrite, cache.
const VALIDATORS_SINK: &str = include_str!("testdata/validators_sink.json");
const FEATURE_WARNINGS_GOLDEN: &str = include_str!("testdata/feature_warnings.golden");
const VALIDATE_GOLDEN: &str = include_str!("testdata/validate.golden");
const VALIDATORS_GOLDEN: &str = include_str!("testdata/validators.golden");

/// Every root feature whose state can change what the two functions emit.
const FEATURES: [&str; 16] = [
    "proxy",
    "compression",
    "static",
    "hotreload",
    "jwt",
    "consumers",
    "forward-auth",
    "rhai",
    "wasm",
    "tcp",
    "upload",
    "redis",
    "cache",
    "acme",
    "fault-injection",
    "otlp",
];

fn enabled(feature: &str) -> bool {
    match feature {
        "proxy" => cfg!(feature = "proxy"),
        "compression" => cfg!(feature = "compression"),
        "static" => cfg!(feature = "static"),
        "hotreload" => cfg!(feature = "hotreload"),
        "jwt" => cfg!(feature = "jwt"),
        "consumers" => cfg!(feature = "consumers"),
        "forward-auth" => cfg!(feature = "forward-auth"),
        "rhai" => cfg!(feature = "rhai"),
        "wasm" => cfg!(feature = "wasm"),
        "tcp" => cfg!(feature = "tcp"),
        "upload" => cfg!(feature = "upload"),
        "redis" => cfg!(feature = "redis"),
        "cache" => cfg!(feature = "cache"),
        "acme" => cfg!(feature = "acme"),
        "fault-injection" => cfg!(feature = "fault-injection"),
        "otlp" => cfg!(feature = "otlp"),
        other => panic!("golden file names an unknown feature {other:?}"),
    }
}

fn enabled_features() -> Vec<&'static str> {
    FEATURES.iter().copied().filter(|f| enabled(f)).collect()
}

/// `true` when every condition in `cond` (`jwt`, `!jwt`, comma separated, empty = always) holds.
fn condition_holds(cond: &str) -> bool {
    cond.split(',')
        .filter(|t| !t.is_empty())
        .all(|t| match t.strip_prefix('!') {
            Some(feature) => !enabled(feature),
            None => enabled(feature_name(t)),
        })
}

fn feature_name(t: &str) -> &str {
    assert!(
        FEATURES.contains(&t),
        "golden file names an unknown feature {t:?}"
    );
    t
}

/// The golden lines that apply to this build, in order. Every line is parsed (also the ones that do not apply), so a
/// malformed line or a mistyped feature name fails in every profile instead of hiding in one.
fn expected(golden: &str) -> Vec<String> {
    golden
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|line| {
            let (cond, text) = line
                .strip_prefix('[')
                .and_then(|rest| rest.split_once("] "))
                .unwrap_or_else(|| panic!("malformed golden line: {line:?}"));
            for t in cond.split(',').filter(|t| !t.is_empty()) {
                feature_name(t.strip_prefix('!').unwrap_or(t));
            }
            condition_holds(cond).then(|| text.to_owned())
        })
        .collect()
}

fn actual_warnings() -> Vec<String> {
    let config = from_str(KITCHEN_SINK).expect("kitchen_sink.json must parse");
    assert_eq!(
        config.sites.len(),
        10,
        "the golden files assume 10 sites in the fixture"
    );
    feature_warnings(&config)
}

fn actual_validation() -> Vec<String> {
    validation_lines(KITCHEN_SINK, 10)
}

fn actual_validators() -> Vec<String> {
    validation_lines(VALIDATORS_SINK, 82)
}

fn validation_lines(fixture: &str, sites: usize) -> Vec<String> {
    let config = from_str(fixture).expect("the fixture must parse");
    assert_eq!(
        config.sites.len(),
        sites,
        "the golden files assume {sites} sites in the fixture"
    );
    validate(&config)
        .into_iter()
        .map(|e| {
            let severity = match e.severity {
                super::Severity::Error => "error",
                super::Severity::Warning => "warning",
            };
            format!("{severity}|{}|{}", e.path, e.message)
        })
        .collect()
}

#[test]
fn feature_warnings_match_the_golden_for_this_feature_set() {
    let expected = expected(FEATURE_WARNINGS_GOLDEN);
    assert!(
        !expected.is_empty(),
        "an empty expectation would make this test vacuous"
    );
    assert_eq!(
        actual_warnings(),
        expected,
        "feature_warnings() output changed (features on: {:?})",
        enabled_features()
    );
}

#[test]
fn validate_output_matches_the_golden_for_this_feature_set() {
    let expected = expected(VALIDATE_GOLDEN);
    assert!(
        !expected.is_empty(),
        "an empty expectation would make this test vacuous"
    );
    assert_eq!(
        actual_validation(),
        expected,
        "validate() output changed (features on: {:?})",
        enabled_features()
    );
}

#[test]
fn every_validator_output_matches_the_golden_for_this_feature_set() {
    let expected = expected(VALIDATORS_GOLDEN);
    assert!(
        expected.len() > 30,
        "the dense fixture is expected to produce dozens of messages, got {}",
        expected.len()
    );
    assert_eq!(
        actual_validators(),
        expected,
        "validate() output for the validators fixture changed (features on: {:?})",
        enabled_features()
    );
}

#[test]
#[should_panic(expected = "unknown feature")]
fn a_misspelt_feature_name_in_a_golden_line_panics() {
    let _ = expected("[!jwtt] some text\n");
}

#[test]
#[should_panic(expected = "malformed golden line")]
fn a_golden_line_without_a_condition_panics() {
    let _ = expected("some text without a condition\n");
}

/// Prints this build's actual output, one `GOLDEN-DUMP` line per item, for regenerating the baseline.
#[test]
#[ignore = "regenerates the golden baseline; run with --ignored --nocapture in several feature sets"]
fn dump_actual_output() {
    println!("GOLDEN-DUMP FEATURES {}", enabled_features().join(","));
    for w in actual_warnings() {
        assert!(
            !w.contains('\n'),
            "a warning with a newline cannot be stored line-based: {w:?}"
        );
        println!("GOLDEN-DUMP W {w}");
    }
    for e in actual_validation() {
        assert!(
            !e.contains('\n'),
            "a message with a newline cannot be stored line-based: {e:?}"
        );
        println!("GOLDEN-DUMP E {e}");
    }
    for v in actual_validators() {
        assert!(
            !v.contains('\n'),
            "a message with a newline cannot be stored line-based: {v:?}"
        );
        println!("GOLDEN-DUMP V {v}");
    }
}
