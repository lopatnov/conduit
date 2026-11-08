use std::path::Path;

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::env::interpolate;
use crate::format::ConfigFormat;

/// Highest config schema version this binary understands.
///
/// Bump when a breaking config-schema change ships; a config file declaring
/// a newer `version` is rejected with a clear upgrade message rather than
/// silently misparsed.
pub const CONFIG_VERSION: u32 = 1;

/// Reads only the version field before doing a full parse.
#[derive(Deserialize, Default)]
struct VersionProbe {
    version: Option<u32>,
}

/// Parse a config JSON string of type `T`, performing env interpolation first.
pub fn from_json_str<T: DeserializeOwned>(text: &str) -> Result<T> {
    let text = interpolate(text);

    let probe: VersionProbe = serde_json::from_str(&text).unwrap_or_default();
    check_version(probe)?;

    let jd = &mut serde_json::Deserializer::from_str(&text);
    serde_path_to_error::deserialize(jd)
        .map_err(|e| anyhow::anyhow!("Config parse error at '{}': {}", e.path(), e.inner()))
}

/// How a config is read. Duplicate keys, alias bombs and deep nesting are refused by default; three defaults are
/// changed (issue #676), so that the same file means the same thing and a mistake does not leak a secret:
/// - `yes`, `no`, `on` and `off` are not booleans (JSON and the schema say `true` / `false`);
/// - a merge key `<<` stays an ordinary key, as it was, instead of merging;
/// - an error does not quote the source. By default serde-saphyr prints the offending line and the lines around it,
///   and this text is read after `$VAR` interpolation, so a typo in a port would print a JWT secret on the next
///   line into the log, `conduit validate` output and the `/reload` reply. The error keeps its line and column.
fn yaml_options() -> serde_saphyr::Options {
    serde_saphyr::options! {
        strict_booleans: true,
        merge_keys: serde_saphyr::MergeKeyPolicy::AsOrdinary,
        with_snippet: false,
    }
}

/// Parse a config YAML string of type `T`, performing env interpolation first.
pub fn from_yaml_str<T: DeserializeOwned>(text: &str) -> Result<T> {
    let text = interpolate(text);

    let probe: VersionProbe =
        serde_saphyr::from_str_with_options(&text, yaml_options()).unwrap_or_default();
    check_version(probe)?;

    // serde-saphyr hands its deserializer to a closure that must return its own error type, so the path that
    // serde_path_to_error recorded is carried out in `path` and joined to the message afterwards.
    let mut path = String::new();
    serde_saphyr::with_deserializer_from_str_with_options(&text, yaml_options(), |de| {
        serde_path_to_error::deserialize(de).map_err(|e| {
            path = e.path().to_string();
            e.into_inner()
        })
    })
    .map_err(|e| anyhow::anyhow!("Config parse error at '{path}': {e}"))
}

/// Read `path` from disk and parse it as `T`, performing env interpolation first.
///
/// Both JSON (`.json`) and YAML (`.yaml` / `.yml`) are supported; the format
/// is determined by the file extension, defaulting to JSON.
pub fn load_file<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Cannot read config file: {}", path.display()))?;
    match ConfigFormat::from_path(path) {
        ConfigFormat::Yaml => from_yaml_str(&raw)
            .with_context(|| format!("Cannot parse config file: {}", path.display())),
        ConfigFormat::Json => from_json_str(&raw)
            .with_context(|| format!("Cannot parse config file: {}", path.display())),
    }
}

