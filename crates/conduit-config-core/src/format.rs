use std::path::Path;

/// Serialization format of a config file, determined by its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Json,
    Yaml,
}

impl ConfigFormat {
    /// Detect the format from a file path's extension.
    ///
    /// `.yaml`/`.yml` (case-insensitive) select YAML; everything else
    /// (including no extension) defaults to JSON.
    pub fn from_path(path: &Path) -> Self {
        let ext = path
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .map(str::to_lowercase)
            .unwrap_or_default();
        match ext.as_str() {
            "yaml" | "yml" => Self::Yaml,
            _ => Self::Json,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_extension_detected() {
        assert_eq!(
            ConfigFormat::from_path(Path::new("c.json")),
            ConfigFormat::Json
        );
    }

    #[test]
    fn yaml_extension_detected() {
        assert_eq!(
            ConfigFormat::from_path(Path::new("c.yaml")),
            ConfigFormat::Yaml
        );
    }

    #[test]
    fn yml_extension_detected() {
        assert_eq!(
            ConfigFormat::from_path(Path::new("c.yml")),
            ConfigFormat::Yaml
        );
    }

    #[test]
    fn uppercase_extension_detected() {
        assert_eq!(
            ConfigFormat::from_path(Path::new("c.YAML")),
            ConfigFormat::Yaml
        );
    }

    #[test]
    fn missing_extension_defaults_to_json() {
        assert_eq!(ConfigFormat::from_path(Path::new("c")), ConfigFormat::Json);
    }
}
