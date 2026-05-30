//! Browser hot-reload support.
//!
//! Exposes two endpoints:
//! - `/__hot-reload__` — a Server-Sent Events stream. Browsers connect and
//!   receive a `data: reload` event whenever a watched file changes.
//! - `/__hot-reload__/client.js` — the JavaScript snippet that connects to
//!   the SSE stream and triggers `location.reload()` on each event.
//!
//! The file watcher ([`run_file_watcher`]) is started in the Admin API
//! background service and sends `()` on the shared broadcast channel whenever
//! a debounced change event arrives.

use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use notify::Watcher;
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::Session;
use tokio::sync::broadcast;

use super::response::write_response;

// ── Client JavaScript ─────────────────────────────────────────────────────────

/// Minified client-side snippet served at `/__hot-reload__/client.js`.
///
/// Connect to the SSE endpoint, reload on `reload` events, reconnect on errors.
const CLIENT_JS: &str = r#"(function(){
  var url='/__hot-reload__';
  function connect(){
    var es=new EventSource(url);
    es.onmessage=function(e){if(e.data==='reload'){location.reload();}};
    es.onerror=function(){es.close();setTimeout(connect,3000);};
  }
  connect();
})();
"#;

// ── Handlers ──────────────────────────────────────────────────────────────────

/// Serve the client-side JavaScript at `/__hot-reload__/client.js`.
pub async fn handle_client_js(session: &mut Session, extra: &[(String, String)]) -> Result<()> {
    write_response(
        session,
        200,
        "application/javascript; charset=utf-8",
        Bytes::from_static(CLIENT_JS.as_bytes()),
        extra,
    )
    .await
}

/// Serve the Server-Sent Events stream at `/__hot-reload__`.
///
/// Writes SSE headers, sends an initial `: connected` comment, then loops:
/// - On a broadcast signal → `data: reload\n\n`
/// - Every 25 s → `: ping\n\n` keepalive comment
/// - On client disconnect or channel closed → end gracefully
pub async fn handle_sse(
    session: &mut Session,
    mut rx: broadcast::Receiver<()>,
    extra: &[(String, String)],
) -> Result<()> {
    // SSE response headers — no Content-Length (streaming, chunked).
    let mut resp = ResponseHeader::build(200, Some(4 + extra.len()))?;
    resp.insert_header("content-type", "text/event-stream; charset=utf-8")?;
    resp.insert_header("cache-control", "no-cache")?;
    resp.insert_header("connection", "keep-alive")?;
    // Disable proxy buffering (nginx / similar).
    resp.insert_header("x-accel-buffering", "no")?;
    for (k, v) in extra {
        resp.insert_header(k.clone(), v.clone())?;
    }
    session.write_response_header(Box::new(resp), false).await?;

    // Confirm connection to the browser.
    if session
        .write_response_body(Some(Bytes::from_static(b": connected\n\n")), false)
        .await
        .is_err()
    {
        return Ok(());
    }

    // Event loop.
    loop {
        use tokio::sync::broadcast::error::RecvError;

        tokio::select! {
            res = rx.recv() => {
                match res {
                    Ok(()) | Err(RecvError::Lagged(_)) => {
                        // Lagged means we missed some signals — a single reload is still correct.
                        if session
                            .write_response_body(Some(Bytes::from_static(b"data: reload\n\n")), false)
                            .await
                            .is_err()
                        {
                            break; // client disconnected
                        }
                    }
                    Err(RecvError::Closed) => break, // watcher stopped
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(25)) => {
                // Keepalive comment — prevents proxies and browsers from timing out.
                if session
                    .write_response_body(Some(Bytes::from_static(b": ping\n\n")), false)
                    .await
                    .is_err()
                {
                    break; // client disconnected
                }
            }
        }
    }

    // Graceful end-of-stream (client may have already closed).
    let _ = session.write_response_body(None, true).await;
    Ok(())
}

// ── File watcher ──────────────────────────────────────────────────────────────

/// Collect all directories to watch and the union of configured extensions
/// across every site that has `hotReload` enabled.
///
/// Returns `None` when no site has `hotReload` configured.
pub fn build_watch_config(
    config: &crate::config::schema::AppConfig,
) -> Option<(Vec<PathBuf>, Option<Vec<String>>)> {
    use crate::config::schema::{HotReloadConfig, StaticConfig};

    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut all_exts: Vec<String> = Vec::new();
    let mut any_all_exts = false; // true if any site sets no extension filter

    for site in &config.sites {
        let Some(hr) = &site.hot_reload else { continue };
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
        if let Some(static_cfg) = &site.static_files {
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
