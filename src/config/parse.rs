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
