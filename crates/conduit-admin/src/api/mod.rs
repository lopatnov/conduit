//! The Admin API: the axum server, bearer-token authentication and the endpoint handlers (moved out of the root
//! crate's `src/admin/api.rs`, issue #146). `POST /reload` stays in the root — it needs the root's config validation —
//! and joins the router through [`build_router`]'s `extra` parameter.

mod cache_purge;
mod certs;
mod error;
mod ip_deny;
mod rate_limits;
mod router;
mod server;
mod status;
mod upstreams;
mod upstreams_view;

pub use certs::validate_cert_key_pem;
pub use error::{AdminError, AdminResult};
pub use router::build_router;
pub use server::serve;

// The test module reaches the handlers and helpers through `super::*`.
#[cfg(test)]
use cache_purge::*;
#[cfg(test)]
use certs::*;
#[cfg(test)]
use ip_deny::*;
#[cfg(test)]
use rate_limits::*;
#[cfg(test)]
use router::*;
#[cfg(test)]
use upstreams::*;
#[cfg(test)]
use upstreams_view::*;

#[cfg(test)]
mod tests;
