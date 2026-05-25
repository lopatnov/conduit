use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::config::schema::{CacheConfig, ConnectionPoolConfig, ProxyTimeout, StaticOptions};

#[derive(Debug)]
pub struct RequestCtx {
    pub site_idx: usize,
    pub upstream: UpstreamTarget,
    pub start_time: Instant,
    pub accept_enc: AcceptEncoding,
    /// Populated when the matched route has a `retry` configuration.
    pub retry: Option<RetryState>,
    /// CORS + security headers to inject into every response for this request.
    /// Computed once in `request_filter` and reused for all write paths.
    pub extra_headers: Vec<(String, String)>,
    /// Per-route proxy connection timeouts (from `proxy.*.timeout`).
    pub proxy_timeout: Option<ProxyTimeout>,
    /// Per-route connection pool settings (from `proxy.*.pool`).
    pub proxy_pool: Option<ConnectionPoolConfig>,
    /// When `true`, negotiate HTTP/2 with the upstream (ALPN H2H1).
    /// Derived from `proxy.*.http2: true` in the route config.
    pub proxy_http2: bool,
    /// The upstream URL that was selected for this request.
    ///
    /// `Some` only for `strategy: "least-conn"` routes so that the `logging()`
    /// hook can decrement the per-upstream connection counter after the response
    /// is sent.
    pub proxy_upstream_url: Option<String>,
    /// Cache configuration for this route (`proxy.*.cache`), if caching is enabled.
    ///
    /// `None` means the route has no cache config and caching is disabled for
    /// this request.
    pub proxy_cache_cfg: Option<CacheConfig>,
}

impl RequestCtx {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        site_idx: usize,
        upstream: UpstreamTarget,
        retry: Option<RetryState>,
        proxy_timeout: Option<ProxyTimeout>,
        proxy_pool: Option<ConnectionPoolConfig>,
        proxy_http2: bool,
        proxy_upstream_url: Option<String>,
        proxy_cache_cfg: Option<CacheConfig>,
    ) -> Self {
        Self {
            site_idx,
            upstream,
            start_time: Instant::now(),
            accept_enc: AcceptEncoding::default(),
            retry,
            extra_headers: Vec::new(),
            proxy_timeout,
            proxy_pool,
            proxy_http2,
            proxy_upstream_url,
            proxy_cache_cfg,
        }
    }
}

/// Per-request retry state for proxy routes that have `retry` configured.
///
/// The URL list is rotated so that `urls[0]` is the round-robin starting
/// target for this particular request.  Subsequent retries advance through
/// `urls[1 % len]`, `urls[2 % len]`, etc.
#[derive(Debug)]
pub struct RetryState {
    /// All target URLs for the route, rotated to start at the RR position.
    pub urls: Vec<String>,
    /// Number of times `upstream_peer()` has been called so far (0 = first call).
    pub attempt: usize,
    /// Total attempts allowed including the initial one (e.g. `attempts: 3` ⇒ 3 tries).
    pub max_attempts: usize,
    /// Error conditions that should trigger a retry.
    /// Valid values: `"connection_error"` | `"5xx"` | `"timeout"`.
    pub conditions: Vec<String>,
    /// Optional delay between retries in milliseconds.
    pub backoff_ms: Option<u64>,
}

impl RetryState {
    /// Returns `true` when there are retries left (i.e. we have not yet exhausted
    /// `max_attempts`).  Call this *after* `attempt` has been incremented by
    /// `upstream_peer()`.
    pub fn has_attempts_left(&self) -> bool {
        self.attempt < self.max_attempts
    }

    pub fn has_condition(&self, cond: &str) -> bool {
        self.conditions.iter().any(|c| c == cond)
    }
}

#[derive(Debug)]
pub enum UpstreamTarget {
    Local(LocalHandler),
    Proxy {
        /// "host:port" string passed to Pingora's HttpPeer::new.
        addr: String,
        tls: bool,
        sni: String,
        strip_prefix: Option<String>,
    },
    Upload {
        addr: SocketAddr,
    },
}

