//! Per-request limits state (`CLAUDE.md` decision #30).
//!
//! Mirrors `conduit_cache::CacheReqState` — a small state struct owned by
//! this crate that the root crate's `RequestCtx` holds. Unlike
//! `CacheReqState` (which is `#[cfg(feature = "cache")]`-gated because
//! `cache` is an optional Cargo feature), [`LimitsReqState`] has **no
//! feature gate at all** — `limits` is always-on (`CLAUDE.md` decision #31),
//! so `RequestCtx` holds it as a plain, always-present field
//! (`pub limits: conduit_limits::LimitsReqState`), never wrapped in
//! `Option<>` or gated behind `#[cfg(feature = "...")]`.

/// Per-request limits-related state, threaded through the request pipeline.
#[derive(Debug, Default)]
pub struct LimitsReqState {
    /// Running tally of actual body bytes received so far.
    ///
    /// Incremented in `request_body_filter` for every chunk regardless of
    /// whether retry buffering is active. Used to enforce
    /// `limits.maxBodyBytes` against clients that omit `Content-Length` or
    /// use chunked encoding.
    pub actual_body_bytes: u64,
    /// RAII guard that releases the per-IP connection slot when this context
    /// is dropped at the end of `logging()`. `None` when
    /// `limits.maxConnectionsPerIp` is not configured or the request was
    /// rejected before a slot was acquired.
    pub ip_conn_slot: Option<crate::guard::IpConnSlotGuard>,
    /// Slow-loris upload defense: accumulated excess bytes for the
    /// leaky-bucket rate checker in `request_body_filter`.
    ///
    /// Positive excess means the client is sending faster than
    /// `minUploadRate` would allow; negative means the client has headroom.
    /// Set to 0.0 on init.
    pub upload_excess_bytes: f64,
    /// Timestamp of the last body chunk received, used by the upload-rate checker.
    pub upload_last_chunk: Option<std::time::Instant>,
}
