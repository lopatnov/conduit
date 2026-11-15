//! External processing filter: the Envoy `ext_proc` protocol (a bidirectional gRPC stream per request) as a Conduit
//! middleware, `type: "ext_proc"` in `sites[].middleware[]` (issue #27).
//!
//! What this slice does: the request-headers phase. The proxy sends the request headers to the processor and applies what
//! comes back: header mutations (under [`mutation`]'s rules) or an immediate response that ends the request. Request and
//! response bodies, the response phase and trailers are later slices of the same issue.
//!
//! The crate follows the usual shape of a feature crate: [`config`] and [`warnings`] are always compiled, so a
//! configuration that names the filter parses and validates in every build (and a build without the `ext-proc` feature says
//! so instead of silently skipping a policy check); the gRPC client, the protocol messages and the mutation rules are
//! behind the crate's own `ext-proc` feature.
//!
//! Failure is closed by default: a processor that cannot be reached, answers late, or answers with something this proxy does
//! not accept ends the request with `statusOnError` (503). `failOpen: true` lets the request continue unchanged instead.

pub mod config;
pub mod warnings;

pub use config::ExtProcConfig;

#[cfg(feature = "ext-proc")]
pub mod client;
#[cfg(feature = "ext-proc")]
pub mod mutation;
#[cfg(feature = "ext-proc")]
pub mod proto;

#[cfg(feature = "ext-proc")]
pub use client::{clear_channels, DirectResponse, ExtProcClient, RequestInfo, Verdict};
#[cfg(feature = "ext-proc")]
pub use mutation::{HeaderChanges, HeaderSet, SetMode};
