//! `KubernetesProvider<B>` — list+watch `ConduitSite` CRDs and stream config
//! updates through a [`conduit_config_core::provider::Provider`], generic
//! over the config schema via [`CrdConfigBuilder`].

use std::marker::PhantomData;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;

use futures::StreamExt as _;
use kube::runtime::watcher::{watcher, Config as WatcherConfig};
use kube::{Api, Client};

use conduit_config_core::provider::Provider;

use crate::{ConduitSite, CrdConfigBuilder};

// ── Provider ──────────────────────────────────────────────────────────────────

/// Stream config updates from `ConduitSite` Kubernetes CRDs.
///
/// Connects to the current cluster (via `KUBECONFIG` or in-cluster service
/// account), lists all `ConduitSite` resources in the configured namespace,
/// and watches for changes. Every add/modify/delete event rebuilds the full
/// config and sends it on the channel.
///
/// Generic over `B: CrdConfigBuilder` so this crate never names the real
/// config schema — see the crate-level doc comment.
pub struct KubernetesProvider<B: CrdConfigBuilder> {
    /// Kubernetes namespace to watch. Use `"default"` for the default namespace
    /// or `"*"` to watch all namespaces.
    pub namespace: String,
    /// Address for the Admin API (forwarded into the generated config).
    pub admin_bind: String,
    // `fn() -> B` (not bare `B`) keeps this struct unconditionally
    // `Send + Sync` regardless of `B` — a function pointer's phantom is
    // always Send + Sync, so this doesn't accidentally tie the provider's
    // own auto-trait bounds to whatever `B` happens to be (`B` itself is
    // already required to be `Send + Sync + 'static` by its own trait
    // bound, but that's a constraint on `B`, not on this struct's markers).
    _marker: PhantomData<fn() -> B>,
}

impl<B: CrdConfigBuilder> KubernetesProvider<B> {
    /// Create a provider that watches the given namespace.
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            admin_bind: "127.0.0.1:2019".to_owned(),
            _marker: PhantomData,
        }
    }

    /// Override the Admin API bind address in the generated config.
    #[must_use]
    pub fn with_admin_bind(mut self, bind: impl Into<String>) -> Self {
        self.admin_bind = bind.into();
        self
    }
}

#[async_trait]
impl<B: CrdConfigBuilder> Provider<B::Config> for KubernetesProvider<B> {
    fn name(&self) -> &'static str {
        "kubernetes"
    }

    async fn run(&self, tx: mpsc::Sender<B::Config>) -> Result<()> {
        let client = Client::try_default().await?;
        let api: Api<ConduitSite> = make_api(&client, &self.namespace);

        // Build and send the initial config from the current list of CRDs.
        let list = api.list(&Default::default()).await?;
        let cfg = build_app_config::<B>(list.items.iter(), &self.admin_bind)?;
        if tx.send(cfg).await.is_err() {
            return Ok(());
        }
        tracing::info!(
            provider = "kubernetes",
            namespace = %self.namespace,
            sites = list.items.len(),
            "initial config loaded from ConduitSite CRDs"
        );

        // Watch for changes and rebuild the full config on every event.
        let mut stream = watcher(api, WatcherConfig::default()).boxed();
        while let Some(event) = stream.next().await {
            match event {
                Ok(_) => {
                    if !handle_watch_event::<B>(&client, &self.namespace, &self.admin_bind, &tx)
                        .await
                    {
                        return Ok(());
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "ConduitSite watch error");
                }
            }
        }

        Ok(())
    }
}

// ── Internal watch helpers ────────────────────────────────────────────────────

/// Create a typed API client scoped to the given namespace.
/// Use `"*"` to watch all namespaces.
fn make_api(client: &Client, namespace: &str) -> Api<ConduitSite> {
    if namespace == "*" {
        Api::all(client.clone())
    } else {
        Api::namespaced(client.clone(), namespace)
    }
}

/// Re-list all `ConduitSite` CRDs and send a fresh config on the channel.
///
/// Returns `true` if the update was sent (or was a no-op), `false` when the
/// receiver has been dropped and the caller should shut down.
async fn handle_watch_event<B: CrdConfigBuilder>(
    client: &Client,
    namespace: &str,
    admin_bind: &str,
    tx: &mpsc::Sender<B::Config>,
) -> bool {
    // Re-list to get a consistent snapshot after any change.
    // In production this could be optimised to maintain an in-memory cache
    // updated by the events.
    let api: Api<ConduitSite> = make_api(client, namespace);
    match api.list(&Default::default()).await {
        Ok(list) => {
            let cfg = match build_app_config::<B>(list.items.iter(), admin_bind) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "failed to rebuild config from ConduitSite CRDs — keeping current config"
                    );
                    return true;
                }
            };
            tracing::info!(
                provider = "kubernetes",
                sites = list.items.len(),
                "config updated from ConduitSite CRDs"
            );
            tx.send(cfg).await.is_ok()
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to re-list ConduitSite CRDs after event");
            true
        }
    }
}

// ── CRD → config conversion ────────────────────────────────────────────────────

