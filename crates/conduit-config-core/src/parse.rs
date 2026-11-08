use std::path::Path;

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::env::{interpolate, interpolate_yaml};
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
    from_json_str_verbatim(&interpolate(text))
}

/// Parse a config JSON string of type `T` exactly as written, **without** env interpolation.
///
/// For text that is already interpolated, such as a diagnostic re-parse of a value that was read from a
/// document: interpolating it a second time would expand a `$NAME` that an environment value contains (issue #586).
pub fn from_json_str_verbatim<T: DeserializeOwned>(text: &str) -> Result<T> {
    let probe: VersionProbe = serde_json::from_str(text).unwrap_or_default();
    check_version(probe)?;

    let jd = &mut serde_json::Deserializer::from_str(text);
    serde_path_to_error::deserialize(jd)
        .map_err(|e| anyhow::anyhow!("Config parse error at '{}': {}", e.path(), e.inner()))
}

/// Parse a config YAML string of type `T`, performing env interpolation first.
///
/// The references are written by [`interpolate_yaml`], which knows where a value stands (plain, `"..."`, `'...'`), so
/// a value arrives unchanged; a value that cannot be written where its reference stands is an error naming the
/// variable (issue #680).
pub fn from_yaml_str<T: DeserializeOwned>(text: &str) -> Result<T> {
    let text = interpolate_yaml(text).map_err(|e| anyhow::anyhow!("Config parse error: {e}"))?;

    let probe: VersionProbe = serde_yaml::from_str(&text).unwrap_or_default();
    check_version(probe)?;

    let de = serde_yaml::Deserializer::from_str(&text);
    serde_path_to_error::deserialize(de)
        .map_err(|e| anyhow::anyhow!("Config parse error at '{}': {}", e.path(), e.inner()))
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

    /// Issue #586: the verbatim parse does not expand a reference that the text contains; `from_json_str` does.
    #[test]
    #[serial_test::serial]
    fn the_verbatim_json_parse_does_not_interpolate() {
        std::env::set_var("CONDUIT_PARSE_586", "expanded");
        let text = r#"{ "name": "$CONDUIT_PARSE_586" }"#;
        let expanded: serde_json::Value = from_json_str(text).unwrap();
        let verbatim: serde_json::Value = from_json_str_verbatim(text).unwrap();
        std::env::remove_var("CONDUIT_PARSE_586");
        assert_eq!(expanded["name"], "expanded");
        assert_eq!(verbatim["name"], "$CONDUIT_PARSE_586");
    }

    /// Issue #680: a value arrives in a YAML file as it is, or the load fails naming the variable.
    #[test]
    #[serial_test::serial]
    fn a_yaml_value_with_a_backslash_or_a_quote_arrives_unchanged() {
        let value = r#"pa\ss"word'x"#;
        std::env::set_var("CONDUIT_PARSE_680", value);
        for text in [
            "name: $CONDUIT_PARSE_680\n",
            "name: \"$CONDUIT_PARSE_680\"\n",
        ] {
            let got: serde_json::Value = from_yaml_str(text).unwrap();
            assert_eq!(got["name"], value, "{text}");
        }
        // a value a plain scalar cannot carry is refused, naming the variable and never the value
        std::env::set_var("CONDUIT_PARSE_680", "a: b #c");
        let e = from_yaml_str::<serde_json::Value>("name: $CONDUIT_PARSE_680\n")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("CONDUIT_PARSE_680") && !e.contains("a: b #c"),
            "{e}"
        );
        // and is fine in double quotes
        let got: serde_json::Value = from_yaml_str("name: \"$CONDUIT_PARSE_680\"\n").unwrap();
        assert_eq!(got["name"], "a: b #c");
        std::env::remove_var("CONDUIT_PARSE_680");
    }

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
