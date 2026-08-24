#![cfg(feature = "jwt")]
//! Extracted into `crates/conduit-auth-jwt` (issue #114/#133) — this is a
//! facade re-export so `crate::filter::jwt::{check_jwt, check_jwt_extracting,
//! JwtCheckResult}` keep resolving to the same items at the same location
//! for every existing call site/test (notably the per-consumer JWT V2/V3
//! credential checks in `src/filter/auth.rs`, which stay in the root crate
//! since `consumers` hasn't been extracted yet — see #134). See
//! `conduit_auth_jwt::jwt` for the implementation.

pub use conduit_auth_jwt::{check_jwt, check_jwt_extracting, JwtCheckResult};
