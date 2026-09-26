//! Upstream peer resolution: `upstream_peer` (the
//! [`pingora_proxy::ProxyHttp::upstream_peer`] trait-method body),
//! `resolve_peer_addr`, and `apply_peer_options`.
//!
//! Split out of the former monolithic `request_phase.rs` (issue #144 prep,
//! PR 1 of 2) -- pure code relocation, no behavioral change.
//!
//! **Feature gating (issue #144, PR 3):** an upstream peer is only ever
//! selected for a proxied upstream (`proxy`) or for the internal upload
//! loopback upstream (`upload` -- `UpstreamTarget::Upload` classifies as
//! `HandlerKind::Proxy` and therefore reaches [`upstream_peer`] too), so the
//! real implementation compiles when either is enabled. In a build with
//! neither, `service.rs`'s `ProxyHttp::upstream_peer` still needs a target,
//! so [`upstream_peer`] becomes a stub answering 404 -- the same plain
//! degradation every other disabled feature gets, never a 502/500 from a
//! request Pingora was told to proxy with nothing to proxy to. Retry
//! handling inside it is `proxy`-only (retry state is only populated by the
//! proxy routing resolvers).

#[cfg(any(feature = "proxy", feature = "upload"))]
use std::time::Duration;
#[cfg(any(feature = "proxy", feature = "upload"))]
use std::time::Instant;

use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::Result;

#[cfg(any(feature = "proxy", feature = "upload"))]
use crate::config::schema::{ConnectionPoolConfig, ProxyTimeout};
use crate::proxy::ctx::RequestCtx;
#[cfg(any(feature = "proxy", feature = "upload"))]
use crate::proxy::ctx::UpstreamTarget;
#[cfg(any(feature = "proxy", feature = "upload"))]
use crate::proxy::health::UpstreamRegistry;
#[cfg(any(feature = "proxy", feature = "upload"))]
use crate::proxy::request::dns::resolve_socket_addr;
#[cfg(feature = "proxy")]
use crate::proxy::request::retry::{apply_backoff, select_retry_target};
use crate::proxy::service::ConduitProxy;
#[cfg(feature = "proxy")]
use crate::proxy::upstream;

/// Body of [`pingora_proxy::ProxyHttp::upstream_peer`] for a build with
/// neither `proxy` nor `upload`: there is no upstream to select, so answer
/// 404 (Pingora's `fail_to_proxy` turns `ErrorType::HTTPStatus(404)` into a
/// plain 404 response). Unreachable in practice -- no routing path builds a
/// proxied target without one of those features -- but it keeps the trait
/// impl total instead of `#[cfg]`-ing the method away.
#[cfg(not(any(feature = "proxy", feature = "upload")))]
pub(crate) async fn upstream_peer(
    _proxy: &ConduitProxy,
    _ctx: &mut Option<RequestCtx>,
) -> Result<Box<HttpPeer>> {
    Err(pingora_core::Error::explain(
        pingora_core::ErrorType::HTTPStatus(404),
        "no upstream: neither the `proxy` nor the `upload` feature is compiled in",
    ))
}

/// Sleep for a retry attempt's configured backoff -- the `proxy` variant.
#[cfg(feature = "proxy")]
async fn apply_retry_backoff(req_ctx: &RequestCtx) {
    if let Some(ref retry) = req_ctx.proxy.retry {
        apply_backoff(retry).await;
    }
}

/// No-`proxy` variant: retry state is never populated without the proxy
/// routing resolvers, so there is never a backoff to apply.
#[cfg(all(feature = "upload", not(feature = "proxy")))]
async fn apply_retry_backoff(_req_ctx: &RequestCtx) {}

