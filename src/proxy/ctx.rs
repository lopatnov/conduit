use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::config::schema::StaticOptions;

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
}

impl RequestCtx {
    pub fn new(site_idx: usize, upstream: UpstreamTarget, retry: Option<RetryState>) -> Self {
        Self {
            site_idx,
            upstream,
            start_time: Instant::now(),
            accept_enc: AcceptEncoding::default(),
            retry,
            extra_headers: Vec::new(),
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
    Metrics { token: Option<String> },
    StaticFile {
        roots: Vec<PathBuf>,
        options: Arc<StaticOptions>,
        strip_prefix: Option<String>,
    },
}

#[derive(Debug, Default)]
pub struct AcceptEncoding {
    pub brotli: bool,
    pub gzip: bool,
    pub deflate: bool,
}

impl AcceptEncoding {
    pub fn parse(value: &str) -> Self {
        let mut enc = Self::default();
        for part in value.split(',') {
            let token = part
                .trim()
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
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
