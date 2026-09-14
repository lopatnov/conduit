//! Facade re-export — URL parsing helpers and the plain-slice load-balancing
//! "pick" algorithms moved to `crates/conduit-upstream::targets` (issue
//! #114/#142).
//!
//! `target_urls`/`weighted_targets`/`target_urls_from_proxy` — the three
//! upstream-target-list functions that needed `ProxyRouteTarget`/
//! `ProxyConfig` and therefore stayed behind in this file since #142 (moving
//! them into `conduit-upstream` would have forced `ProxyRouteConfig` to move
//! too, a genuine circular dependency with several other not-yet-extracted
//! crates — see `crates/conduit-upstream/src/lib.rs`'s own doc comment for
//! the detail) — have now moved to `crates/conduit-proxy-http::targets`
//! (issue #143 PR B), which resolved that circular dependency by owning
//! `ProxyRouteTarget`/`ProxyConfig` itself. `strip_prefix_enabled` (the
//! fourth function named by #142's own scope) did **not** move: it had zero
//! real production call sites (confirmed via grep before this PR — only its
//! own unit tests and doc-comment mentions referenced it), so it was deleted
//! outright rather than relocated. See `crates/conduit-proxy-http/src/
//! targets.rs`'s own doc comment for the full detail.

pub use conduit_upstream::targets::*;

pub use conduit_proxy_http::targets::{target_urls, target_urls_from_proxy, weighted_targets};