/// Body of [`pingora_proxy::ProxyHttp::upstream_peer`].
#[cfg(any(feature = "proxy", feature = "upload"))]
pub(crate) async fn upstream_peer(
    proxy: &ConduitProxy,
    ctx: &mut Option<RequestCtx>,
) -> Result<Box<HttpPeer>> {
    let req_ctx = ctx.as_mut().expect("ctx set in request_filter");

    apply_retry_backoff(req_ctx).await;

    // #47/#216: resolve_peer_addr() -> select_retry_target() owns choosing
    // this attempt's URL AND all proxy_upstream_url/upstream_conn_slot
    // bookkeeping for it (release the previous attempt's slot, forward-probe
    // for capacity, acquire the new one) so that logging(), the access log,
    // and EWMA tracking all reflect the *actual* upstream this attempt
    // targets -- see select_retry_target's doc comment for the full design
    // (#216 part 2 -- real per-attempt capacity admission, not just part
    // 1's leak fix).
    let (addr_str, tls, sni) = resolve_peer_addr(req_ctx, &proxy.state.upstream_health)?;

    // Derive fallback timeout from `limits.timeoutSecs` on the matched site.
    // Computed here (rather than only below, before `apply_peer_options`) so
    // the same effective connect deadline also bounds DNS resolution below —
    // otherwise a stalled resolver could hold a request open indefinitely
    // with no timeout at all (CodeRabbit finding on PR #227).
    let limits_timeout_secs = {
        let cfg = proxy.state.config.load();
        cfg.sites
            .get(req_ctx.site_idx)
            .and_then(|s| s.limits.as_ref())
            .and_then(|l| l.timeout_secs)
    };
    let resolution_timeout = req_ctx
        .proxy
        .proxy_timeout
        .as_ref()
        .and_then(|t| t.connect_ms)
        .or_else(|| limits_timeout_secs.map(|s| s.saturating_mul(1000)))
        .map(Duration::from_millis);

    let resolve_start = Instant::now();
    let socket_addr = resolve_socket_addr(&addr_str, resolution_timeout).await?;
    let resolve_elapsed = resolve_start.elapsed();
    let mut peer = HttpPeer::new(socket_addr, tls, sni);

    // Negotiate HTTP/2 with the upstream when the route sets `http2: true`.
    if req_ctx.proxy.proxy_http2 {
        peer.options.alpn = pingora_core::upstreams::peer::ALPN::H2H1;
    }

    // Apply upstream TLS settings (cert verification, custom server name).
    if let UpstreamTarget::Proxy {
        ref upstream_tls,
        ref sni,
        ..
    } = req_ctx.upstream
    {
        if let Some(tls_cfg) = upstream_tls {
            if let Some(verify) = tls_cfg.verify {
                peer.options.verify_cert = verify;
                peer.options.verify_hostname = verify;
            }
            if let Some(ref server_name) = tls_cfg.server_name {
                peer.options.alternative_cn = Some(server_name.clone());
            }
        }
        // Always set the SNI for TLS connections (already done by HttpPeer::new
        // but explicit here for clarity).
        let _ = sni; // sni already used in HttpPeer::new above
    }

    apply_peer_options(
        &mut peer,
        req_ctx.proxy.proxy_timeout.as_ref(),
        req_ctx.proxy.proxy_pool.as_ref(),
        limits_timeout_secs,
    );

    // Gitar finding on PR #227: `resolution_timeout` above and
    // `connection_timeout` here are derived from the same configured value,
    // but were two independent deadlines — a hostname upstream stalling at
    // both DNS resolution and TCP connect could consume up to 2x the
    // configured `connectMs`. Sharing one budget instead.
    peer.options.connection_timeout =
        remaining_budget(peer.options.connection_timeout, resolve_elapsed);

    Ok(Box::new(peer))
}

/// Subtract time already spent (e.g. on DNS resolution) from a connect
/// deadline, so two sequential phases share one budget instead of each
/// getting a fresh full timeout.
///
/// `saturating_sub` floors at zero rather than going negative, so a phase
/// that already consumed the whole budget correctly leaves zero time for
/// the next one (which then fails immediately) rather than silently
/// granting it a fresh deadline. `None` (no deadline configured) passes
/// through unchanged — there is no budget to share.
#[cfg(any(feature = "proxy", feature = "upload"))]
fn remaining_budget(deadline: Option<Duration>, elapsed: Duration) -> Option<Duration> {
    deadline.map(|d| d.saturating_sub(elapsed))
}

