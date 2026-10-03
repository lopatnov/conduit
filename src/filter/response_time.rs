//! Facade over `conduit_runtime` (issue #145): the `X-Response-Time` helpers moved into `crates/conduit-runtime`.
//! Every item is re-exported here at its original path, so no call site changed.

pub use conduit_runtime::filter::response_time::{decimal_digits, format_elapsed, is_enabled};
