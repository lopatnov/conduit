//! File watcher for browser hot-reload.
//!
//! Moved from `src/handler/hot_reload.rs` (issue #114/#140) — see this
//! crate's `src/lib.rs` doc comment.

use std::path::PathBuf;
use std::time::Duration;

use notify::Watcher;
use tokio::sync::broadcast;

use crate::config::HotReloadConfig;

/// Collect all directories to watch and the union of configured extensions
/// across every site that has `hotReload` enabled.
///
/// Takes one `(hot_reload, static_files)` pair per site rather than the
/// whole `AppConfig` — `AppConfig`/`SiteConfig` aren't extracted out of the
/// root crate yet, so this crate can't name them directly (see this crate's
/// `Cargo.toml` comment on the `lopatnov-conduit-static` dependency). The
/// root crate's own caller maps `config.sites.iter().map(|s|
/// (s.hot_reload.as_ref(), s.static_files.as_ref()))` into this shape.
///
/// Returns `None` when no site has `hotReload` configured.
pub fn build_watch_config<'a, I>(sites: I) -> Option<(Vec<PathBuf>, Option<Vec<String>>)>
where
    I: IntoIterator<
        Item = (
            Option<&'a HotReloadConfig>,
            Option<&'a conduit_static::StaticConfig>,
        ),
    >,
{
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut all_exts: Vec<String> = Vec::new();
    let mut any_all_exts = false; // true if any site sets no extension filter

    for (hot_reload, static_files) in sites {
        let Some(hr) = hot_reload else { continue };
        let exts = match hr {
            HotReloadConfig::Enabled(true) => {
                any_all_exts = true;
                None
            }
            HotReloadConfig::Enabled(false) => continue,
            HotReloadConfig::Options(opts) => opts.extensions.clone(),
        };

        if let Some(ref e) = exts {
            all_exts.extend(e.iter().cloned());
        } else {
            any_all_exts = true;
        }

        // Collect the site's static directories as watch roots.
        if let Some(static_cfg) = static_files {
            use conduit_static::StaticConfig;
            match static_cfg {
                StaticConfig::Single(path) => dirs.push(PathBuf::from(path)),
                StaticConfig::Multi(paths) => {
                    dirs.extend(paths.iter().map(PathBuf::from));
                }
                StaticConfig::Mapped(map) => {
                    dirs.extend(map.values().map(PathBuf::from));
                }
            }
        }
    }

    if dirs.is_empty() {
        return None;
    }

    // Deduplicate directories.
    dirs.sort();
    dirs.dedup();

    let extensions = if any_all_exts || all_exts.is_empty() {
        None // watch all files (or no filter needed)
    } else {
        all_exts.sort();
        all_exts.dedup();
        Some(all_exts)
    };

    Some((dirs, extensions))
}

/// Run the file watcher task.
///
/// Watches `dirs` for write, create, and remove events (filtered by
/// `extensions` when set).  On each debounced batch (200 ms quiet period),
/// sends `()` on `reload_tx`.
///
/// Runs indefinitely even when there are no active SSE subscribers: send
/// errors on `reload_tx` are intentionally ignored so that events are simply
/// dropped when nobody is listening, and the watcher remains ready to deliver
/// the next event when a subscriber reconnects.  The task only exits on an
/// unrecoverable watcher error.
pub async fn run_file_watcher(
    dirs: Vec<PathBuf>,
    extensions: Option<Vec<String>>,
    reload_tx: broadcast::Sender<()>,
) {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<()>(32);

    // The notify callback is called from a background thread — use blocking_send.
    let exts_clone = extensions.clone();
    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            if event_passes_filter(&event, exts_clone.as_deref()) {
                let _ = event_tx.blocking_send(());
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("hot-reload: failed to create file watcher: {e}");
                return;
            }
        };

    let mut watched = 0usize;
    for dir in &dirs {
        match watcher.watch(dir, notify::RecursiveMode::Recursive) {
            Ok(()) => {
                watched += 1;
                tracing::debug!(path = %dir.display(), "hot-reload: watching directory");
            }
            Err(e) => {
                tracing::warn!(path = %dir.display(), "hot-reload: cannot watch directory: {e}");
            }
        }
    }

    if watched == 0 {
        tracing::warn!("hot-reload: no directories could be watched — watcher inactive");
        return;
    }

    let ext_msg = extensions
        .as_deref()
        .map(|e| e.join(", "))
        .unwrap_or_else(|| "*".to_owned());
    tracing::info!(
        dirs = watched,
        extensions = %ext_msg,
        "hot-reload: file watcher active"
    );

    // Debounce: wait for first event, drain additional ones within 200 ms,
    // then broadcast a single reload signal.
    loop {
        // Wait for at least one event.
        if event_rx.recv().await.is_none() {
            break; // mpsc channel closed (watcher dropped)
        }

        // Drain the 200 ms quiet window.
        while let Ok(Some(())) =
            tokio::time::timeout(Duration::from_millis(200), event_rx.recv()).await
        {
            // more events — keep draining
        }

        // Ignore send errors: no active SSE subscribers means the signal is
        // simply dropped, but the watcher must keep running so it can deliver
        // the next event when a subscriber reconnects.
        let _ = reload_tx.send(());
    }

    // `watcher` is dropped here, which stops the background notify thread.
}

