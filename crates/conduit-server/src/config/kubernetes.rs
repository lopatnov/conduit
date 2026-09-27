//! Kubernetes provider — watch `ConduitSite` CRDs and stream config updates.
//!
//! Enabled with the `kubernetes` feature flag:
//! ```bash
//! cargo build --features kubernetes
//! ```
//!
//! ## Custom Resource Definition
//!
//! Each `ConduitSite` resource in Kubernetes maps to one [`SiteConfig`].
//! The provider lists all `ConduitSite` resources in the configured namespace,
//! builds an [`AppConfig`], and sends it. It then watches for `Added`,
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
//!
//! The generic list+watch mechanism (`KubernetesProvider<B>`, the CRD types,
//! and the watch machinery) lives in the Layer-0 [`conduit_k8s`] crate
//! (issue #114/#249) — it cannot know about [`AppConfig`]/[`SiteConfig`]
//! without creating a dependency cycle (see `CONTRIBUTING.md`'s crate
//! extraction recipe, rule 2: "generic-in-crate, bound-by-type-alias-in-
//! root"). This module binds the generic mechanism to conduit's real schema
//! via [`ConduitSchema`] and re-exports everything at its original path so
//! `crate::config::kubernetes::*` call sites (`src/cli/serve.rs`) don't change.

use anyhow::Result;

use conduit_k8s::CrdConfigBuilder;

use crate::config::schema::{AdminConfig, AppConfig, GlobalConfig, SiteConfig};

pub use conduit_k8s::{ConduitSite, ConduitSiteSpec, ConduitSiteStatus};

/// Binds [`conduit_k8s::CrdConfigBuilder`] to conduit's real config schema.
///
/// `site_from_spec` relies on [`ConduitSiteSpec`] using the same field names
/// (and serde renames) as [`SiteConfig`], so serializing to JSON and
/// deserializing as `SiteConfig` produces the correct result.
pub struct ConduitSchema;

impl CrdConfigBuilder for ConduitSchema {
    type Site = SiteConfig;
    type Config = AppConfig;

    fn site_from_spec(spec: &ConduitSiteSpec) -> Result<SiteConfig> {
        let json = serde_json::to_value(spec)?;
        let site: SiteConfig = serde_json::from_value(json)?;
        Ok(site)
    }

    fn build_config(sites: Vec<SiteConfig>, admin_bind: &str) -> AppConfig {
        AppConfig {
            global: Some(GlobalConfig {
                admin: Some(AdminConfig {
                    bind: Some(admin_bind.to_owned()),
                    token: None,
                }),
                ..Default::default()
            }),
            sites,
        }
    }
}

/// Stream [`AppConfig`] updates from `ConduitSite` Kubernetes CRDs.
///
/// See [`conduit_k8s::KubernetesProvider`] for the generic mechanism
/// (list+watch, namespace handling, error recovery).
pub type KubernetesProvider = conduit_k8s::KubernetesProvider<ConduitSchema>;

/// Convert a [`ConduitSiteSpec`] to a [`SiteConfig`] via JSON round-trip.
///
/// Thin wrapper over [`ConduitSchema::site_from_spec`] kept at its original
/// name and signature (recipe rule 3) — `conduit_k8s::build_app_config` is
/// generic over the schema binding, so a direct re-export can't preserve the
/// original turbofish-free call signature the way this wrapper does.
pub fn spec_to_site_config(spec: &ConduitSiteSpec) -> Result<SiteConfig> {
    ConduitSchema::site_from_spec(spec)
}

/// Build an [`AppConfig`] from a list of `ConduitSite` CRDs.
///
/// Thin wrapper over [`conduit_k8s::build_app_config`] bound to
/// [`ConduitSchema`] — same reasoning as [`spec_to_site_config`] above.
pub fn build_app_config<'a>(
    sites: impl Iterator<Item = &'a ConduitSite>,
    admin_bind: &str,
) -> Result<AppConfig> {
    conduit_k8s::build_app_config::<ConduitSchema>(sites, admin_bind)
}

// ── Tests ─────────────────────────────────────────────────────────────────────
//
// These cover the *schema-specific* behavior of `ConduitSchema`'s JSON
// round-trip against the real `SiteConfig`/`AppConfig` types — the generic
// list+watch mechanism's own tests (constructor/builder fields, generic
// error-attribution) moved to `crates/conduit-k8s/src/provider.rs`, which
// can't test against the real schema without creating a dependency cycle.

