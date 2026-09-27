//! Facade over `conduit_server` (issue #147): moved into `crates/conduit-server`. Every item is re-exported
//! here at its original path, so no call site changed.

pub use conduit_server::config::provider::{
    file_provider, load_and_validate, FileProvider, Provider,
};