/// Return `true` when the notify event should trigger a browser reload.
///
/// Accepts Modify, Create, and Remove events.  When `exts` is `Some`, the
/// event's file paths must include at least one file whose extension is in
/// the allowlist (case-insensitive, leading dot optional).
fn event_passes_filter(event: &notify::Event, exts: Option<&[String]>) -> bool {
    use notify::EventKind::*;
    if !matches!(event.kind, Modify(_) | Create(_) | Remove(_)) {
        return false;
    }
    exts.is_none_or(|exts| {
        event.paths.iter().any(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| {
                    exts.iter()
                        .any(|x| x.trim_start_matches('.').eq_ignore_ascii_case(e))
                })
                .unwrap_or(false)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HotReloadOptions;
    use conduit_static::StaticConfig;

    // ── build_watch_config ────────────────────────────────────────────────────

    #[test]
    fn build_watch_config_no_hot_reload_returns_none() {
        let sites: Vec<(Option<&HotReloadConfig>, Option<&StaticConfig>)> = vec![(None, None)];
        assert!(build_watch_config(sites).is_none());
    }

    #[test]
    fn build_watch_config_hot_reload_false_returns_none() {
        let hr = HotReloadConfig::Enabled(false);
        let sites: Vec<(Option<&HotReloadConfig>, Option<&StaticConfig>)> = vec![(Some(&hr), None)];
        assert!(build_watch_config(sites).is_none());
    }

    #[test]
    fn build_watch_config_no_static_dir_returns_none() {
        // hotReload=true but no static directory — nothing to watch.
        let hr = HotReloadConfig::Enabled(true);
        let sites: Vec<(Option<&HotReloadConfig>, Option<&StaticConfig>)> = vec![(Some(&hr), None)];
        assert!(build_watch_config(sites).is_none());
    }

    #[test]
    fn build_watch_config_hot_reload_true_returns_dir() {
        let hr = HotReloadConfig::Enabled(true);
        let static_cfg = StaticConfig::Single("./dist".to_owned());
        let sites: Vec<(Option<&HotReloadConfig>, Option<&StaticConfig>)> =
            vec![(Some(&hr), Some(&static_cfg))];
        let result = build_watch_config(sites);
        assert!(
            result.is_some(),
            "hotReload=true with static dir must return Some"
        );
        let (dirs, exts) = result.unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], std::path::PathBuf::from("./dist"));
        assert!(exts.is_none(), "no extension filter → watch all files");
    }

    #[test]
    fn build_watch_config_with_extension_filter() {
        let hr = HotReloadConfig::Options(HotReloadOptions {
            extensions: Some(vec![
                ".html".to_owned(),
                ".css".to_owned(),
                ".js".to_owned(),
            ]),
        });
        let static_cfg = StaticConfig::Single("./dist".to_owned());
        let sites: Vec<(Option<&HotReloadConfig>, Option<&StaticConfig>)> =
            vec![(Some(&hr), Some(&static_cfg))];
        let result = build_watch_config(sites);
        assert!(result.is_some());
        let (dirs, exts) = result.unwrap();
        assert_eq!(dirs.len(), 1);
        let exts = exts.expect("extension filter must be present");
        assert!(exts.contains(&".css".to_owned()) || exts.iter().any(|e| e.contains("css")));
    }

    #[test]
    fn build_watch_config_deduplicates_dirs() {
        // Two sites with the same static dir → only one watch dir.
        let hr = HotReloadConfig::Enabled(true);
        let static_cfg = StaticConfig::Single("./dist".to_owned());
        let sites: Vec<(Option<&HotReloadConfig>, Option<&StaticConfig>)> = vec![
            (Some(&hr), Some(&static_cfg)),
            (Some(&hr), Some(&static_cfg)),
        ];
        let result = build_watch_config(sites);
        assert!(result.is_some());
        let (dirs, _) = result.unwrap();
        assert_eq!(dirs.len(), 1, "duplicate dirs must be deduped: {dirs:?}");
    }

    // ── event_passes_filter ───────────────────────────────────────────────────

    fn make_event(kind: notify::EventKind, paths: Vec<std::path::PathBuf>) -> notify::Event {
        notify::Event {
            kind,
            paths,
            attrs: notify::event::EventAttributes::default(),
        }
    }

    #[test]
    fn event_passes_filter_modify_no_ext_filter() {
        let evt = make_event(
            notify::EventKind::Modify(notify::event::ModifyKind::Any),
            vec![PathBuf::from("index.html")],
        );
        assert!(
            event_passes_filter(&evt, None),
            "no filter → all events pass"
        );
    }

    #[test]
    fn event_passes_filter_create_no_ext_filter() {
        let evt = make_event(
            notify::EventKind::Create(notify::event::CreateKind::File),
            vec![PathBuf::from("style.css")],
        );
        assert!(event_passes_filter(&evt, None));
    }

    #[test]
    fn event_passes_filter_access_event_rejected() {
        // Access events (reads) must not trigger hot-reload.
        let evt = make_event(
            notify::EventKind::Access(notify::event::AccessKind::Open(
                notify::event::AccessMode::Any,
            )),
            vec![PathBuf::from("index.html")],
        );
        assert!(
            !event_passes_filter(&evt, None),
            "Access events must be ignored"
        );
    }

    #[test]
    fn event_passes_filter_matching_extension_passes() {
        let evt = make_event(
            notify::EventKind::Modify(notify::event::ModifyKind::Any),
            vec![PathBuf::from("style.css")],
        );
        let exts = vec![".css".to_owned(), ".js".to_owned()];
        assert!(event_passes_filter(&evt, Some(&exts)));
    }

    #[test]
    fn event_passes_filter_non_matching_extension_blocked() {
        let evt = make_event(
            notify::EventKind::Modify(notify::event::ModifyKind::Any),
            vec![PathBuf::from("image.png")],
        );
        let exts = vec![".css".to_owned(), ".js".to_owned()];
        assert!(
            !event_passes_filter(&evt, Some(&exts)),
            "png not in filter → blocked"
        );
    }

    #[test]
    fn event_passes_filter_case_insensitive() {
        // Extension comparison is case-insensitive.
        let evt = make_event(
            notify::EventKind::Modify(notify::event::ModifyKind::Any),
            vec![PathBuf::from("STYLE.CSS")],
        );
        let exts = vec![".css".to_owned()];
        assert!(
            event_passes_filter(&evt, Some(&exts)),
            "case-insensitive match must pass"
        );
    }

    #[test]
    fn event_passes_filter_strip_leading_dot() {
        // Extension can be configured with or without a leading dot.
        let evt = make_event(
            notify::EventKind::Modify(notify::event::ModifyKind::Any),
            vec![PathBuf::from("app.js")],
        );
        let exts_no_dot = vec!["js".to_owned()]; // no leading dot
        assert!(
            event_passes_filter(&evt, Some(&exts_no_dot)),
            "extension without leading dot must still match"
        );
    }
}
