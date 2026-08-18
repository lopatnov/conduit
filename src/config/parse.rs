use std::path::Path;

use anyhow::Result;
use conduit_config_core::parse::{from_json_str, from_yaml_str, load_file};

use crate::config::schema::{AppConfig, ConfigFile};

/// Load and parse a config file from disk, performing env interpolation first.
///
/// Both JSON (`.json`) and YAML (`.yaml` / `.yml`) are supported.
/// The format is determined by the file extension; JSON is the default.
pub fn load_config(path: &Path) -> Result<AppConfig> {
    let file: ConfigFile = load_file(path)?;
    Ok(normalize(file))
}

/// Parse a config JSON string, performing env interpolation first.
pub fn from_str(text: &str) -> Result<AppConfig> {
    let file: ConfigFile = from_json_str(text)?;
    Ok(normalize(file))
}

/// Parse a config YAML string, performing env interpolation first.
pub fn from_yaml(text: &str) -> Result<AppConfig> {
    let file: ConfigFile = from_yaml_str(text)?;
    Ok(normalize(file))
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
    use crate::config::schema::SiteConfig;

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
        use crate::config::schema::GlobalConfig;
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
        let out = normalize(ConfigFile::Single(Box::new(SiteConfig::default())));
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
