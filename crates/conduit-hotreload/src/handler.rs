//! Browser hot-reload request handlers.
//!
//! Moved from `src/handler/hot_reload.rs` (issue #114/#140) — see this
//! crate's `src/lib.rs` doc comment. Exposes two endpoints:
//! - `/__hot-reload__` — a Server-Sent Events stream. Browsers connect and
//!   receive a `data: reload` event whenever a watched file changes.
//! - `/__hot-reload__/client.js` — the JavaScript snippet that connects to
//!   the SSE stream and triggers `location.reload()` on each event.
//!
//! The file watcher ([`crate::watcher::run_file_watcher`]) is started in the
//! root crate's Admin API background service and sends `()` on the shared
//! broadcast channel whenever a debounced change event arrives.

use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use conduit_core::handler::response::write_response;
use conduit_core::handler::LocalHandlerImpl;
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::Session;
use tokio::sync::broadcast;

/// Handler struct for serving the hot-reload client JavaScript snippet.
pub struct HotReloadJsHandler {
    pub extra_headers: Vec<(String, String)>,
}

#[async_trait]
impl LocalHandlerImpl for HotReloadJsHandler {
    async fn handle(&mut self, session: &mut Session) -> Result<()> {
        handle_client_js(session, &self.extra_headers).await
    }
}

/// Handler struct for the Server-Sent Events hot-reload stream.
pub struct HotReloadSseHandler {
    pub extra_headers: Vec<(String, String)>,
    /// Wrapped in `Option` so the receiver can be moved out on the first
    /// (and only) call to `handle`.
    pub rx: Option<broadcast::Receiver<()>>,
}

#[async_trait]
impl LocalHandlerImpl for HotReloadSseHandler {
    async fn handle(&mut self, session: &mut Session) -> Result<()> {
        let rx = self
            .rx
            .take()
            .expect("HotReloadSseHandler::handle called twice");
        handle_sse(session, rx, &self.extra_headers).await
    }
}

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
