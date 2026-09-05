use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct LimitsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_body_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_header_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Maximum concurrent requests to this site.  When the number of in-flight
    /// requests reaches this limit, new requests receive `503 Service Unavailable`
    /// immediately rather than queuing.  Defaults to unlimited.
    #[serde(
        rename = "maxInflightRequests",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_inflight_requests: Option<u64>,
    /// Maximum request body size to buffer for retry replay (bytes).
    ///
    /// When a route has `retry` configured, Conduit buffers the request body
    /// up to this limit so it can be replayed on a retry attempt.  If the
    /// body exceeds this limit, retries are still attempted but without body
    /// replay (safe only for GET/HEAD which have no body in practice).
    ///
    /// Defaults to 1 MiB (1_048_576 bytes).  Set to 0 to disable buffering.
    #[serde(rename = "maxBodyBufferBytes", skip_serializing_if = "Option::is_none")]
    pub max_body_buffer_bytes: Option<u64>,
    /// Maximum number of requests served over a single keepalive connection.
    ///
    /// After this many requests the connection is closed and per-connection
    /// memory is reclaimed.  `None` means unlimited (default Pingora behaviour).
    ///
    /// Equivalent to nginx's `keepalive_requests`.
    #[serde(
        rename = "keepaliveRequestLimit",
        skip_serializing_if = "Option::is_none"
    )]
    pub keepalive_request_limit: Option<u32>,
    /// Inflight load fraction at which low-priority routes are shed (0.0–1.0).
    ///
    /// Requires `maxInflightRequests` to be set.  When
    /// `inflight / maxInflightRequests ≥ priorityThreshold`, requests whose
    /// effective priority (from `proxy.*.priority` or the `X-Priority` header)
    /// is below 50 are rejected with `503 Load Shedding`.  Requests with no
    /// explicit priority or priority ≥ 50 are always forwarded.
    ///
    /// Example: `maxInflightRequests: 1000, priorityThreshold: 0.8` →
    /// at 800+ concurrent requests, low-priority routes are shed.
    ///
    /// Defaults to `0.8` when `maxInflightRequests` is set and any route
    /// configures a `priority`.
    #[serde(rename = "priorityThreshold", skip_serializing_if = "Option::is_none")]
    pub priority_threshold: Option<f64>,
    /// Maximum concurrent in-flight requests from a single client IP.
    ///
    /// **nginx `limit_conn` pattern.**  Unlike `rateLimit` (requests per second),
    /// this limits the number of *simultaneous* open requests from the same IP.
    /// Protects against connection-flooding attacks that bypass per-second rate
    /// limits by opening many slow/hung connections at once.
    ///
    /// When the limit is reached, new requests from that IP receive `429 Too Many
    /// Requests` until an existing connection completes.  The counter is incremented
    /// at request entry and decremented in the `logging()` hook.
    ///
    /// Uses the same IP-trust logic as `ipFilter.trustProxy`.
    ///
    /// Default: unlimited (`None`).
    ///
    /// ```json
    /// { "maxConnectionsPerIp": 20 }
    /// ```
    #[serde(
        rename = "maxConnectionsPerIp",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_connections_per_ip: Option<u64>,
    /// Maximum number of headers allowed in an incoming request.
    ///
    /// Requests with more headers than this limit are rejected with
    /// `431 Request Header Fields Too Large`.  Protects against header-flooding
    /// attacks and misconfigured clients that send excessive headers.
    ///
    /// Equivalent to nginx `large_client_header_buffers` count limit.
    ///
    /// Default: unlimited (`None`).
    ///
    /// ```json
    /// { "maxRequestHeaders": 100 }
    /// ```
    #[serde(rename = "maxRequestHeaders", skip_serializing_if = "Option::is_none")]
    pub max_request_headers: Option<u32>,
    /// Minimum upload rate in bytes per second (slow-loris upload defence).
    ///
    /// **freenginx / nginx `client_body_min_rate` pattern.**  Uses a leaky-bucket
    /// algorithm: excess accumulates when the client sends slower than the limit.
    /// When accumulated excess exceeds the burst allowance (1 second of data by
    /// default) the request is terminated with `408 Request Timeout`.
    ///
    /// Typical values: `1024` (1 KiB/s) for strict protection,
    /// `256` for slow-network tolerance.
    ///
    /// `None` (default) disables the check.
    ///
    /// ```yaml
    /// limits:
    ///   minUploadRateBytesPerSec: 1024
    /// ```
    #[serde(
        rename = "minUploadRateBytesPerSec",
        skip_serializing_if = "Option::is_none"
    )]
    pub min_upload_rate_bytes_per_sec: Option<u64>,
}
