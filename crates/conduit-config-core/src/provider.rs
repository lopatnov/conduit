//! Generic configuration provider abstraction.
//!
//! A [`Provider<C>`] is a source that supplies the initial config value `C`
//! and streams updates whenever the configuration changes. `C` is fully
//! opaque here — this crate never names a concrete schema type. The root
//! crate binds `C = AppConfig` via a type alias (see `src/config/provider.rs`).
//!
//! ## Adding a new provider
//!
//! 1. Create a struct that holds connection / path information.
//! 2. `impl Provider<C> for YourProvider`.
//! 3. In `run()`: send the initial config immediately, then loop and send updates.
//! 4. Return `Ok(())` when `tx.send()` returns `Err` (receiver dropped = server
//!    shutting down).

use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;

use crate::parse::load_file;
use crate::validation::ValidationError;

// ── Trait ─────────────────────────────────────────────────────────────────────

/// A configuration source that streams `C` updates.
#[async_trait]
pub trait Provider<C>: Send + Sync
where
    C: Send + 'static,
{
    /// Human-readable name used in log messages.
    fn name(&self) -> &'static str;

    /// Stream configuration updates.
    ///
    /// Send the initial config immediately via `tx.send()`, then keep watching
    /// for changes and send further updates.  Return `Ok(())` when `tx` is
    /// dropped (server shutting down).
    async fn run(&self, tx: mpsc::Sender<C>) -> Result<()>;
}

// ── FileProvider ──────────────────────────────────────────────────────────────

type Validator<C> = Box<dyn Fn(&C) -> Vec<ValidationError> + Send + Sync>;

/// Load configuration `C` from a JSON or YAML file.
///
/// **One-shot mode** (default): sends the initial config once and returns.
/// The server only reconfigures when `conduit reload` is called.
///
/// **Auto-reload mode** ([`with_auto_reload`]): watches the file with `notify`
/// and automatically sends a new config whenever the file changes on disk.
/// Useful when the file is managed externally (config management, k8s ConfigMap
/// volume mount, Consul Template, …).
///
/// An optional [`with_validator`] closure runs after each successful parse
/// (initial load and every reload); a non-empty result is treated as a load
/// failure — the file is rejected and the current config kept, same as a
/// parse error.
///
/// [`with_auto_reload`]: FileProvider::with_auto_reload
/// [`with_validator`]: FileProvider::with_validator
pub struct FileProvider<C> {
    path: PathBuf,
    auto_reload: bool,
    validator: Option<Validator<C>>,
}

impl<C> FileProvider<C> {
    /// Create a one-shot provider that reads `path` once.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            auto_reload: false,
            validator: None,
        }
    }

    /// Enable automatic config reload when the file changes on disk.
    #[must_use]
    pub fn with_auto_reload(mut self) -> Self {
        self.auto_reload = true;
        self
    }

    /// Reject a parsed config (and keep the previous one) whenever `f`
    /// returns a non-empty `Vec<ValidationError>`.
    #[must_use]
    pub fn with_validator(
        mut self,
        f: impl Fn(&C) -> Vec<ValidationError> + Send + Sync + 'static,
    ) -> Self {
        self.validator = Some(Box::new(f));
        self
    }

    /// Path to the config file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl<C: DeserializeOwned> FileProvider<C> {
    /// Load `C` from [`Self::path`] and run the validator (if any).
    ///
    /// Returns an error if the file cannot be read, fails to parse, or the
    /// validator reports errors.
    fn load(&self) -> Result<C> {
        let cfg: C = load_file(&self.path)?;
        if let Some(validator) = &self.validator {
            let errors = validator(&cfg);
            if !errors.is_empty() {
                let msgs: Vec<String> = errors
                    .iter()
                    .map(|e| format!("{}: {}", e.path, e.message))
                    .collect();
                anyhow::bail!("config validation errors:\n{}", msgs.join("\n"));
            }
        }
        Ok(cfg)
    }
}