/// Build a full config value from a list of `ConduitSite` CRDs.
///
/// Each spec is converted to `B::Site` via [`CrdConfigBuilder::site_from_spec`]
/// (a `serde_json` round-trip in the root crate's real implementation), then
/// combined via [`CrdConfigBuilder::build_config`].
pub fn build_app_config<'a, B: CrdConfigBuilder>(
    sites: impl Iterator<Item = &'a ConduitSite>,
    admin_bind: &str,
) -> Result<B::Config> {
    let mut site_values: Vec<B::Site> = Vec::new();

    for crd in sites {
        let site = B::site_from_spec(&crd.spec).map_err(|e| {
            anyhow::anyhow!(
                "ConduitSite '{}': {}",
                crd.metadata.name.as_deref().unwrap_or("<unnamed>"),
                e
            )
        })?;
        site_values.push(site);
    }

    Ok(B::build_config(site_values, admin_bind))
}

// ── Tests ─────────────────────────────────────────────────────────────────────
//
// These cover the generic mechanism only (constructor/builder fields, and
// build_app_config's error-attribution wrapping) via a trivial test-only
// schema binding. Tests that exercise the *real* `SiteConfig`/`AppConfig`
// JSON round-trip (the previous `spec_to_site_config_*`/`build_app_config_*`
// tests covering field values) moved to the root crate's own
// `src/config/kubernetes.rs`, which is the only place that can construct a
// real `SiteConfig`/`AppConfig` without creating a dependency cycle.

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal `CrdConfigBuilder` binding used only by this crate's own
    /// tests — proves the generic mechanism works for *some* schema without
    /// depending on the root crate's real one.
    struct TestSchema;

    #[derive(Debug, PartialEq)]
    struct TestSite {
        port: Option<u16>,
    }

    #[derive(Debug)]
    struct TestConfig {
        sites: Vec<TestSite>,
        admin_bind: String,
    }

    impl CrdConfigBuilder for TestSchema {
        type Site = TestSite;
        type Config = TestConfig;

        fn site_from_spec(spec: &crate::ConduitSiteSpec) -> Result<Self::Site> {
            Ok(TestSite { port: spec.port })
        }

        fn build_config(sites: Vec<Self::Site>, admin_bind: &str) -> Self::Config {
            TestConfig {
                sites,
                admin_bind: admin_bind.to_owned(),
            }
        }
    }

    /// A schema binding whose `site_from_spec` always fails, to exercise
    /// `build_app_config`'s CRD-name error-attribution wrapping.
    struct FailingSchema;

    impl CrdConfigBuilder for FailingSchema {
        type Site = ();
        type Config = ();

        fn site_from_spec(_spec: &crate::ConduitSiteSpec) -> Result<Self::Site> {
            anyhow::bail!("boom")
        }

        fn build_config(_sites: Vec<Self::Site>, _admin_bind: &str) -> Self::Config {}
    }

    fn make_spec(port: u16) -> crate::ConduitSiteSpec {
        crate::ConduitSiteSpec {
            port: Some(port),
            host: None,
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

    fn make_crd(name: &str, spec: crate::ConduitSiteSpec) -> ConduitSite {
        ConduitSite::new(name, spec)
    }

    // ── KubernetesProvider (generic — independent of B) ─────────────────────

    #[test]
    fn kubernetes_provider_name_is_kubernetes() {
        assert_eq!(
            KubernetesProvider::<TestSchema>::new("default").name(),
            "kubernetes"
        );
    }

    #[test]
    fn kubernetes_provider_stores_namespace() {
        let p = KubernetesProvider::<TestSchema>::new("my-ns");
        assert_eq!(p.namespace, "my-ns");
    }

    #[test]
    fn kubernetes_provider_with_admin_bind() {
        let p = KubernetesProvider::<TestSchema>::new("default").with_admin_bind("0.0.0.0:2019");
        assert_eq!(p.admin_bind, "0.0.0.0:2019");
    }

    // ── build_app_config (generic mechanism) ────────────────────────────────

    #[test]
    fn build_app_config_empty_produces_empty_sites() {
        let cfg = build_app_config::<TestSchema>(std::iter::empty(), "127.0.0.1:2019")
            .expect("build must succeed");
        assert!(cfg.sites.is_empty());
        assert_eq!(cfg.admin_bind, "127.0.0.1:2019");
    }

    #[test]
    fn build_app_config_multiple_sites_preserves_order() {
        let crds = [
            make_crd("site-a", make_spec(8080)),
            make_crd("site-b", make_spec(8081)),
        ];
        let cfg = build_app_config::<TestSchema>(crds.iter(), "127.0.0.1:2019")
            .expect("build must succeed");
        assert_eq!(
            cfg.sites,
            vec![TestSite { port: Some(8080) }, TestSite { port: Some(8081) }]
        );
    }

    #[test]
    fn build_app_config_wraps_error_with_crd_name() {
        let crds = [make_crd("bad-site", make_spec(8080))];
        let err = build_app_config::<FailingSchema>(crds.iter(), "127.0.0.1:2019")
            .expect_err("must propagate site_from_spec's error");
        assert!(
            err.to_string().contains("bad-site"),
            "error must name the failing CRD: {err}"
        );
        assert!(
            err.to_string().contains("boom"),
            "error must include the cause: {err}"
        );
    }
}
