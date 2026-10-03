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

/// Parse a config YAML string of type `T`, performing env interpolation first.
pub fn from_yaml_str<T: DeserializeOwned>(text: &str) -> Result<T> {
    let text = interpolate(text);

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
