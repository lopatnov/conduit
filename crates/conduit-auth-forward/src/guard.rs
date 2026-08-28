use async_trait::async_trait;
use conduit_core::filter::chain::{FilterContext, FilterOutcome, RequestFilter};
use conduit_core::filter::path::is_path_skipped;
use conduit_core::handler::response;
use pingora_core::Result;
use pingora_proxy::Session;

use crate::config::ForwardAuthConfig;

/// Forward Auth guard — delegates authentication/authorization to an external service.
///
/// Sends the incoming request (filtered headers) to the configured auth URL.
/// - **2xx** → auth passed; headers listed in `responseHeaders` are injected
///   into the upstream request so the upstream receives user identity/role info.
/// - **4xx / 5xx** → auth denied; the auth service status is returned to the
///   client immediately.
///
/// Uses a process-wide `reqwest::Client` with a connection pool so that
/// hot-path requests don't pay TCP setup overhead.
pub struct ForwardAuthGuard {
    pub cfg: ForwardAuthConfig,
    pub path: String,
}

/// Process-wide reqwest client for forward-auth.
///
/// Uses separate `connect_timeout` (TCP SYN + TLS handshake) and overall
/// `timeout` (from connect to last body byte) so that both hung TCP
/// connections AND slow auth servers are bounded.
fn forward_auth_client() -> &'static reqwest::Client {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(3)) // TCP+TLS max
            .timeout(std::time::Duration::from_secs(10)) // total request max
            .build()
            .unwrap_or_default()
    })
}

#[async_trait]
impl RequestFilter for ForwardAuthGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        // Bypass for configured skip paths.
        if let Some(skip) = &self.cfg.skip_paths {
            if is_path_skipped(Some(skip.as_slice()), &self.path) {
                return Ok(FilterOutcome::Continue);
            }
        }

        let auth_url = &self.cfg.url;
        let timeout_ms = self.cfg.timeout_ms.unwrap_or(5000);
        let client = forward_auth_client();

        // Build the subrequest.
        let method = ctx.session.req_header().method.as_str();
        let uri = ctx
            .session
            .req_header()
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");
        let client_ip = ctx
            .session
            .client_addr()
            .and_then(|a| a.as_inet())
            .map(|a| a.ip().to_string())
            .unwrap_or_default();

        let mut req = client
            .get(auth_url)
            .timeout(std::time::Duration::from_millis(timeout_ms))
            .header("X-Forwarded-Method", method)
            .header("X-Forwarded-Uri", uri)
            .header("X-Forwarded-For", &client_ip);

        // Forward specific request headers if configured.
        if let Some(fwd_hdrs) = &self.cfg.request_headers {
            req = forward_auth_add_headers(req, fwd_hdrs, ctx.session);
        }

        // Make the subrequest.
        let auth_resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(url = %auth_url, error = %e, "forward-auth service unreachable");
                // Fail closed: treat unreachable auth service as 401.
                response::write_denied(ctx.session, None, ctx.extra_headers).await?;
                ctx.inflight
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                return Ok(FilterOutcome::Handled);
            }
        };

        let status = auth_resp.status();
        if status.is_success() {
            // Inject auth service response headers into the upstream request.
            if let Some(copy_hdrs) = &self.cfg.response_headers {
                forward_auth_inject_response_headers(&auth_resp, copy_hdrs, ctx.session);
            }
            Ok(FilterOutcome::Continue)
        } else {
            let status_code = status.as_u16();
            let body = bytes::Bytes::from_static(if status_code == 403 {
                b"Forbidden"
            } else {
                b"Unauthorized"
            });
            response::write_response(
                ctx.session,
                status_code,
                "text/plain",
                body,
                ctx.extra_headers,
            )
            .await?;
            ctx.inflight
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            Ok(FilterOutcome::Handled)
        }
    }
}

/// Add configured request headers to a forward-auth subrequest.
fn forward_auth_add_headers(
    mut req: reqwest::RequestBuilder,
    fwd_hdrs: &[String],
    session: &Session,
) -> reqwest::RequestBuilder {
    for name in fwd_hdrs {
        if let Some(val) = session.req_header().headers.get(name.as_str()) {
            if let Ok(v) = val.to_str() {
                req = req.header(name.as_str(), v);
            }
        }
    }
    req
}

