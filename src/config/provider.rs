//! Configuration provider abstraction.
//!
//! A [`Provider`] is a source that supplies the initial [`AppConfig`] and
//! streams updates whenever the configuration changes.  This lets Conduit be
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
//! 2. `impl Provider for YourProvider`.
//! 3. In `run()`: send the initial config immediately, then loop and send updates.
//! 4. Return `Ok(())` when `tx.send()` returns `Err` (receiver dropped = server
//!    shutting down).

use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::config::schema::AppConfig;
use crate::config::{load_config, validate};

// ── Trait ─────────────────────────────────────────────────────────────────────

/// A configuration source that streams [`AppConfig`] updates.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Human-readable name used in log messages.
    fn name(&self) -> &'static str;

    /// Stream configuration updates.
    ///
    /// Send the initial config immediately via `tx.send()`, then keep watching
    /// for changes and send further updates.  Return `Ok(())` when `tx` is
    /// dropped (server shutting down).
    async fn run(&self, tx: mpsc::Sender<AppConfig>) -> Result<()>;
}

// ── FileProvider ──────────────────────────────────────────────────────────────

/// Load configuration from a JSON or YAML file.
///
/// **One-shot mode** (default): sends the initial config once and returns.
/// The server only reconfigures when `conduit reload` is called.
///
/// **Auto-reload mode** ([`with_auto_reload`]): watches the file with `notify`
/// and automatically sends a new config whenever the file changes on disk.
/// Useful when the file is managed externally (config management, k8s ConfigMap
/// volume mount, Consul Template, …).
///
/// [`with_auto_reload`]: FileProvider::with_auto_reload
pub struct FileProvider {
    path: PathBuf,
    auto_reload: bool,
}

impl FileProvider {
    /// Create a one-shot provider that reads `path` once.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            auto_reload: false,
        }
    }

    /// Enable automatic config reload when the file changes on disk.
    #[must_use]
    pub fn with_auto_reload(mut self) -> Self {
        self.auto_reload = true;
        self
    }

    /// Path to the config file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl Provider for FileProvider {
    fn name(&self) -> &'static str {
        "file"
    }

    async fn run(&self, tx: mpsc::Sender<AppConfig>) -> Result<()> {
        // Always send the initial configuration immediately.
        let cfg = load_and_validate(&self.path)?;
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
            if !reload_on_change(&self.path, &tx).await {
                break; // Server shutting down.
            }
        }

        Ok(())
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

/// Reload the config from `path` and send it on `tx`.
///
/// Returns `true` if the reload was successful or failed gracefully.
/// Returns `false` when the receiver has been dropped (server shutting down).
async fn reload_on_change(
    path: &std::path::Path,
    tx: &tokio::sync::mpsc::Sender<AppConfig>,
) -> bool {
    match load_and_validate(path) {
        Ok(cfg) => {
            if tx.send(cfg).await.is_err() {
                return false; // Server shutting down.
            }
            tracing::info!(
                path = %path.display(),
                "config auto-reloaded from file"
            );
            true
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "config file changed but reload failed — keeping current config"
            );
            true
        }
    }
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

    // ── Provider trait ────────────────────────────────────────────────────────

    #[test]
    fn file_provider_name_is_file() {
        assert_eq!(FileProvider::new("/tmp/x.json").name(), "file");
    }

    #[test]
    fn file_provider_path_stored_correctly() {
        let p = FileProvider::new("/tmp/conduit.json");
        assert_eq!(p.path(), Path::new("/tmp/conduit.json"));
    }

    #[test]
    fn with_auto_reload_sets_flag() {
        let p = FileProvider::new("/tmp/x.json").with_auto_reload();
        assert!(p.auto_reload);
    }

    // ── One-shot mode ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn oneshot_sends_initial_config_and_returns() {
        let (_f, path) = write_config(MINIMAL);
        let provider = FileProvider::new(&path);
        let (tx, mut rx) = mpsc::channel(1);

        let result = provider.run(tx).await;
        assert!(result.is_ok());

        let cfg = rx.recv().await.expect("must receive config");
        assert_eq!(cfg.sites.len(), 1);
        assert_eq!(cfg.sites[0].port, Some(0));
    }

    #[tokio::test]
    async fn oneshot_only_sends_one_message() {
        let (_f, path) = write_config(MINIMAL);
        let provider = FileProvider::new(&path);
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
        let provider = FileProvider::new(&path);
        let (tx, rx) = mpsc::channel(1);
        drop(rx); // Drop before run() starts.

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), provider.run(tx))
            .await
            .expect("must not hang");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn oneshot_fails_on_missing_file() {
        let provider = FileProvider::new("/nonexistent/conduit.json");
        let (tx, _rx) = mpsc::channel(1);
        assert!(provider.run(tx).await.is_err());
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
        let provider = FileProvider::new(&path).with_auto_reload();
        let (tx, mut rx) = mpsc::channel(8);

        let handle = tokio::spawn(async move { provider.run(tx).await });

        // 1. Receive initial config.
        let initial = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("timeout on initial config")
            .expect("initial config");
        assert_eq!(initial.sites[0].port, Some(0));

        // 2. Change the file and wait for the reload.
        // Wait longer before writing so any spurious watcher events (common on
        // macOS FSEvents) from the initial file creation settle first.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let updated = r#"{"global":{"admin":{"bind":"127.0.0.1:0"}},"sites":[{"port":7777}]}"#;
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
            if cfg.sites[0].port == Some(7777) {
                break; // got the expected update
            }
            // Spurious event with old config — keep waiting.
        }

        handle.abort();
    }

    #[tokio::test]
    async fn auto_reload_ignores_invalid_file_changes() {
        let (_f, path) = write_config(MINIMAL);
        let provider = FileProvider::new(&path).with_auto_reload();
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