#[async_trait]
impl<C> Provider<C> for FileProvider<C>
where
    C: DeserializeOwned + Send + 'static,
{
    fn name(&self) -> &'static str {
        "file"
    }

    async fn run(&self, tx: mpsc::Sender<C>) -> Result<()> {
        // Always send the initial configuration immediately.
        let cfg = self.load()?;
        if tx.send(cfg).await.is_err() {
            return Ok(()); // Receiver dropped — server shutting down.
        }

        if !self.auto_reload {
            return Ok(()); // One-shot provider: work is done.
        }

        // Auto-reload: watch the file for changes using notify.
        // notify callbacks are sync, so we bridge to async via a tokio channel.
        let (change_tx, mut change_rx) = tokio::sync::mpsc::channel::<()>(8);

        // Canonicalize the path so symlinks are resolved before comparison.
        // On macOS /tmp is a symlink to /private/tmp; notify returns canonical paths,
        // so comparing without canonicalization would silently drop all events.
        let target_path = self
            .path
            .canonicalize()
            .unwrap_or_else(|_| self.path.clone());

        // Build the watcher using a helper to keep CC low.
        let mut watcher = build_file_watcher(target_path, change_tx)?;

        {
            use notify::Watcher as _;
            // Watch the parent directory to survive atomic saves (editor temp-file + rename).
            let dir = self.path.parent().unwrap_or(&self.path);
            watcher.watch(dir, notify::RecursiveMode::NonRecursive)?;
        }

        while let Some(()) = change_rx.recv().await {
            if !self.reload_on_change(&tx).await {
                break; // Server shutting down.
            }
        }

        Ok(())
    }
}

