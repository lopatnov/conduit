#![cfg(feature = "jwt")]
//! Extracted into `crates/conduit-auth-jwt` (issue #114/#133) — this is a
//! facade re-export so `crate::filter::jwt::{check_jwt, check_jwt_extracting,
//! JwtCheckResult}` keep resolving to the same items at the same location
//! for backward compatibility. See `conduit_auth_jwt::jwt` for the
//! implementation.
//!
//! **No in-tree call site left as of #114/#134**: the one caller that used
//! to justify this facade — the per-consumer JWT V2/V3 credential checks in
//! `src/filter/auth.rs` — moved to `crates/conduit-auth-consumers` (#134)
//! and now imports `conduit_auth_jwt::{check_jwt, check_jwt_extracting,
//! JwtCheckResult}` directly (it can't reach this root-crate facade from a
//! sibling Layer-1 crate). Left in place rather than deleted: it's a `pub`
//! item, so removing it would be a semver-relevant surface change per
//! `CLAUDE.md` decision #32 even though nothing in this tree calls it today.

pub use conduit_auth_jwt::{check_jwt, check_jwt_extracting, JwtCheckResult};
