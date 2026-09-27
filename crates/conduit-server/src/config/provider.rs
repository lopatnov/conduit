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
///
/// `conduit_config_core::provider::Validator<C>`'s own contract has no
/// concept of severity — it treats any non-empty `Vec<ValidationError>` as
/// a load failure. Advisory findings (e.g. a still-valid cert nearing
/// expiry, issue #191) must not be treated the same as a real config
/// error, so this closure partitions them itself before returning: warnings
/// are logged and swallowed, only hard errors are handed back — same split
/// `src/cli/serve.rs::run()` and the admin `/reload` handler already do
/// (issue #253).
pub fn file_provider(path: impl Into<std::path::PathBuf>) -> FileProvider {
    FileProvider::new(path).with_validator(|cfg| {
        let (warnings, hard_errors) = validate::partition_by_severity(validate::validate(cfg));
        for w in &warnings {
            tracing::warn!("config: {}: {}", w.path, w.message);
        }
        hard_errors
    })
}

/// Load a config file, validate it, and return the [`AppConfig`].
///
/// Returns an error if the file cannot be read, fails to parse, or has
/// hard validation errors (duplicate ports, etc.). Advisory findings (e.g.
/// a still-valid cert nearing expiry, issue #191) are logged but do not
/// fail the load — same split as [`file_provider`]'s validator and
/// `src/cli/serve.rs::run()` (issue #253).
pub fn load_and_validate(path: &Path) -> Result<AppConfig> {
    let cfg = load_config(path)?;
    let (warnings, hard_errors) = validate::partition_by_severity(validate::validate(&cfg));
    for w in &warnings {
        tracing::warn!("config: {}: {}", w.path, w.message);
    }
    if !hard_errors.is_empty() {
        let msgs: Vec<String> = hard_errors
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

    /// Generate a self-signed cert/key pair expiring 15 days from now —
    /// still valid, but inside `check_cert_expiry`'s 30-day warning window
    /// (issue #191/#253).
    fn near_expiry_cert_pair() -> (String, String) {
        let kp = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(15);
        let cert = params.self_signed(&kp).unwrap();
        (cert.pem(), kp.serialize_pem())
    }

    /// Write a config referencing a near-expiry (but valid) TLS cert/key
    /// pair, returning the temp dir (keeps cert/key/config files alive) and
    /// the config file path.
    fn write_near_expiry_tls_config() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let (cert_pem, key_pem) = near_expiry_cert_pair();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        std::fs::write(&cert_path, cert_pem).unwrap();
        std::fs::write(&key_path, key_pem).unwrap();

        // FileProvider<AppConfig>::load() deserializes straight into
        // AppConfig with no ConfigFile-enum shorthand normalization (that
        // only happens in the root crate's own load_config(), which
        // load_and_validate() uses) — so this needs the real shape
        // (explicit `sites` array), not the "Single" catch-all shorthand.
        let config_path = dir.path().join("conduit.json");
        let json = format!(
            r#"{{"sites": [{{"port": 0, "tls": {{"cert": {:?}, "key": {:?}}}}}]}}"#,
            cert_path.to_str().unwrap(),
            key_path.to_str().unwrap()
        );
        std::fs::write(&config_path, json).unwrap();
        (dir, config_path)
    }

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

    #[test]
    fn load_and_validate_accepts_near_expiry_cert_as_warning_not_error() {
        // Regression test for #253: a near-expiry-but-valid cert produces
        // only a Severity::Warning finding (issue #191) — load_and_validate
        // must not treat that the same as a hard validation error the way
        // it did before this fix (any non-empty Vec<ValidationError> was
        // treated as fatal, warnings included).
        let (_dir, path) = write_near_expiry_tls_config();
        let result = load_and_validate(&path);
        assert!(
            result.is_ok(),
            "a near-expiry (but valid) cert must not fail the load: {:?}",
            result.err()
        );
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

    #[tokio::test]
    async fn file_provider_accepts_near_expiry_cert_as_warning_not_error() {
        // Regression test for #253 — same as load_and_validate's version
        // above, but through FileProvider's own with_validator closure,
        // which has the identical bug potential (the generic Validator<C>
        // contract treats any non-empty Vec<ValidationError> as fatal).
        let (_dir, path) = write_near_expiry_tls_config();
        let provider = file_provider(&path);
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let result = provider.run(tx).await;
        assert!(
            result.is_ok(),
            "a near-expiry (but valid) cert must not fail the load: {:?}",
            result.err()
        );
        assert!(
            rx.recv().await.is_some(),
            "the config must still be sent to the receiver"
        );
    }
}