/// The `(addr, tls, sni)` triple for this attempt of a retry-configured
/// request -- the `proxy` variant. `None` when the route has no `retry`
/// configured at all (the first-attempt values then come from `ctx.upstream`
/// directly, see [`resolve_peer_addr`]).
///
/// Delegates to [`select_retry_target`] to decide which URL this attempt
/// targets, unifying URL selection with the request's
/// `proxy_upstream_url`/`upstream_conn_slot` bookkeeping into one decision
/// (#216 part 2).
#[cfg(feature = "proxy")]
fn retry_peer_addr(
    req_ctx: &mut RequestCtx,
    health: &UpstreamRegistry,
) -> Option<pingora_core::Result<(String, bool, String)>> {
    req_ctx.proxy.retry.as_ref()?;
    let url = select_retry_target(req_ctx, health);
    let Some(addr) = upstream::url_to_host_port(&url) else {
        return Some(Err(pingora_core::Error::explain(
            pingora_core::ErrorType::ConnectProxyFailure,
            format!("invalid upstream address: {url}"),
        )));
    };
    let tls = upstream::url_is_tls(&url);
    let sni = if tls {
        upstream::url_host(&url)
    } else {
        String::new()
    };
    Some(Ok((addr, tls, sni)))
}

/// No-`proxy` variant of [`retry_peer_addr`]: retry state is only ever
/// populated by the proxy routing resolvers, so without the feature there is
/// never a retry target to select and `ctx.upstream` is always authoritative.
#[cfg(all(feature = "upload", not(feature = "proxy")))]
fn retry_peer_addr(
    _req_ctx: &mut RequestCtx,
    _health: &UpstreamRegistry,
) -> Option<pingora_core::Result<(String, bool, String)>> {
    None
}

/// Resolve the upstream `(addr, tls, sni)` from the request context.
///
/// A retry-configured request (`proxy` only) takes its target from
/// [`retry_peer_addr`]; otherwise the values come from `ctx.upstream`
/// directly.
#[cfg(any(feature = "proxy", feature = "upload"))]
pub(super) fn resolve_peer_addr(
    req_ctx: &mut RequestCtx,
    health: &UpstreamRegistry,
) -> pingora_core::Result<(String, bool, String)> {
    if let Some(resolved) = retry_peer_addr(req_ctx, health) {
        return resolved;
    }
    match &req_ctx.upstream {
        UpstreamTarget::Proxy { addr, tls, sni, .. } => Ok((addr.clone(), *tls, sni.clone())),
        UpstreamTarget::Upload { addr } => Ok((addr.to_string(), false, String::new())),
        UpstreamTarget::Local(_) => Err(pingora_core::Error::explain(
            pingora_core::ErrorType::InternalError,
            "upstream_peer called for local handler",
        )),
    }
}

/// Apply per-route timeout, connection-pool settings, and global limits to an
/// `HttpPeer`.
///
/// Priority (highest → lowest):
/// 1. `proxy.*.timeout.*` — per-route fine-grained timeouts
/// 2. `limits.timeoutSecs` — site-wide fallback timeout
///
/// `limits.timeout_secs` is applied to all three timeout fields only when
/// the corresponding per-route field is absent.
#[cfg(any(feature = "proxy", feature = "upload"))]
pub(super) fn apply_peer_options(
    peer: &mut HttpPeer,
    timeout: Option<&ProxyTimeout>,
    pool: Option<&ConnectionPoolConfig>,
    limits_timeout_secs: Option<u64>,
) {
    let fallback_ms = limits_timeout_secs.map(|s| s.saturating_mul(1000));

    // connection_timeout
    peer.options.connection_timeout = timeout
        .and_then(|t| t.connect_ms)
        .or(fallback_ms)
        .map(Duration::from_millis);

    // read_timeout
    peer.options.read_timeout = timeout
        .and_then(|t| t.read_ms)
        .or(fallback_ms)
        .map(Duration::from_millis);

    // write_timeout
    peer.options.write_timeout = timeout
        .and_then(|t| t.send_ms)
        .or(fallback_ms)
        .map(Duration::from_millis);

    // first_byte_timeout — maps to read_timeout when explicitly set, allowing
    // operators to enforce a tight "time to first byte" window independently
    // of per-read I/O timeouts.  Takes precedence over readMs.
    if let Some(first_byte_ms) = timeout.and_then(|t| t.first_byte_ms) {
        peer.options.read_timeout = Some(Duration::from_millis(first_byte_ms));
    }

    if let Some(p) = pool {
        if let Some(secs) = p.idle_timeout_secs {
            peer.options.idle_timeout = Some(Duration::from_secs(secs));
        }
    }
}

