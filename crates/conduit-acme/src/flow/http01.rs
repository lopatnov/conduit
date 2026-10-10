//! HTTP-01 challenge serving: the temporary challenge server bound during an
//! order, and the per-port lock that serialises orders sharing a port.
use std::sync::{Arc, OnceLock};

use dashmap::DashMap;

/// How long to wait for the HTTP-01 challenge server to shut down gracefully
/// before forcibly aborting it (issue #352).
///
/// axum/hyper's graceful shutdown waits for any request that has already
/// been fully parsed and dispatched to a still-running handler before
/// closing that connection -- confirmed directly against this crate's
/// pinned axum/hyper versions (see the `challenge_server_shutdown_hangs_*`
/// test below), which also ruled out an earlier, less precise version of
/// this claim: a connection with merely *incomplete* request headers is
/// NOT treated as active and does not block shutdown on its own. A
/// pathologically slow handler invocation, or a peer slow enough to delay
/// completing an otherwise-dispatched request/response cycle, could
/// otherwise keep this task -- and the port/`_port_guard` it holds --
/// alive past the intended shutdown.
pub(super) const CHALLENGE_SHUTDOWN_TIMEOUT_SECS: u64 = 10;

// ── Per-port serialization lock ───────────────────────────────────────────────

/// One `Mutex` per HTTP-01 challenge port, ensuring that concurrent
/// `obtain_certificate` calls for different domains never race to bind the
/// same port.
static HTTP01_PORT_LOCKS: OnceLock<DashMap<u16, Arc<tokio::sync::Mutex<()>>>> = OnceLock::new();