/// Copy configured response headers from a forward-auth response into the session.
fn forward_auth_inject_response_headers(
    auth_resp: &reqwest::Response,
    copy_hdrs: &[String],
    session: &mut Session,
) {
    let to_inject: Vec<(String, String)> = copy_hdrs
        .iter()
        .filter_map(|name| {
            auth_resp
                .headers()
                .get(name.as_str())
                .and_then(|val| val.to_str().ok())
                .map(|v| (name.clone(), v.to_owned()))
        })
        .collect();
    for (name, value) in to_inject {
        let _ = session.req_header_mut().insert_header(name, value);
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// Build a real [`pingora_proxy::Session`] with a parsed GET request
    /// already read off the wire, so guards that touch `ctx.session`
    /// (`req_header()`, `write_response`, …) can be exercised as real unit
    /// tests instead of only via `tests/*.rs` integration tests (which
    /// `cargo llvm-cov --lib` never instruments — see #248). Mirrors
    /// `conduit-faults`/`conduit-auth-jwt`'s own `guard.rs` test helper.
    async fn session_with_request(raw: &[u8]) -> (pingora_proxy::Session, tokio::io::DuplexStream) {
        let (server_side, mut client_side) = tokio::io::duplex(4096);
        client_side.write_all(raw).await.unwrap();

        let stream: pingora_core::protocols::Stream = Box::new(server_side);
        let mut session = pingora_proxy::Session::new_h1(stream);
        session
            .as_downstream_mut()
            .read_request()
            .await
            .expect("read_request");

        (session, client_side)
    }

    // ── forward_auth_client ───────────────────────────────────────────────────

    #[test]
    fn forward_auth_client_returns_same_singleton() {
        let c1 = forward_auth_client();
        let c2 = forward_auth_client();
        // Both calls must return the same static reference.
        assert!(
            std::ptr::eq(c1 as *const _, c2 as *const _),
            "forward_auth_client must be a singleton"
        );
    }

    // ── ForwardAuthGuard::apply ───────────────────────────────────────────────

    #[tokio::test]
    async fn apply_skips_configured_skip_path() {
        let (mut session, _client_sock) =
            session_with_request(b"GET /health HTTP/1.1\r\nHost: test\r\n\r\n").await;
        let guard = ForwardAuthGuard {
            cfg: ForwardAuthConfig {
                url: "http://127.0.0.1:1".to_owned(), // unreachable — must never be hit
                skip_paths: Some(vec!["/health".to_owned()]),
                ..Default::default()
            },
            path: "/health".to_owned(),
        };
        let inflight = std::sync::atomic::AtomicUsize::new(1);
        let mut ctx = FilterContext {
            session: &mut session,
            extra_headers: &[],
            inflight: &inflight,
        };

        let outcome = guard.apply(&mut ctx).await.expect("apply");
        assert!(matches!(outcome, FilterOutcome::Continue));
        assert_eq!(
            inflight.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "skip-path bypass must not touch inflight"
        );
    }

    #[tokio::test]
    async fn apply_denies_with_401_when_auth_service_unreachable() {
        let (mut session, mut client_sock) =
            session_with_request(b"GET /api HTTP/1.1\r\nHost: test\r\n\r\n").await;
        let guard = ForwardAuthGuard {
            cfg: ForwardAuthConfig {
                // Port 1 is a reserved/unlikely-to-be-listening port — the
                // connection attempt fails fast (connection refused) rather
                // than genuinely timing out, keeping this test fast.
                url: "http://127.0.0.1:1/auth".to_owned(),
                timeout_ms: Some(500),
                ..Default::default()
            },
            path: "/api".to_owned(),
        };
        let inflight = std::sync::atomic::AtomicUsize::new(1);
        let mut ctx = FilterContext {
            session: &mut session,
            extra_headers: &[],
            inflight: &inflight,
        };

        let outcome = guard.apply(&mut ctx).await.expect("apply");
        assert!(matches!(outcome, FilterOutcome::Handled));
        assert_eq!(inflight.load(std::sync::atomic::Ordering::Relaxed), 0);

        let mut buf = vec![0u8; 512];
        let n = client_sock.read(&mut buf).await.expect("read response");
        let response = String::from_utf8_lossy(&buf[..n]);
        assert!(response.starts_with("HTTP/1.1 401"), "got: {response}");
    }
}