/// Reject configs whose `version` field is newer than what this binary supports.
fn check_version(probe: VersionProbe) -> Result<()> {
    if let Some(v) = probe.version {
        if v > CONFIG_VERSION {
            anyhow::bail!(
                "Config version {} is not supported (this binary supports up to version {}). \
                 Please upgrade conduit.",
                v,
                CONFIG_VERSION
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Toy {
        #[serde(default)]
        port: Option<u16>,
        #[serde(default)]
        version: Option<u32>,
    }

    // ── load_file ─────────────────────────────────────────────────────────────

    #[test]
    fn load_file_parses_valid_json_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conduit.json");
        std::fs::write(&path, r#"{"port": 8080}"#).unwrap();
        let cfg: Toy = load_file(&path).unwrap();
        assert_eq!(cfg.port, Some(8080));
    }

    #[test]
    fn load_file_parses_valid_yaml_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conduit.yaml");
        std::fs::write(&path, "port: 8080\n").unwrap();
        let cfg: Toy = load_file(&path).unwrap();
        assert_eq!(cfg.port, Some(8080));
    }

    #[test]
    fn load_file_parses_yml_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conduit.yml");
        std::fs::write(&path, "port: 3000\n").unwrap();
        let cfg: Toy = load_file(&path).expect("should parse .yml");
        assert_eq!(cfg.port, Some(3000));
    }

    #[test]
    fn load_file_missing_file_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("__conduit_test_missing__.json");
        let result: Result<Toy> = load_file(&missing);
        assert!(result.is_err());
    }

    // ── from_yaml_str ────────────────────────────────────────────────────────

    #[test]
    fn yaml_parses_simple_config() {
        let cfg: Toy = from_yaml_str("port: 8080\n").expect("parse");
        assert_eq!(cfg.port, Some(8080));
    }

    #[test]
    fn yaml_invalid_port_type_returns_error() {
        let result: Result<Toy> = from_yaml_str("port: \"not-a-number\"\n");
        assert!(result.is_err(), "invalid port type must fail");
    }

    #[test]
    fn yaml_unsupported_version_returns_error() {
        let result: Result<Toy> = from_yaml_str("version: 999\nport: 8080\n");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("999") || msg.contains("version") || msg.contains("upgrade"));
    }

    // The YAML reader's rules (issue #676): what a config may and may not contain.

    #[derive(Debug, Deserialize)]
    struct Nested {
        a: Inner,
    }

    #[derive(Debug, Deserialize)]
    struct Inner {
        port: u16,
        #[serde(default)]
        enabled: Option<bool>,
    }

    #[test]
    fn yaml_error_names_the_path_of_the_bad_value() {
        let err = from_yaml_str::<Nested>("a:\n  port: \"not-a-number\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Config parse error at 'a.port'"), "{err}");
    }

    #[test]
    fn yaml_errors_do_not_quote_the_neighbouring_lines() {
        // The text is read after `$VAR` interpolation, so a neighbouring line may hold a secret.
        let secret = "S3CRET-neighbour-TOKEN-value";
        let cases = [
            // a type error on line 3, the secret on line 1 and 2
            format!("token: {secret}\na:\n  port: abc\n"),
            // a syntax error (bad indentation) right after the secret
            format!("token: {secret}\na:\n  port: 1\n bad: indent\n"),
            // a repeated key right after the secret
            format!("token: {secret}\nport: 1\nport: 2\n"),
        ];
        for text in cases {
            let err = from_yaml_str::<Nested>(&text).unwrap_err().to_string();
            assert!(!err.contains(secret), "the error quotes the source: {err}");
            assert!(err.contains("line"), "the error keeps its position: {err}");
        }
    }

    #[test]
    fn yaml_duplicate_key_is_rejected() {
        let err = from_yaml_str::<Toy>("port: 1\nport: 2\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("duplicate"), "{err}");
        // also for a generic value, where serde_yaml let the last one win
        assert!(from_yaml_str::<serde_json::Value>("port: 1\nport: 2\n").is_err());
    }

    #[test]
    fn yaml_two_documents_are_rejected() {
        let err = from_yaml_str::<Toy>("port: 1\n---\nport: 2\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("multiple YAML documents"), "{err}");
    }

    #[test]
    fn yaml_yes_and_on_are_not_booleans() {
        for word in ["yes", "no", "on", "off", "Yes", "ON"] {
            let text = format!("a:\n  port: 1\n  enabled: {word}\n");
            assert!(
                from_yaml_str::<Nested>(&text).is_err(),
                "`{word}` must not be a boolean"
            );
        }
        for word in ["true", "false", "True", "FALSE"] {
            let text = format!("a:\n  port: 1\n  enabled: {word}\n");
            let parsed: Nested =
                from_yaml_str(&text).unwrap_or_else(|e| panic!("`{word}` is a boolean: {e}"));
            assert_eq!(parsed.a.port, 1);
            assert_eq!(parsed.a.enabled, Some(word.eq_ignore_ascii_case("true")));
        }
    }

    #[test]
    fn yaml_merge_key_is_an_ordinary_key() {
        let text = "base: &b {port: 1}\nm:\n  <<: *b\n  name: x\n";
        let value: serde_json::Value = from_yaml_str(text).unwrap();
        assert_eq!(value["m"]["name"], "x");
        assert!(
            value["m"].get("port").is_none(),
            "the merge key must not be expanded: {value}"
        );
        assert!(
            value["m"].get("<<").is_some(),
            "it stays a key named <<: {value}"
        );
    }

    #[test]
    fn yaml_alias_bomb_is_refused() {
        // 10 levels of 10 aliases each: 10^10 nodes if it were expanded.
        let mut text = String::from("a0: &a0 [x,x,x,x,x,x,x,x,x,x]\n");
        for i in 1..10 {
            let p = i - 1;
            text.push_str(&format!(
                "a{i}: &a{i} [{}]\n",
                vec![format!("*a{p}"); 10].join(",")
            ));
        }
        let started = std::time::Instant::now();
        let err = from_yaml_str::<serde_json::Value>(&text)
            .unwrap_err()
            .to_string();
        assert!(err.contains("budget"), "{err}");
        assert!(started.elapsed().as_secs() < 5, "refusing it must be quick");
    }

    #[test]
    fn yaml_deep_nesting_is_refused() {
        let text = format!("{}1{}", "[".repeat(5000), "]".repeat(5000));
        let err = from_yaml_str::<serde_json::Value>(&text)
            .unwrap_err()
            .to_string();
        assert!(err.contains("recursion limit"), "{err}");
    }

    #[test]
    fn yaml_differences_from_serde_yaml_are_the_documented_ones() {
        // Leniency that came with the new reader (JSON still wants a number): a quoted number, and `1_000`.
        assert_eq!(
            from_yaml_str::<Toy>("port: \"80\"\n").unwrap().port,
            Some(80)
        );
        assert_eq!(
            from_yaml_str::<Toy>("port: 1_000\n").unwrap().port,
            Some(1000)
        );
        assert!(from_json_str::<Toy>(r#"{"port": "80"}"#).is_err());
        // Strictness: a null key is an error (serde_yaml ignored it).
        assert!(from_yaml_str::<Toy>("~: 1\n").is_err());
        // Unchanged: leading zeros and exponents are not integers.
        assert!(from_yaml_str::<Toy>("port: 017\n").is_err());
        assert!(from_yaml_str::<Toy>("port: 1e3\n").is_err());
    }

    #[test]
    fn yaml_empty_and_comment_only_files_are_an_empty_config() {
        assert_eq!(from_yaml_str::<Toy>("").unwrap().port, None);
        assert_eq!(from_yaml_str::<Toy>("# nothing here\n").unwrap().port, None);
    }

    #[test]
    fn yaml_tab_indentation_is_rejected() {
        assert!(from_yaml_str::<Nested>("a:\n\tport: 1\n").is_err());
    }

    // ── from_json_str ─────────────────────────────────────────────────────────

    #[test]
    fn unsupported_version_returns_error() {
        let result: Result<Toy> = from_json_str(r#"{"version": 999, "port": 8080}"#);
        assert!(result.is_err(), "version 999 must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("999") || msg.contains("version") || msg.contains("upgrade"),
            "error message should mention version: {msg}"
        );
    }

    #[test]
    fn supported_version_parses_ok() {
        let result: Result<Toy> = from_json_str(r#"{"version": 1, "port": 8080}"#);
        assert!(result.is_ok());
    }

    #[test]
    fn no_version_field_parses_ok() {
        let result: Result<Toy> = from_json_str(r#"{"port": 8080}"#);
        assert!(result.is_ok());
    }

    #[test]
    fn invalid_json_returns_error() {
        let result: Result<Toy> = from_json_str(r#"{"port": "not-a-number"}"#);
        assert!(result.is_err(), "invalid port type must fail to parse");
    }
}
