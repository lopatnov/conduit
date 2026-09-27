//! Graceful shutdown coordination.
//!
//! ## How shutdown works in Conduit
//!
//! 1. **SIGTERM** is caught by Pingora's server loop, which triggers a graceful
//!    shutdown sequence.
//! 2. Pingora stops accepting new connections on all bound TCP listeners.
//! 3. In-flight requests are tracked by `AppState.inflight: Arc<AtomicUsize>`.
//!    Each request increments this counter in `request_filter` and decrements
//!    it in `logging()` after the response is sent.
//! 4. Pingora waits for all active connections to drain before the process exits.
//!    The timeout is controlled by `global.shutdownTimeoutSecs` (default: 30 s).
//!
//! ## Admin API `/shutdown`
//!
//! `POST /shutdown` triggers a graceful shutdown via `std::process::exit(0)`,
//! allowing Pingora's drop handlers and the OS to clean up resources.  This
//! endpoint is intentionally simple — coordinated multi-worker draining is
//! delegated to Pingora.
//!
//! ## Future work
//!
//! - Expose inflight counter in `/status` for operational visibility (done ✓).
//! - Hook `AppState.inflight` into Pingora's shutdown signal so the server waits
//!   for zero inflight before exiting (currently Pingora's built-in drain is used).
