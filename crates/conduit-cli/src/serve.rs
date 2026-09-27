use std::path::Path;
use std::process;

use conduit_config::schema::AppConfig;
use conduit_server::config::validate;
use conduit_server::server::builder;

use super::config_path::load_config_or_exit;

// ── Server ─────────────────────────────────────────────────────────────────

pub fn run(config_path: &str) {
    let path = Path::new(&config_path);
    let cfg = load_config_or_exit(path);
    validate_or_exit(&cfg);
    if let Err(e) = builder::run_server(cfg, path.to_path_buf(), None) {
        eprintln!("server error: {e}");
        process::exit(1);
    }
}

/// Validate `cfg`, logging warnings and feature-off warnings, and exit the process on a hard
/// config error. Shared by the file-based [`run`] and [`run_kubernetes`]'s initial config — a
/// config sourced from `ConduitSite` CRDs is just as capable of having duplicate host:port
/// pairs, bad TLS config, etc. as one sourced from a file, and skipping validation for it was
/// a gap, not a deliberate difference (issue #492).
fn validate_or_exit(cfg: &AppConfig) {
    let errors = validate::validate(cfg);
    let (warnings, hard_errors) = validate::partition_by_severity(errors);
    // Advisory findings (e.g. a still-valid cert nearing expiry, issue #191)
    // are logged but must not block startup — only a real config error does.
    for w in &warnings {
        tracing::warn!("config: {}: {}", w.path, w.message);
    }
    if !hard_errors.is_empty() {
        for e in &hard_errors {
            eprintln!("config error at {}: {}", e.path, e.message);
        }
        process::exit(1);
    }
    for w in validate::feature_warnings(cfg) {
        tracing::warn!("{w}");
    }
}

// ── Kubernetes provider startup ────────────────────────────────────────────

/// Start Conduit using Kubernetes `ConduitSite` CRDs as the config source.
///
/// Spawns a background thread that runs the [`KubernetesProvider`], waits for
/// the initial config, then starts the server. Subsequent CRD changes are
/// received by the server's live-update watcher and hot-swapped without restart.
///
/// Requires: `cargo build --features kubernetes`.
#[cfg(feature = "kubernetes")]
pub fn run_kubernetes(namespace: &str) {
    use conduit_server::config::kubernetes::KubernetesProvider;
    use conduit_server::config::provider::Provider;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<conduit_config::schema::AppConfig>(4);
    let ns = namespace.to_owned();

    // Spawn the provider in its own thread with a dedicated Tokio runtime.
    std::thread::Builder::new()
        .name("kubernetes-provider".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime for kubernetes-provider");
            if let Err(e) = rt.block_on(KubernetesProvider::new(ns).run(tx)) {
                eprintln!("kubernetes provider error: {e}");
                process::exit(1);
            }
        })
        .expect("failed to spawn kubernetes-provider thread");

    // Block until the provider delivers the initial config (or the channel closes).
    let initial_config = match rx.blocking_recv() {
        Some(cfg) => cfg,
        None => {
            eprintln!("error: kubernetes provider closed before sending initial config");
            process::exit(1);
        }
    };

    tracing::info!(
        namespace,
        sites = initial_config.sites.len(),
        "initial config loaded from ConduitSite CRDs"
    );
    validate_or_exit(&initial_config);

    // Start the server; pass `rx` so CRD changes are hot-swapped automatically.
    if let Err(e) = builder::run_server(initial_config, std::path::PathBuf::new(), Some(rx)) {
        eprintln!("server error: {e}");
        process::exit(1);
    }
}
