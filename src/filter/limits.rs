//! Extracted into `crates/conduit-limits` (issue #114/#137) — this is a
//! thin facade so `crate::filter::limits::{CheckResult, check,
//! upload_rate_step}` keep resolving to the same names at the same location
//! for every existing call site/test. See that crate's `src/limits.rs` for
//! the implementation: declared-Content-Length / header-size checks, and
//! the leaky-bucket minimum-upload-rate algorithm (issue #51).

pub use conduit_limits::limits::{check, upload_rate_step, CheckResult};
