//! Extracted into `crates/conduit-auth-jwt` (issue #114/#133) — this is a
//! facade re-export so `crate::util::jwt_template::expand_jwt_templates`
//! keeps resolving to the same item at the same location for every existing
//! call site/test. Deliberately kept `pub(crate)` here (narrower than the
//! new crate's own `pub fn`) to preserve the original encapsulation
//! boundary — this function was never part of the root crate's own public
//! surface. See `conduit_auth_jwt::template` for the implementation and its
//! doc comment for *why* it's always compiled regardless of the `jwt`
//! feature.

pub(crate) use conduit_auth_jwt::template::expand_jwt_templates;