/// The no-upstream stub is the only `upstream_peer` in a build with neither
/// `proxy` nor `upload`, so it gets its own tiny module -- the main test
/// module below only exists when a real upstream can be selected.
#[cfg(all(test, not(any(feature = "proxy", feature = "upload"))))]
mod stub_tests {
    use super::*;

    use crate::proxy::service::AppState;

    /// Without `proxy` and `upload` there is nothing to proxy to. The stub must
    /// answer with `ErrorType::HTTPStatus(404)` -- Pingora's `fail_to_proxy`
    /// turns that into a plain 404, never the 500/502 an `InternalError` or a
    /// panic would produce.
    #[tokio::test]
    async fn upstream_peer_without_proxy_or_upload_answers_404() {
        let state = AppState::new(
            crate::config::schema::AppConfig::default(),
            std::path::PathBuf::from("."),
            None,
        );
        let proxy = ConduitProxy {
            state: std::sync::Arc::new(state),
        };
        // `ctx` is deliberately `None`: the stub must not depend on request
        // state (the real implementation `expect`s it to be set).
        let mut ctx: Option<RequestCtx> = None;
        let err = upstream_peer(&proxy, &mut ctx)
            .await
            .expect_err("no upstream can be selected in this build");
        assert!(
            matches!(err.etype(), pingora_core::ErrorType::HTTPStatus(404)),
            "expected HTTPStatus(404), got {:?}",
            err.etype()
        );
    }
}

#[cfg(all(test, any(feature = "proxy", feature = "upload")))]
mod tests {
    use super::*;

    #[cfg(feature = "proxy")]
    use crate::proxy::ctx::RetryState;
    use crate::proxy::ctx::{LocalHandler, ProxyReqState};

    // ── resolve_peer_addr ─────────────────────────────────────────────────────

    // Canonical original location -- duplicated (identically) into
    // `handlers.rs`, `retry.rs`, and `transform.rs`'s own test modules, each
    // of which needs it and none of which depend on this one.
    fn make_ctx(upstream: UpstreamTarget) -> RequestCtx {
        RequestCtx::new(0, upstream, ProxyReqState::default(), None)
    }

    #[test]
    fn resolve_peer_addr_proxy_returns_addr_tls_sni() {
        let mut ctx = make_ctx(UpstreamTarget::Proxy {
            addr: "backend:4000".to_owned(),
            tls: false,
            sni: String::new(),
            strip_prefix: None,
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        });
        let reg = UpstreamRegistry::new();
        let (addr, tls, sni) = resolve_peer_addr(&mut ctx, &reg).unwrap();
        assert_eq!(addr, "backend:4000");
        assert!(!tls);
        assert!(sni.is_empty());
    }

    #[test]
    fn resolve_peer_addr_https_sets_tls_and_sni() {
        let mut ctx = make_ctx(UpstreamTarget::Proxy {
            addr: "api.example.com:443".to_owned(),
            tls: true,
            sni: "api.example.com".to_owned(),
            strip_prefix: None,
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        });
        let reg = UpstreamRegistry::new();
        let (addr, tls, sni) = resolve_peer_addr(&mut ctx, &reg).unwrap();
        assert_eq!(addr, "api.example.com:443");
        assert!(tls);
        assert_eq!(sni, "api.example.com");
    }

