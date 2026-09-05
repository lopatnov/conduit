//! Extracted into `crates/conduit-cors` (issue #114/#136) — this is a
//! facade re-export so `crate::filter::cors::{is_preflight,
//! requests_private_network_access, request_origin, response_headers,
//! handle_preflight}` keep resolving to the same items at the same location
//! for backward compatibility. See `conduit_cors::cors` for the
//! implementation and `conduit_cors::guard` for the real `CorsPreflight`
//! guard (re-exported at `crate::filter::chain::CorsPreflight`, see that
//! file).
pub use conduit_cors::cors::{
    handle_preflight, is_preflight, request_origin, requests_private_network_access,
    response_headers,
};
