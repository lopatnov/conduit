//! Kubernetes provider — watch `ConduitSite` CRDs and stream config updates.
//!
//! This crate implements the generic list+watch machinery for `ConduitSite`
//! CRDs, but must not know about conduit's real `AppConfig`/`SiteConfig`
//! schema (a dependency on the root crate would be a cycle) — see
//! `CONTRIBUTING.md`'s "Cargo Workspace Crate Extraction Recipe", rule 2:
//! "generic-in-crate, bound-by-type-alias-in-root".
//!
//! The root crate supplies the missing schema knowledge by implementing
//! [`CrdConfigBuilder`] on a zero-sized type and binding
//! `KubernetesProvider<ThatType>` to a `pub type KubernetesProvider` (see
//! `src/config/kubernetes.rs`) — the same shape already used for
//! `conduit_upload`'s `UploadConfigSource`, just for a bigger single trait
//! instead of several smaller ones.
//!
//! Enabled via the root crate's `kubernetes` Cargo feature
//! (`dep:lopatnov-conduit-k8s`):
//! ```bash
//! cargo build --features kubernetes
//! ```
//!
//! ## Custom Resource Definition
//!
//! Each `ConduitSite` resource in Kubernetes maps to one site config. The
//! provider lists all `ConduitSite` resources in the configured namespace,
//! builds the full config, and sends it. It then watches for `Added`,
//! `Modified`, and `Deleted` events and re-sends an updated config on each.
//!
//! Example resource:
//! ```yaml
//! apiVersion: conduit.io/v1
//! kind: ConduitSite
//! metadata:
//!   name: my-app
//!   namespace: default
//! spec:
//!   port: 8080
//!   proxy: "http://my-svc:4000"
//!   healthCheck: true
//! ```
//!
//! Install the CRD with:
//! ```bash
//! kubectl apply -f contrib/k8s/conduitsite-crd.yaml
//! ```

mod crd;
mod provider;

pub use crd::{ConduitSite, ConduitSiteSpec, ConduitSiteStatus};
pub use provider::{build_app_config, KubernetesProvider};

/// Binds `ConduitSite` CRD watching to a concrete conduit config schema.
///
/// The root crate supplies the `AppConfig`/`SiteConfig` knowledge this crate
/// must not have. `Site` is the per-CRD converted value (`SiteConfig` in the
/// root crate); `Config` is the full value sent on the provider channel
/// (`AppConfig` in the root crate).
pub trait CrdConfigBuilder: Send + Sync + 'static {
    /// The per-CRD converted site value (e.g. `SiteConfig`).
    type Site;
    /// The full config value streamed to the provider's channel (e.g. `AppConfig`).
    type Config: Send + 'static;

    /// Convert one CRD spec into one site value.
    fn site_from_spec(spec: &ConduitSiteSpec) -> anyhow::Result<Self::Site>;

    /// Combine every site value plus the Admin API bind address into the
    /// full config value sent on the channel.
    fn build_config(sites: Vec<Self::Site>, admin_bind: &str) -> Self::Config;
}
