use std::path::Path;

use anyhow::{Context, Result};
use conduit_config_core::format::ConfigFormat;
use conduit_config_core::parse::{from_json_str, from_yaml_str};

use crate::schema::{AppConfig, ConfigFile, SiteConfig};

/// Load and parse a config file from disk, performing env interpolation first.
///
/// Both JSON (`.json`) and YAML (`.yaml` / `.yml`) are supported.
/// The format is determined by the file extension; JSON is the default.
pub fn load_config(path: &Path) -> Result<AppConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Cannot read config file: {}", path.display()))?;
    match ConfigFormat::from_path(path) {
        ConfigFormat::Yaml => from_yaml(&raw),
        ConfigFormat::Json => from_str(&raw),
    }
    .with_context(|| format!("Cannot parse config file: {}", path.display()))
}

/// Parse a config JSON string, performing env interpolation first.
pub fn from_str(text: &str) -> Result<AppConfig> {
    let file: ConfigFile = from_json_str(text).map_err(|e| diagnose(text, false, e))?;
    reject_failed_full(&file, text, false)?;
    Ok(normalize(file))
}

/// Parse a config YAML string, performing env interpolation first.
pub fn from_yaml(text: &str) -> Result<AppConfig> {
    let file: ConfigFile = from_yaml_str(text).map_err(|e| diagnose(text, true, e))?;
    reject_failed_full(&file, text, true)?;
    Ok(normalize(file))
}

/// `ConfigFile` is an untagged enum, so when no variant matches serde can only say "data did not
/// match any variant" and `serde_path_to_error` loses the field path (issue #570). Pick the variant
/// the document's shape implies (array → sites, `sites` key → full, else single site) and parse into
/// that concrete type, whose error names the field. The variant order of `ConfigFile` is unchanged.
fn diagnose(text: &str, yaml: bool, original: anyhow::Error) -> anyhow::Error {
    if !original.to_string().starts_with("Config parse error") {
        return original;
    }
    let value: Result<serde_json::Value> = if yaml {
        from_yaml_str(text)
    } else {
        from_json_str(text)
    };
    let Ok(value) = value else {
        return original;
    };
    let json = value.to_string();
    let refined = if value.is_array() {
        from_json_str::<Vec<SiteConfig>>(&json).err()
    } else if value.get("sites").is_some() {
        from_json_str::<AppConfig>(&json).err()
    } else {
        from_json_str::<SiteConfig>(&json).err()
    };
    refined.unwrap_or(original)
}

/// `Single` accepts anything, so a full config (`sites: [...]`) whose sites fail to parse used to
/// fall through to it and load as one empty site with `sites` parked in `extra`. Report the real
/// error instead (issue #570).
fn reject_failed_full(file: &ConfigFile, text: &str, yaml: bool) -> Result<()> {
    if let ConfigFile::Single(site) = file {
        if site.extra.contains_key("sites") {
            let value: Result<serde_json::Value> = if yaml {
                from_yaml_str(text)
            } else {
                from_json_str(text)
            };
            value.and_then(|v| from_json_str::<AppConfig>(&v.to_string()))?;
        }
    }
    Ok(())
}