#[cfg(test)]
mod tests {
    use super::*;

    fn make_spec(port: u16, host: Option<&str>) -> ConduitSiteSpec {
        ConduitSiteSpec {
            port: Some(port),
            host: host.map(str::to_owned),
            proxy: None,
            static_files: None,
            health_check: None,
            tls: None,
            headers: None,
            fallback: None,
            logging: None,
            compression: None,
            rate_limit: None,
            basic_auth: None,
            api_key: None,
            ip_filter: None,
            cors: None,
            metrics: None,
            upload: None,
            redirects: None,
            middleware: None,
            routes: None,
        }
    }

    fn make_crd(name: &str, spec: ConduitSiteSpec) -> ConduitSite {
        ConduitSite::new(name, spec)
    }

    // ── spec_to_site_config ───────────────────────────────────────────────────

    #[test]
    fn spec_to_site_config_port_and_host() {
        let spec = make_spec(8080, Some("app.example.com"));
        let site = spec_to_site_config(&spec).expect("conversion must succeed");
        assert_eq!(site.port, Some(8080));
        assert_eq!(site.host.as_deref(), Some("app.example.com"));
    }

    #[test]
    fn spec_to_site_config_minimal_defaults() {
        let spec = make_spec(3000, None);
        let site = spec_to_site_config(&spec).expect("conversion must succeed");
        assert_eq!(site.port, Some(3000));
        assert!(site.host.is_none());
        assert!(site.proxy.is_none());
        assert!(site.health_check.is_none());
    }

    #[test]
    fn spec_to_site_config_with_health_check() {
        let mut spec = make_spec(8080, None);
        spec.health_check = Some(serde_json::json!(true));
        let site = spec_to_site_config(&spec).expect("conversion must succeed");
        assert!(site.health_check.is_some());
    }

    #[test]
    fn spec_to_site_config_with_proxy_string() {
        let mut spec = make_spec(8080, None);
        spec.proxy = Some(serde_json::json!("http://backend:4000"));
        let site = spec_to_site_config(&spec).expect("conversion must succeed");
        assert!(site.proxy.is_some());
    }

    // ── build_app_config ──────────────────────────────────────────────────────

    #[test]
    fn build_app_config_empty_produces_empty_sites() {
        let cfg =
            build_app_config(std::iter::empty(), "127.0.0.1:2019").expect("build must succeed");
        assert!(cfg.sites.is_empty());
        assert!(cfg.global.is_some());
    }

    #[test]
    fn build_app_config_multiple_sites() {
        let sites = [
            make_crd("site-a", make_spec(8080, Some("a.example.com"))),
            make_crd("site-b", make_spec(8081, Some("b.example.com"))),
        ];
        let cfg = build_app_config(sites.iter(), "127.0.0.1:2019").expect("build must succeed");
        assert_eq!(cfg.sites.len(), 2);
        assert_eq!(cfg.sites[0].port, Some(8080));
        assert_eq!(cfg.sites[1].port, Some(8081));
    }

    #[test]
    fn build_app_config_sets_admin_bind() {
        let cfg = build_app_config(std::iter::empty(), "0.0.0.0:9090").expect("build must succeed");
        let bind = cfg
            .global
            .as_ref()
            .and_then(|g| g.admin.as_ref())
            .and_then(|a| a.bind.as_deref());
        assert_eq!(bind, Some("0.0.0.0:9090"));
    }

    // ── KubernetesProvider ────────────────────────────────────────────────────

    #[test]
    fn kubernetes_provider_name_is_kubernetes() {
        use conduit_config_core::provider::Provider as _;
        assert_eq!(KubernetesProvider::new("default").name(), "kubernetes");
    }

    #[test]
    fn kubernetes_provider_stores_namespace() {
        let p = KubernetesProvider::new("my-ns");
        assert_eq!(p.namespace, "my-ns");
    }

    #[test]
    fn kubernetes_provider_with_admin_bind() {
        let p = KubernetesProvider::new("default").with_admin_bind("0.0.0.0:2019");
        assert_eq!(p.admin_bind, "0.0.0.0:2019");
    }
}