impl<C: DeserializeOwned + Send + 'static> FileProvider<C> {
    /// Reload the config and send it on `tx`.
    ///
    /// Returns `true` if the reload was successful or failed gracefully.
    /// Returns `false` when the receiver has been dropped (server shutting down).
    async fn reload_on_change(&self, tx: &tokio::sync::mpsc::Sender<C>) -> bool {
        match self.load() {
            Ok(cfg) => {
                if tx.send(cfg).await.is_err() {
                    return false; // Server shutting down.
                }
                tracing::info!(
                    path = %self.path.display(),
                    "config auto-reloaded from file"
                );
                true
            }
            Err(e) => {
                tracing::warn!(
                    path = %self.path.display(),
                    error = %e,
                    "config file changed but reload failed — keeping current config"
                );
                true
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Create a [`notify`] watcher that fires on the given `target_path`.
///
/// Only `Modify`, `Create`, and `Remove` events for `target_path` are forwarded
/// to `tx`.  All other events (and events for unrelated files in the same
/// directory) are silently dropped.
///
/// Watching the *parent directory* rather than the file itself handles atomic
/// saves (temp-file + rename pattern used by many editors and config-management
/// tools).
fn build_file_watcher(
    target_path: std::path::PathBuf,
    tx: tokio::sync::mpsc::Sender<()>,
) -> Result<impl notify::Watcher> {
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        use notify::EventKind::*;
        let involves_target = event
            .paths
            .iter()
            .any(|p| p.canonicalize().unwrap_or_else(|_| p.clone()) == target_path);
        if !involves_target {
            return;
        }
        if matches!(event.kind, Modify(_) | Create(_) | Remove(_)) {
            // blocking_send is safe here: the callback runs on a notify
            // thread pool, not inside an async context.
            let _ = tx.blocking_send(());
        }
    })?;
    Ok(watcher)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize, PartialEq, Clone)]
    struct Toy {
        #[serde(default)]
        port: Option<u16>,
    }

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

    const MINIMAL: &str = r#"{"port": 0}"#;

    // ── Provider trait ────────────────────────────────────────────────────────

    #[test]
    fn file_provider_name_is_file() {
        assert_eq!(FileProvider::<Toy>::new("/tmp/x.json").name(), "file");
    }

    #[test]
    fn file_provider_path_stored_correctly() {
        let p = FileProvider::<Toy>::new("/tmp/conduit.json");
        assert_eq!(p.path(), Path::new("/tmp/conduit.json"));
    }

    #[test]
    fn with_auto_reload_sets_flag() {
        let p = FileProvider::<Toy>::new("/tmp/x.json").with_auto_reload();
        assert!(p.auto_reload);
    }

    // ── One-shot mode ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn oneshot_sends_initial_config_and_returns() {
        let (_f, path) = write_config(MINIMAL);
        let provider = FileProvider::<Toy>::new(&path);
        let (tx, mut rx) = mpsc::channel(1);

        let result = provider.run(tx).await;
        assert!(result.is_ok());

        let cfg = rx.recv().await.expect("must receive config");
        assert_eq!(cfg.port, Some(0));
    }

    #[tokio::test]
    async fn oneshot_only_sends_one_message() {
        let (_f, path) = write_config(MINIMAL);
        let provider = FileProvider::<Toy>::new(&path);
        let (tx, mut rx) = mpsc::channel(4);

        provider.run(tx).await.unwrap();

        // Exactly one message, then the channel is empty.
        assert!(rx.recv().await.is_some(), "must receive one config");
        assert!(
            rx.try_recv().is_err(),
            "one-shot must send exactly one config"
        );
    }

    #[tokio::test]
    async fn oneshot_returns_ok_when_receiver_dropped_before_send() {
        let (_f, path) = write_config(MINIMAL);
        let provider = FileProvider::<Toy>::new(&path);
        let (tx, rx) = mpsc::channel(1);
        drop(rx); // Drop before run() starts.

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), provider.run(tx))
            .await
            .expect("must not hang");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn oneshot_fails_on_missing_file() {
        let provider = FileProvider::<Toy>::new("/nonexistent/conduit.json");
        let (tx, _rx) = mpsc::channel(1);
        assert!(provider.run(tx).await.is_err());
    }

    // ── load / with_validator ────────────────────────────────────────────────

    #[test]
    fn load_accepts_valid_config() {
        let (_f, path) = write_config(MINIMAL);
        assert!(FileProvider::<Toy>::new(&path).load().is_ok());
    }

    #[test]
    fn load_rejects_missing_file() {
        assert!(FileProvider::<Toy>::new("/nonexistent.json")
            .load()
            .is_err());
    }

    #[test]
    fn load_rejects_parse_error() {
        let (_f, path) = write_config(r#"{"port": "not-a-number"}"#);
        assert!(FileProvider::<Toy>::new(&path).load().is_err());
    }

    #[test]
    fn with_validator_rejects_config_that_fails_validation() {
        let (_f, path) = write_config(MINIMAL);
        let provider = FileProvider::<Toy>::new(&path)
            .with_validator(|_| vec![ValidationError::new("port", "must not be zero")]);
        let err = provider.load().expect_err("validator must reject");
        assert!(err.to_string().contains("must not be zero"));
    }

    #[test]
    fn with_validator_accepts_config_that_passes_validation() {
        let (_f, path) = write_config(MINIMAL);
        let provider = FileProvider::<Toy>::new(&path).with_validator(|_| vec![]);
        assert!(provider.load().is_ok());
    }

    // ── Auto-reload ───────────────────────────────────────────────────────────

    /// Write `content` to an existing open file, truncating first.
    fn overwrite(path: &Path, content: &str) {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .expect("open for overwrite");
        f.write_all(content.as_bytes()).expect("write");
        f.flush().expect("flush");
    }

    #[tokio::test]
    async fn auto_reload_sends_initial_then_update() {
        let (_f, path) = write_config(MINIMAL);
        let provider = FileProvider::<Toy>::new(&path).with_auto_reload();
        let (tx, mut rx) = mpsc::channel(8);

        let handle = tokio::spawn(async move { provider.run(tx).await });

        // 1. Receive initial config.
        let initial = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("timeout on initial config")
            .expect("initial config");
        assert_eq!(initial.port, Some(0));

        // 2. Change the file and wait for the reload.
        // Wait longer before writing so any spurious watcher events (common on
        // macOS FSEvents) from the initial file creation settle first.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let updated = r#"{"port": 7777}"#;
        overwrite(&path, updated);

        // Retry-receive: on macOS the watcher may fire a spurious reload with the
        // old config before delivering the actual change event.  Keep draining
        // the channel until we see port 7777 or the overall deadline expires.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                panic!("timeout waiting for reloaded config with port 7777");
            }
            let cfg = tokio::time::timeout(remaining, rx.recv())
                .await
                .expect("timeout waiting for reloaded config")
                .expect("channel closed before reload");
            if cfg.port == Some(7777) {
                break; // got the expected update
            }
            // Spurious event with old config — keep waiting.
        }

        handle.abort();
    }

    #[tokio::test]
    async fn auto_reload_ignores_invalid_file_changes() {
        let (_f, path) = write_config(MINIMAL);
        let provider = FileProvider::<Toy>::new(&path).with_auto_reload();
        let (tx, mut rx) = mpsc::channel(8);

        let handle = tokio::spawn(async move { provider.run(tx).await });

        // Drain initial config.
        tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();

        // Write invalid JSON.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        overwrite(&path, r#"THIS IS NOT JSON"#);

        // No new config should arrive (invalid file is ignored).
        let result = tokio::time::timeout(std::time::Duration::from_millis(800), rx.recv()).await;
        assert!(
            result.is_err(),
            "invalid file change must NOT send a new config"
        );

        handle.abort();
    }
}