/// Normalize all ConfigFile variants into a canonical AppConfig.
pub fn normalize(file: ConfigFile) -> AppConfig {
    match file {
        ConfigFile::Full(app) => app,
        ConfigFile::Sites(sites) => AppConfig {
            global: None,
            sites,
        },
        ConfigFile::Single(site) => AppConfig {
            global: None,
            sites: vec![*site],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #570: a wrong-typed value is reported with its field path, in every config shape.
    #[test]
    fn type_error_names_the_field() {
        let single = r#"{ "port": 8080, "rateLimit": { "windowSecs": "soon", "limit": 5 } }"#;
        let e = from_str(single).unwrap_err().to_string();
        assert!(e.contains("rateLimit.windowSecs"), "single: {e}");

        let sites = r#"{ "sites": [{ "port": 8080 }, { "port": "http" }] }"#;
        let e = from_str(sites).unwrap_err().to_string();
        assert!(e.contains("sites[1].port"), "sites: {e}");

        let array = r#"[{ "port": 1 }, { "port": "x" }]"#;
        let e = from_str(array).unwrap_err().to_string();
        assert!(e.contains("[1].port"), "array: {e}");

        let yaml = "port: 8080\nrateLimit: { windowSecs: soon, limit: 5 }\n";
        let e = from_yaml(yaml).unwrap_err().to_string();
        assert!(e.contains("rateLimit.windowSecs"), "yaml: {e}");
    }
    use crate::schema::SiteConfig;

    // ── load_config ───────────────────────────────────────────────────────────

    #[test]
    fn load_config_parses_valid_json_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conduit.json");
        std::fs::write(&path, r#"{"port": 8080}"#).unwrap();
        assert!(load_config(&path).is_ok());
    }

    #[test]
    fn load_config_parses_valid_yaml_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conduit.yaml");
        std::fs::write(&path, "port: 8080\n").unwrap();
        assert!(load_config(&path).is_ok());
    }

    #[test]
    fn load_config_parses_yml_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conduit.yml");
        std::fs::write(&path, "port: 3000\n").unwrap();
        let cfg = load_config(&path).expect("should parse .yml");
        assert_eq!(cfg.sites[0].port, Some(3000));
    }

    #[test]
    fn load_config_missing_file_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("__conduit_test_missing__.json");
        assert!(load_config(&missing).is_err());
    }

    // ── from_yaml ─────────────────────────────────────────────────────────────

    #[test]
    fn yaml_single_site_form() {
        let cfg = from_yaml("port: 8080\n").expect("parse");
        assert_eq!(cfg.sites[0].port, Some(8080));
    }

    #[test]
    fn yaml_sites_array_form() {
        let yaml = "- port: 8080\n- port: 8081\n";
        let cfg = from_yaml(yaml).expect("parse");
        assert_eq!(cfg.sites.len(), 2);
        assert_eq!(cfg.sites[0].port, Some(8080));
        assert_eq!(cfg.sites[1].port, Some(8081));
    }

    #[test]
    fn yaml_full_form_with_global() {
        let yaml = "global:\n  workers: 4\nsites:\n  - port: 9000\n";
        let cfg = from_yaml(yaml).expect("parse");
        assert_eq!(cfg.sites[0].port, Some(9000));
        assert!(cfg.global.is_some());
    }

    #[test]
    fn yaml_invalid_port_type_returns_error() {
        let result = from_yaml("port: \"not-a-number\"\n");
        assert!(result.is_err(), "invalid port type must fail");
    }

    #[test]
    fn yaml_unsupported_version_returns_error() {
        let result = from_yaml("version: 999\nport: 8080\n");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("999") || msg.contains("version") || msg.contains("upgrade"));
    }

    // ── from_str (JSON) ───────────────────────────────────────────────────────

    #[test]
    fn unsupported_version_returns_error() {
        let result = from_str(r#"{"version": 999, "port": 8080}"#);
        assert!(result.is_err(), "version 999 must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("999") || msg.contains("version") || msg.contains("upgrade"),
            "error message should mention version: {msg}"
        );
    }

    #[test]
    fn supported_version_parses_ok() {
        assert!(from_str(r#"{"version": 1, "port": 8080}"#).is_ok());
    }

    #[test]
    fn no_version_field_parses_ok() {
        assert!(from_str(r#"{"port": 8080}"#).is_ok());
    }

    #[test]
    fn invalid_json_returns_error() {
        let result = from_str(r#"{"port": "not-a-number"}"#);
        assert!(result.is_err(), "invalid port type must fail to parse");
    }

    #[test]
    fn normalize_full_config_returns_sites() {
        use crate::schema::GlobalConfig;
        let app = AppConfig {
            global: Some(GlobalConfig {
                workers: Some(4),
                ..Default::default()
            }),
            sites: vec![SiteConfig::default()],
        };
        let out = normalize(ConfigFile::Full(app.clone()));
        assert_eq!(out.sites.len(), 1);
        assert!(out.global.is_some());
    }

    #[test]
    fn normalize_single_site_wraps_in_vec() {
        let out = normalize(ConfigFile::Single(Box::default()));
        assert_eq!(out.sites.len(), 1);
        assert!(out.global.is_none());
    }

    // ── from_yaml ─────────────────────────────────────────────────────────────

    #[test]
    fn from_yaml_parses_simple_config() {
        let result = from_yaml("port: 9090\n");
        assert!(result.is_ok(), "valid YAML must parse: {:?}", result);
        let cfg = result.unwrap();
        assert_eq!(cfg.sites[0].port, Some(9090));
    }

    #[test]
    fn from_yaml_version_too_new_rejected() {
        let result = from_yaml("version: 999\nport: 8080\n");
        assert!(result.is_err(), "YAML version 999 must be rejected");
    }

    // ── normalize ─────────────────────────────────────────────────────────────

    #[test]
    fn normalize_sites_array_preserves_order() {
        let sites = vec![
            SiteConfig {
                port: Some(8080),
                ..Default::default()
            },
            SiteConfig {
                port: Some(8081),
                ..Default::default()
            },
        ];
        let out = normalize(ConfigFile::Sites(sites));
        assert_eq!(out.sites[0].port, Some(8080));
        assert_eq!(out.sites[1].port, Some(8081));
    }
}
