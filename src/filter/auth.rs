//! Facade over `conduit_runtime` (issue #145): Basic-auth / API-key checks moved into `crates/conduit-runtime`.
//! Every item is re-exported here at its original path, so no call site changed.

pub use conduit_runtime::filter::auth::{
    check_api_key, check_basic_auth, is_path_skipped, BasicAuthResult,
};
