//! Per-request proxy-routing state, grouped ahead of the eventual
//! `conduit-proxy-http` crate extraction (issue #143).
//!
//! This module is deliberately created now so that a later PR (A2 of the
//! 3-PR plan for #143) can fill it further with the phase-split work, and a
//! subsequent PR (B) can move it wholesale into the new crate — this PR (A1)
//! is a pure same-crate regrouping and does not extract anything.

pub mod state;