    /// `Upload` is the internal loopback upstream and must resolve in every
    /// build that compiles this module -- in particular `--features upload`
    /// without `proxy`, where it is the *only* target `upstream_peer` can
    /// see. Exact tuple on purpose: a wrong `tls`/`sni` here would silently
    /// break the upload service, not just fail a lookup.
    #[test]
    fn resolve_peer_addr_upload_returns_loopback_addr_without_tls() {
        let mut ctx = make_ctx(UpstreamTarget::Upload {
            addr: "127.0.0.1:4000".parse().unwrap(),
        });
        let reg = UpstreamRegistry::new();
        let resolved = resolve_peer_addr(&mut ctx, &reg).unwrap();
        assert_eq!(
            resolved,
            ("127.0.0.1:4000".to_owned(), false, String::new())
        );
    }

    /// Without `proxy`, retry state must never be consulted: the routing
    /// resolvers that populate it don't exist, so a stray `Some(retry)` must
    /// not redirect the request away from `ctx.upstream` (and must not be
    /// advanced -- `attempt` stays 0).
    #[cfg(not(feature = "proxy"))]
    #[test]
    fn resolve_peer_addr_ignores_retry_state_without_proxy() {
        use crate::proxy::ctx::RetryState;
        let mut ctx = make_ctx(UpstreamTarget::Upload {
            addr: "127.0.0.1:4000".parse().unwrap(),
        });
        ctx.proxy.retry = Some(RetryState {
            urls: vec!["http://a:4000".to_owned(), "http://b:4000".to_owned()],
            attempt: 0,
            max_attempts: 3,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: None,
            is_retrying: false,
            max_conns_per_upstream: None,
            tracks_conn_slot: false,
        });
        let reg = UpstreamRegistry::new();
        let resolved = resolve_peer_addr(&mut ctx, &reg).unwrap();
        assert_eq!(resolved.0, "127.0.0.1:4000");
        assert_eq!(ctx.proxy.retry.as_ref().unwrap().attempt, 0);
    }

    #[test]
    fn resolve_peer_addr_local_handler_returns_error() {
        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        let reg = UpstreamRegistry::new();
        assert!(
            resolve_peer_addr(&mut ctx, &reg).is_err(),
            "local handler must return error"
        );
    }

    // ── remaining_budget (Gitar finding on PR #227: shared connect budget) ──────

    #[test]
    fn remaining_budget_none_deadline_stays_none() {
        // No timeout configured at all -- nothing to share, passes through.
        assert_eq!(remaining_budget(None, Duration::from_millis(500)), None);
    }

    #[test]
    fn remaining_budget_subtracts_elapsed() {
        let deadline = Duration::from_millis(5000);
        let elapsed = Duration::from_millis(1200);
        assert_eq!(
            remaining_budget(Some(deadline), elapsed),
            Some(Duration::from_millis(3800))
        );
    }

    #[test]
    fn remaining_budget_zero_elapsed_is_unchanged() {
        // The IP-literal fast path in resolve_socket_addr never awaits DNS,
        // so elapsed is ~0 -- the configured deadline must be preserved
        // exactly, not just "close to" it.
        let deadline = Duration::from_millis(5000);
        assert_eq!(
            remaining_budget(Some(deadline), Duration::ZERO),
            Some(deadline)
        );
    }

    #[test]
    fn remaining_budget_elapsed_exceeding_deadline_saturates_to_zero() {
        // A resolution that already consumed the whole budget (or more, if
        // resolution_timeout itself elapsed) must leave zero time for the
        // next phase -- not underflow/panic, and not silently grant a fresh
        // deadline by wrapping.
        let deadline = Duration::from_millis(1000);
        let elapsed = Duration::from_millis(1500);
        assert_eq!(
            remaining_budget(Some(deadline), elapsed),
            Some(Duration::ZERO)
        );
    }