#[derive(Debug, Clone)]
pub enum LocalHandler {
    Health,
    Fallback,
    Metrics {
        token: Option<String>,
    },
    StaticFile {
        roots: Vec<PathBuf>,
        options: Arc<StaticOptions>,
        strip_prefix: Option<String>,
    },
}

#[derive(Debug, Default, Clone)]
pub struct AcceptEncoding {
    pub brotli: bool,
    pub gzip: bool,
    pub deflate: bool,
}

impl AcceptEncoding {
    pub fn parse(value: &str) -> Self {
        let mut enc = Self::default();
        for part in value.split(',') {
            let mut segments = part.trim().split(';');
            let token = segments.next().unwrap_or("").trim().to_ascii_lowercase();
            // Skip encodings explicitly disabled with q=0 or q=0.0.
            let is_zero_q = segments.any(|seg| {
                let seg = seg.trim();
                seg.eq_ignore_ascii_case("q=0") || seg.eq_ignore_ascii_case("q=0.0")
            });
            if is_zero_q {
                continue;
            }
            match token.as_str() {
                "br" => enc.brotli = true,
                "gzip" => enc.gzip = true,
                "deflate" => enc.deflate = true,
                _ => {}
            }
        }
        enc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── AcceptEncoding::parse ─────────────────────────────────────────────────

    #[test]
    fn parse_empty_enables_nothing() {
        let enc = AcceptEncoding::parse("");
        assert!(!enc.brotli && !enc.gzip && !enc.deflate);
    }

    #[test]
    fn parse_gzip_only() {
        let enc = AcceptEncoding::parse("gzip");
        assert!(enc.gzip);
        assert!(!enc.brotli);
        assert!(!enc.deflate);
    }

    #[test]
    fn parse_br_only() {
        let enc = AcceptEncoding::parse("br");
        assert!(enc.brotli);
        assert!(!enc.gzip);
    }

    #[test]
    fn parse_multiple_encodings() {
        let enc = AcceptEncoding::parse("br, gzip, deflate");
        assert!(enc.brotli);
        assert!(enc.gzip);
        assert!(enc.deflate);
    }

    #[test]
    fn parse_q_zero_disables_encoding() {
        let enc = AcceptEncoding::parse("gzip;q=0, br");
        assert!(!enc.gzip, "gzip with q=0 must be skipped");
        assert!(enc.brotli);
    }

    #[test]
    fn parse_q_zero_zero_disables_encoding() {
        let enc = AcceptEncoding::parse("gzip;q=0.0");
        assert!(!enc.gzip);
    }

    #[test]
    fn parse_case_insensitive() {
        let enc = AcceptEncoding::parse("GZip, BR, Deflate");
        assert!(enc.gzip);
        assert!(enc.brotli);
        assert!(enc.deflate);
    }

    #[test]
    fn parse_unknown_token_ignored() {
        let enc = AcceptEncoding::parse("identity, zstd, gzip");
        assert!(enc.gzip);
        assert!(!enc.brotli);
    }

    // ── RetryState ────────────────────────────────────────────────────────────

    fn make_retry(attempt: usize, max: usize, conditions: &[&str]) -> RetryState {
        RetryState {
            urls: vec!["http://a:4000".to_string()],
            attempt,
            max_attempts: max,
            conditions: conditions.iter().map(|s| s.to_string()).collect(),
            backoff_ms: None,
        }
    }

    #[test]
    fn has_attempts_left_when_under_max() {
        assert!(make_retry(0, 3, &[]).has_attempts_left());
        assert!(make_retry(2, 3, &[]).has_attempts_left());
    }

    #[test]
    fn no_attempts_left_when_at_max() {
        assert!(!make_retry(3, 3, &[]).has_attempts_left());
        assert!(!make_retry(5, 3, &[]).has_attempts_left());
    }

    #[test]
    fn has_condition_matches_exact_string() {
        let rs = make_retry(0, 3, &["5xx", "connection_error"]);
        assert!(rs.has_condition("5xx"));
        assert!(rs.has_condition("connection_error"));
        assert!(!rs.has_condition("timeout"));
    }

    #[test]
    fn has_condition_empty_list_never_matches() {
        let rs = make_retry(0, 3, &[]);
        assert!(!rs.has_condition("5xx"));
    }
}
