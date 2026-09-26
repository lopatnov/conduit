//! Facade over `conduit_runtime` (issue #145): the upload service bound to `AppState` moved into `crates/conduit-runtime`.
//! Every item is re-exported here at its original path, so no call site changed.

pub use conduit_runtime::upload::{run_upload_server, UploadService};
