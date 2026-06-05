//! Custom response-header utilities.
//!
//! Site-level custom headers (`site.headers: { "X-Foo": "bar" }`) are
//! collected in [`crate::proxy::service`]'s `do_request_filter` and stored in
//! [`crate::proxy::ctx::RequestCtx::extra_headers`].  They are then injected
//! into every response — proxy responses in `upstream_response_filter` and
//! local handler responses via [`crate::handler::response`]'s write helpers.
//!
//! Per-route static transforms (`requestTransform` / `responseTransform`) are
//! handled by [`crate::filter::chain`]'s middleware and by helper functions in
//! `service.rs`.
//!
//! This module is reserved for future header-manipulation utilities that don't
//! fit naturally into either location.
