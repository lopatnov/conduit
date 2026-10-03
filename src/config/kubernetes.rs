//! Facade over `conduit_server` (issue #147): moved into `crates/conduit-server`. Every item is re-exported
//! here at its original path, so no call site changed. Compiled only when the `kubernetes` feature is enabled —
//! see the `#[cfg(feature = "kubernetes")] pub mod kubernetes;` gate in `src/config/mod.rs`.

pub use conduit_server::config::kubernetes::{
    build_app_config, spec_to_site_config, ConduitSchema, ConduitSite, ConduitSiteSpec,
    ConduitSiteStatus, KubernetesProvider,
};
