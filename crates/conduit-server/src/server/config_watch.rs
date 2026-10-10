use std::sync::Arc;

use crate::config::schema::AppConfig;
use conduit_runtime::proxy::service::AppState;

/// Validate a config update received from a live provider (Kubernetes CRDs) before it is swapped
/// in. Returns the messages to log (advisory warnings + feature-off warnings) on success, or the
/// hard errors that must reject the update, without applying either as a side effect — pure and
/// unit-testable, unlike the thread/channel plumbing around it.
///
/// A config sourced from `ConduitSite` CRDs is just as capable of having duplicate host:port
/// pairs, bad TLS config, etc. as a file-based one, and [`spawn_config_update_watcher`] used to
/// swap every update in unconditionally, with no validation at all (issue #492).
///
/// A change to a cold field (port, tls cert/key, workers, backlog, admin bind) is rejected as
/// `POST /reload` rejects it: the running listeners cannot follow it, so swapping it in would leave
/// the config describing a server that is not the one running (issue #494).
fn check_config_update(
    current: &AppConfig,
    new_cfg: &AppConfig,
) -> Result<Vec<String>, Vec<conduit_config_core::validation::ValidationError>> {
    let errors = crate::config::validate::validate(new_cfg);
    let (warnings, hard_errors) = crate::config::validate::partition_by_severity(errors);
    if !hard_errors.is_empty() {
        return Err(hard_errors);
    }
    let cold = crate::admin::api::detect_cold_changes(current, new_cfg);
    if !cold.is_empty() {
        return Err(cold
            .into_iter()
            .map(|path| conduit_config_core::validation::ValidationError {
                path,
                message: "cold field changed — restart required".to_owned(),
                severity: conduit_config_core::validation::Severity::Error,
            })
            .collect());
    }
    let mut messages: Vec<String> = warnings
        .iter()
        .map(|w| format!("config: {}: {}", w.path, w.message))
        .collect();
    messages.extend(crate::config::validate::feature_warnings(new_cfg));
    messages
        .extend(crate::config::rate_limit_scan::redis_rate_limit_change_warning(current, new_cfg));
    Ok(messages)
}

/// Spawn a background thread that hot-swaps config from a live provider channel.
pub(super) fn spawn_config_update_watcher(
    mut rx: tokio::sync::mpsc::Receiver<AppConfig>,
    state: Arc<AppState>,
) {
    std::thread::Builder::new()
        .name("config-update-watcher".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime for config-update-watcher");
            rt.block_on(async move {
                while let Some(new_cfg) = rx.recv().await {
                    tracing::info!(
                        sites = new_cfg.sites.len(),
                        "live config update received — hot-swapping"
                    );
                    let warnings = match check_config_update(&state.config.load(), &new_cfg) {
                        Ok(warnings) => warnings,
                        Err(hard_errors) => {
                            for e in &hard_errors {
                                tracing::error!("config error at {}: {}", e.path, e.message);
                            }
                            tracing::error!(
                                "live config update rejected — keeping the currently-serving config"
                            );
                            continue;
                        }
                    };
                    for w in &warnings {
                        tracing::warn!("{w}");
                    }
                    // Connect any Redis-backed proxy cache URL this update
                    // introduced (issue #330) before the swap below -- same
                    // reasoning as the admin API's /reload handler.
                    #[cfg(all(feature = "cache", feature = "redis"))]
                    conduit_runtime::proxy::cache_redis::connect_all(&new_cfg).await;
                    state.config.store(Arc::new(new_cfg));
                }
                tracing::warn!("config update channel closed; live updates stopped");
            });
        })
        .expect("failed to spawn config-update-watcher thread");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::SiteConfig;

    // ── check_config_update (issue #492) ────────────────────────────────────

    /// Two sites sharing a host:port is a real, existing hard validation error
    /// (`validate_no_duplicate_host_port`) — used here only as a convenient way to
    /// produce one, not as new coverage of that rule itself.
    fn duplicate_host_port_config() -> AppConfig {
        AppConfig {
            sites: vec![
                SiteConfig {
                    host: Some("a.example.com".to_owned()),
                    port: Some(8080),
                    ..Default::default()
                },
                SiteConfig {
                    host: Some("a.example.com".to_owned()),
                    port: Some(8080),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn check_config_update_accepts_a_valid_config() {
        let cfg = AppConfig {
            sites: vec![SiteConfig {
                port: Some(8080),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(
            check_config_update(&cfg, &cfg).is_ok(),
            "a config with no validation errors must be accepted"
        );
    }

    #[test]
    fn check_config_update_rejects_a_config_with_hard_errors() {
        let cfg = duplicate_host_port_config();
        let result = check_config_update(&AppConfig::default(), &cfg);
        assert!(
            result.is_err(),
            "duplicate host:port must be rejected, not silently swapped in: {result:?}"
        );
    }

    #[test]
    fn check_config_update_hard_error_message_names_the_real_problem() {
        // Negative control for the rejection above: the returned errors must actually be
        // ABOUT the duplicate host:port, not just any non-empty error list — otherwise this
        // test would also pass for a config rejected for the wrong reason.
        let cfg = duplicate_host_port_config();
        let Err(hard_errors) = check_config_update(&AppConfig::default(), &cfg) else {
            panic!("expected a hard-error rejection");
        };
        assert!(
            hard_errors.iter().any(|e| e.message.contains("host")
                || e.message.contains("port")
                || e.message.contains("duplicate")),
            "hard errors should describe the duplicate host:port problem: {hard_errors:?}"
        );
    }

    // ── cold fields (issue #494) ────────────────────────────────────────────

    fn one_site(port: u16) -> AppConfig {
        AppConfig {
            sites: vec![SiteConfig {
                port: Some(port),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn check_config_update_rejects_a_cold_port_change() {
        let Err(errors) = check_config_update(&one_site(8080), &one_site(9090)) else {
            panic!("a port change needs a restart and must be rejected");
        };
        assert!(
            errors.iter().any(|e| e.path == "sites[0].port"),
            "the rejection must name the cold field: {errors:?}"
        );
    }

    #[test]
    fn check_config_update_accepts_a_hot_only_change() {
        let mut new = one_site(8080);
        new.sites[0].host = Some("renamed.example.com".to_owned());
        assert!(
            check_config_update(&one_site(8080), &new).is_ok(),
            "a host change is hot and must be swapped in"
        );
    }
}
