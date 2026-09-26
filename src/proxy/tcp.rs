#![cfg(feature = "tcp")]
//! Facade for the real `TcpProxy` implementation, which lives in
//! `crates/conduit-tcp` (issue #114/#131). This file just re-exports it so
//! `crate::proxy::tcp::TcpProxy` keeps resolving to the same type at the
//! same location for every existing call site.

pub use conduit_tcp::proxy::TcpProxy;
