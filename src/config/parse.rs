use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::config::schema::{AppConfig, ConfigFile, CONFIG_VERSION};

/// Reads only the version field before doing a full parse.
#[derive(Deserialize, Default)]
struct VersionProbe {
    version: Option<u32>,
}

/// Load and parse a config file from disk, performing env interpolation first.
pub fn load_config(path: &Path) -> Result<AppConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Cannot read config file: {}", path.display()))?;
    from_str(&raw).with_context(|| format!("Cannot parse config file: {}", path.display()))
}

/// Parse a config JSON string, performing env interpolation first.
pub fn from_str(text: &str) -> Result<AppConfig> {
    let text = crate::config::env::interpolate(text);

    let probe: VersionProbe = serde_json::from_str(&text).unwrap_or_default();
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

    let jd = &mut serde_json::Deserializer::from_str(&text);
    let file: ConfigFile = serde_path_to_error::deserialize(jd)
        .map_err(|e| anyhow::anyhow!("Config parse error at '{}': {}", e.path(), e.inner()))?;

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
    fn load_config_parses_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conduit.json");
        std::fs::write(&path, r#"{"port": 8080}"#).unwrap();
        assert!(load_config(&path).is_ok());
    }

    #[test]
    fn load_config_missing_file_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("__conduit_test_missing__.json"); // intentionally not created
        let result = load_config(&missing);
        assert!(result.is_err(), "missing file must return an error");
    }

    // ── from_str ──────────────────────────────────────────────────────────────

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