pub(super) fn http01_port_lock(port: u16) -> Arc<tokio::sync::Mutex<()>> {
    HTTP01_PORT_LOCKS
        .get_or_init(DashMap::new)
        .entry(port)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Where the HTTP-01 challenge tokens of an order are served from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeSource {
    /// Bind a temporary challenge server on this local port for the duration
    /// of the order. Right at startup, before any listener exists.
    Bind(u16),
    /// A running Conduit listener (the HTTP→HTTPS redirect service or a site
    /// on that port) already answers `/.well-known/acme-challenge/{token}`
    /// from the shared challenge map, so binding the port again would fail
    /// with `EADDRINUSE`. Tokens are only inserted into the map.
    Shared,
}

/// Serve ACME HTTP-01 challenges on the pre-bound `listener`.
///
/// Responds to `GET /.well-known/acme-challenge/{token}` with the
/// corresponding key-authorization from `challenges`.
/// Shuts down when `stop_rx` fires.
///
/// The caller must bind the [`TcpListener`] *before* spawning this task so
/// that port-bind failures are surfaced as ACME errors rather than being
/// silently swallowed in the background.
pub(super) async fn run_challenge_server(
    listener: tokio::net::TcpListener,
    challenges: Arc<DashMap<String, String>>,
    stop_rx: tokio::sync::oneshot::Receiver<()>,
) {
    use axum::extract::{Path, State};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;

    async fn challenge_handler(
        Path(token): Path<String>,
        State(store): State<Arc<DashMap<String, String>>>,
    ) -> impl IntoResponse {
        match store.get(&token) {
            Some(key_auth) => (
                axum::http::StatusCode::OK,
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                key_auth.clone(),
            )
                .into_response(),
            None => axum::http::StatusCode::NOT_FOUND.into_response(),
        }
    }

    let app = Router::new()
        .route(
            "/.well-known/acme-challenge/{token}",
            get(challenge_handler),
        )
        .with_state(challenges);

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            stop_rx.await.ok();
        })
        .await
    {
        tracing::error!(error = %e, "ACME HTTP-01 challenge server accept loop failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Mirrors `run_challenge_server`'s exact shape (bind -> axum::serve
    /// with a `stop_rx`-driven `with_graceful_shutdown`) but with a
    /// deliberately slow handler, so the "active connection blocks
    /// graceful shutdown" mechanism can be exercised deterministically.
    ///
    /// An earlier version of these tests tried to trigger this by sending a
    /// request with incomplete headers (no trailing blank line) against the
    /// *real* `run_challenge_server`. That does NOT reproduce a hang on
    /// axum 0.8.9 / hyper 1.10 (confirmed empirically: shutdown completed
    /// well under 300ms) -- graceful shutdown only waits for a request that
    /// has already been fully parsed and dispatched to a still-running
    /// handler, not one still being read. Confirmed the real mechanism
    /// instead with a standalone probe: a genuinely slow handler blocks
    /// shutdown for exactly as long as it keeps running (measured >500ms
    /// against a 5-second sleep), matching axum/hyper's documented
    /// behavior. This helper reproduces that confirmed mechanism instead of
    /// the unconfirmed one.
    async fn spawn_slow_challenge_like_server(
        handler_delay: Duration,
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        use axum::routing::get;
        use axum::Router;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

        let app = Router::new().route(
            "/slow",
            get(move || async move {
                tokio::time::sleep(handler_delay).await;
                "done"
            }),
        );
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    stop_rx.await.ok();
                })
                .await;
        });
        (addr, task, stop_tx)
    }

    /// Returns the connected stream so the caller can keep it alive -- if
    /// dropped, the connection closes and hyper has nothing left to wait
    /// for, defeating the whole point of this helper (confirmed as a real
    /// bug in an earlier draft: the connection was dropped at the end of
    /// this function, and the "active connection" premise silently stopped
    /// applying because there was no longer any connection to be active).
    async fn connect_and_send_get(addr: std::net::SocketAddr, path: &str) -> tokio::net::TcpStream {
        use tokio::io::AsyncWriteExt;
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        // Give the request a moment to actually be parsed and dispatched to
        // the (slow) handler before the caller signals shutdown.
        tokio::time::sleep(Duration::from_millis(50)).await;
        client
    }

    #[tokio::test]
    async fn challenge_server_shutdown_hangs_on_active_connection_without_bound() {
        // Reproduces issue #352's premise: without a bounded wait, an
        // active (in-flight, not-yet-returned handler) connection keeps
        // graceful shutdown from ever completing. This is what
        // obtain_certificate()'s CHALLENGE_SHUTDOWN_TIMEOUT_SECS wrapper
        // guards against.
        let (addr, server_task, stop_tx) =
            spawn_slow_challenge_like_server(Duration::from_secs(5)).await;
        let _client = connect_and_send_get(addr, "/slow").await;
        stop_tx.send(()).unwrap();

        let result = tokio::time::timeout(Duration::from_millis(300), server_task).await;
        assert!(
            result.is_err(),
            "expected the shutdown to still be blocked by the active connection"
        );
    }

    #[tokio::test]
    async fn challenge_server_forcibly_aborted_after_timeout_releases_the_port() {
        // Verifies the actual fix: after the bounded wait elapses, calling
        // AbortHandle::abort() genuinely reclaims the port. Dropping the
        // JoinHandle alone would NOT be enough -- a tokio spawned task keeps
        // running detached when its JoinHandle is simply dropped, so this
        // proves the explicit abort_handle().abort() call is load-bearing,
        // not redundant.
        let (addr, server_task, stop_tx) =
            spawn_slow_challenge_like_server(Duration::from_secs(5)).await;
        let abort_handle = server_task.abort_handle();
        let _client = connect_and_send_get(addr, "/slow").await;
        stop_tx.send(()).unwrap();

        let result = tokio::time::timeout(Duration::from_millis(300), server_task).await;
        assert!(
            result.is_err(),
            "expected shutdown to be blocked by the active connection"
        );

        abort_handle.abort();
        // Task cancellation happens at the next yield point, not
        // synchronously with abort() -- give it a moment to actually land.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let rebind = tokio::net::TcpListener::bind(addr).await;
        assert!(
            rebind.is_ok(),
            "port should be free after the aborted task is reclaimed: {:?}",
            rebind.err()
        );
    }

    /// Sends a raw HTTP/1.1 GET and returns (status_code, body) -- avoids
    /// pulling in an HTTP client dependency this crate doesn't otherwise
    /// need, matching this codebase's established raw-`TcpStream` testing
    /// idiom (see `.claude/skills/testing/SKILL.md`).
    async fn raw_get(addr: std::net::SocketAddr, path: &str) -> (u16, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        let response = String::from_utf8_lossy(&raw);
        let mut parts = response.splitn(2, "\r\n\r\n");
        let head = parts.next().unwrap_or_default();
        let body = parts.next().unwrap_or_default().to_string();
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .expect("response must have a parseable status line");
        (status, body)
    }

    #[tokio::test]
    async fn challenge_server_route_serves_a_registered_token() {
        // Direct end-to-end test of run_challenge_server's real route
        // registration -- found necessary because none of the other tests
        // in this file exercised it directly. It previously used axum
        // 0.6/0.7-style ":token" path-parameter syntax, which axum 0.8
        // rejects: `Router::route()` panics immediately at registration
        // time with "Path segments must not start with `:`" -- meaning
        // *every* real HTTP-01 challenge attempt would have failed before
        // the challenge server ever started accepting connections. Caught
        // by chance while writing the shutdown-timeout tests above (they
        // originally called run_challenge_server directly too, and hit
        // this panic before being rewritten to use a synthetic app
        // instead). Negative control: reverting the route string from
        // "{token}" back to ":token" reproduces the exact panic above.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let challenges = Arc::new(DashMap::new());
        challenges.insert(
            "my-token".to_string(),
            "expected-key-authorization".to_string(),
        );
        let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let _server_task = tokio::spawn(run_challenge_server(listener, challenges, stop_rx));

        // Give the accept loop a moment to actually start listening.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let (status, body) = raw_get(addr, "/.well-known/acme-challenge/my-token").await;
        assert_eq!(status, 200);
        assert_eq!(body, "expected-key-authorization");
    }

    #[tokio::test]
    async fn challenge_server_route_404s_for_an_unregistered_token() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let challenges = Arc::new(DashMap::new());
        let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let _server_task = tokio::spawn(run_challenge_server(listener, challenges, stop_rx));

        tokio::time::sleep(Duration::from_millis(20)).await;

        let (status, _body) = raw_get(addr, "/.well-known/acme-challenge/no-such-token").await;
        assert_eq!(status, 404);
    }
}