    // ── apply_peer_options ───────────────────────────────────────────────────

    #[test]
    fn apply_peer_options_sets_timeouts() {
        use crate::config::schema::ProxyTimeout;
        // Use IP address to avoid DNS lookup in tests
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let mut peer = HttpPeer::new(addr, false, String::new());
        let timeout = ProxyTimeout {
            connect_ms: Some(500),
            read_ms: Some(1000),
            send_ms: Some(2000),
            per_try_ms: None,
            first_byte_ms: None,
        };
        apply_peer_options(&mut peer, Some(&timeout), None, None);
        assert_eq!(
            peer.options.connection_timeout,
            Some(std::time::Duration::from_millis(500))
        );
        assert_eq!(
            peer.options.read_timeout,
            Some(std::time::Duration::from_millis(1000))
        );
        assert_eq!(
            peer.options.write_timeout,
            Some(std::time::Duration::from_millis(2000))
        );
    }

    #[test]
    fn apply_peer_options_uses_fallback_timeout() {
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let mut peer = HttpPeer::new(addr, false, String::new());
        // No per-route timeout, but global limits.timeoutSecs = 5s → 5000ms fallback
        apply_peer_options(&mut peer, None, None, Some(5));
        assert_eq!(
            peer.options.connection_timeout,
            Some(std::time::Duration::from_millis(5000))
        );
    }

    #[test]
    fn apply_peer_options_no_timeout_leaves_defaults() {
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let mut peer = HttpPeer::new(addr, false, String::new());
        apply_peer_options(&mut peer, None, None, None);
        // No timeout set → remains None (Pingora's default).
        assert!(peer.options.connection_timeout.is_none());
        assert!(peer.options.read_timeout.is_none());
    }

    #[test]
    fn apply_peer_options_sets_idle_timeout() {
        use crate::config::schema::ConnectionPoolConfig;
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let mut peer = HttpPeer::new(addr, false, String::new());
        let pool = ConnectionPoolConfig {
            max_idle: None,
            idle_timeout_secs: Some(30),
        };
        apply_peer_options(&mut peer, None, Some(&pool), None);
        assert_eq!(
            peer.options.idle_timeout,
            Some(std::time::Duration::from_secs(30))
        );
    }

    #[test]
    fn apply_peer_options_first_byte_ms_overrides_read_timeout() {
        use crate::config::schema::ProxyTimeout;
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let mut peer = HttpPeer::new(addr, false, String::new());
        let timeout = ProxyTimeout {
            connect_ms: None,
            read_ms: Some(30_000), // 30s general read timeout
            send_ms: None,
            per_try_ms: None,
            first_byte_ms: Some(500), // 500ms first-byte timeout overrides readMs
        };
        apply_peer_options(&mut peer, Some(&timeout), None, None);
        // first_byte_ms takes precedence over read_ms
        assert_eq!(
            peer.options.read_timeout,
            Some(std::time::Duration::from_millis(500))
        );
    }

    // ── resolve_peer_addr (retry) ─────────────────────────────────────────────

    #[cfg(feature = "proxy")]
    #[test]
    fn resolve_peer_addr_with_retry_returns_first_url() {
        let retry = RetryState {
            urls: vec!["http://a:4000".to_owned(), "http://b:4000".to_owned()],
            attempt: 0,
            max_attempts: 3,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: None,
            is_retrying: false,
            max_conns_per_upstream: None,
            tracks_conn_slot: false,
        };
        let mut ctx = make_ctx(UpstreamTarget::Proxy {
            addr: "original:4000".to_owned(),
            tls: false,
            sni: String::new(),
            strip_prefix: None,
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        });
        ctx.proxy.retry = Some(retry);
        let reg = UpstreamRegistry::new();
        let (addr, _, _) = resolve_peer_addr(&mut ctx, &reg).unwrap();
        assert_eq!(addr, "a:4000");
        // Attempt should be incremented.
        assert_eq!(ctx.proxy.retry.unwrap().attempt, 1);
    }
}
