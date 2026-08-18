//! Configuration provider abstraction.
//!
//! A [`Provider`] is a source that supplies the initial [`AppConfig`] and
//! streams updates whenever the configuration changes. This lets Conduit be
//! configured from multiple backends without changing the server core:
//!
//! | Provider             | Source                   | Updates             |
//! |----------------------|--------------------------|---------------------|
//! | [`FileProvider`]     | JSON / YAML file on disk | optional file-watch |
//! | `KubernetesProvider` | `ConduitSite` CRDs       | k8s watch events    |
//!
//! ## Adding a new provider
//!
//! 1. Create a struct that holds connection / path information.
//! 2. `impl Provider<AppConfig> for YourProvider`.
//! 3. In `run()`: send the initial config immediately, then loop and send updates.
//! 4. Return `Ok(())` when `tx.send()` returns `Err` (receiver dropped = server
//!    shutting down).
//!
//! The generic `Provider<C>`/`FileProvider<C>` mechanism lives in the Layer-0
//! [`conduit_config_core::provider`] crate (issue #127) — this module binds
//! `C = AppConfig` and wires up the real validator.

use std::path::Path;

use anyhow::Result;

pub use conduit_config_core::provider::Provider;

use crate::config::schema::AppConfig;
use crate::config::{load_config, validate};

/// [`FileProvider`] bound to conduit's real config schema.
///
/// See [`conduit_config_core::provider::FileProvider`] for the generic
/// mechanism (one-shot / auto-reload / injected validator).
pub type FileProvider = conduit_config_core::provider::FileProvider<AppConfig>;

/// Build a [`FileProvider`] for `path`, pre-wired to reject configs that
/// fail [`validate::validate`].
pub fn file_provider(path: impl Into<std::path::PathBuf>) -> FileProvider {
    FileProvider::new(path).with_validator(validate::validate)
}

/// Load a config file, validate it, and return the [`AppConfig`].
///
/// Returns an error if the file cannot be read, fails to parse, or has
/// validation errors (duplicate ports, etc.).
pub fn load_and_validate(path: &Path) -> Result<AppConfig> {
    let cfg = load_config(path)?;
    let errors = validate::validate(&cfg);
    if !errors.is_empty() {
        let msgs: Vec<String> = errors
            .iter()
            .map(|e| format!("{}: {}", e.path, e.message))
            .collect();
        anyhow::bail!("config validation errors:\n{}", msgs.join("\n"));
    }
    Ok(cfg)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::path::PathBuf;

    use super::*;

    /// Write `content` to a temporary `.json` file and return `(file, path)`.
    fn write_config(content: &str) -> (tempfile::NamedTempFile, PathBuf) {
        let mut f = tempfile::Builder::new()
            .suffix(".json")
            .tempfile()
            .expect("tempfile");
        f.write_all(content.as_bytes()).expect("write");
        f.flush().expect("flush");
        let path = f.path().to_path_buf();
        (f, path)
    }

    const MINIMAL: &str = r#"{"global":{"admin":{"bind":"127.0.0.1:0"}},"sites":[{"port":0}]}"#;

    // ── load_and_validate ─────────────────────────────────────────────────────

    #[test]
    fn load_and_validate_accepts_valid_config() {
        let (_f, path) = write_config(MINIMAL);
        assert!(load_and_validate(&path).is_ok());
    }

    #[test]
    fn load_and_validate_rejects_missing_file() {
        assert!(load_and_validate(Path::new("/nonexistent.json")).is_err());
    }

    #[test]
    fn load_and_validate_rejects_parse_error() {
        let (_f, path) = write_config(r#"{"port": "not-a-number"}"#);
        assert!(load_and_validate(&path).is_err());
    }

    // ── file_provider ─────────────────────────────────────────────────────────

    /// Closes the gap where `with_validator` had no live caller: `file_provider`
    /// wires the real `validate::validate` in, so a config that parses fine but
    /// fails business-rule validation (duplicate host:port) must be rejected.
    #[tokio::test]
    async fn file_provider_rejects_config_that_fails_validation() {
        let duplicate_host_port = r#"{"global":{"admin":{"bind":"127.0.0.1:0"}},"sites":[
              {"host":"a.example.com","port":8080},
              {"host":"a.example.com","port":8080}
           ]}"#;
        let (_f, path) = write_config(duplicate_host_port);
        let provider = file_provider(&path);
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let result = provider.run(tx).await;
        assert!(
            result.is_err(),
            "duplicate host:port must be rejected by the wired validator"
        );
    }
}
