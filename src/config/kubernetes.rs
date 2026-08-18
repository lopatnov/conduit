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

use anyhow::Result;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use futures::StreamExt as _;
use kube::runtime::watcher::{watcher, Config as WatcherConfig};
use kube::{Api, Client, CustomResource};

use crate::config::provider::Provider;
use crate::config::schema::{AdminConfig, AppConfig, GlobalConfig, SiteConfig};

// ── Custom Resource Definition ────────────────────────────────────────────────

/// Spec of a `ConduitSite` Kubernetes custom resource.
///
/// Field names match the Conduit JSON config schema so that the spec can be
/// round-tripped through `serde_json` into a [`SiteConfig`].
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "conduit.io",
    version = "v1",
    kind = "ConduitSite",
    namespaced,
    status = "ConduitSiteStatus",
    shortname = "cs",
    printcolumn = r#"{"name":"Port","type":"integer","jsonPath":".spec.port"}"#,
    printcolumn = r#"{"name":"Host","type":"string","jsonPath":".spec.host"}"#
)]
pub struct ConduitSiteSpec {
    /// TCP port to listen on. Default: 80 (HTTP) or 443 (HTTPS).
    pub port: Option<u16>,

    /// Virtual host for request matching. Omit for catch-all.
    pub host: Option<String>,

    /// Upstream proxy target(s). Accepts the same values as `proxy` in
    /// conduit.json: a URL string, array of URLs, or a route map object.
    pub proxy: Option<serde_json::Value>,

    /// Static file directory path (equivalent to `static` in conduit.json).
    #[serde(rename = "static")]
    pub static_files: Option<serde_json::Value>,

    /// Enable the `/__health__` health-check endpoint.
    #[serde(rename = "healthCheck")]
    pub health_check: Option<serde_json::Value>,

    /// TLS configuration (cert, key, acme, httpRedirectPort).
    pub tls: Option<serde_json::Value>,

    /// Custom response headers added to every response.
    pub headers: Option<serde_json::Value>,

    /// Fallback response when no route matches.
    pub fallback: Option<serde_json::Value>,

    /// Access logging configuration.
    pub logging: Option<serde_json::Value>,

    /// Response compression.
    pub compression: Option<serde_json::Value>,

    /// Token-bucket rate limiting.
    #[serde(rename = "rateLimit")]
    pub rate_limit: Option<serde_json::Value>,

    /// HTTP Basic authentication.
    #[serde(rename = "basicAuth")]
    pub basic_auth: Option<serde_json::Value>,

    /// API-key authentication.
    #[serde(rename = "apiKey")]
    pub api_key: Option<serde_json::Value>,

    /// IP allow/deny filter.
    #[serde(rename = "ipFilter")]
    pub ip_filter: Option<serde_json::Value>,

    /// CORS configuration.
    pub cors: Option<serde_json::Value>,

    /// Prometheus metrics endpoint.
    pub metrics: Option<serde_json::Value>,

    /// File upload endpoint.
    pub upload: Option<serde_json::Value>,

    /// Redirect rules.
    pub redirects: Option<serde_json::Value>,

    /// Rhai scripting middleware.
    pub middleware: Option<serde_json::Value>,

    /// Advanced routes array.
    pub routes: Option<serde_json::Value>,
}

/// Status subresource written back by the Conduit controller.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Default)]
pub struct ConduitSiteStatus {
    /// Human-readable summary (e.g. "Listening on :8080").
    pub message: Option<String>,
    /// Whether this site is currently active.
    pub ready: Option<bool>,
}

// ── Provider ──────────────────────────────────────────────────────────────────

/// Stream [`AppConfig`] updates from `ConduitSite` Kubernetes CRDs.
///
/// Connects to the current cluster (via `KUBECONFIG` or in-cluster service
/// account), lists all `ConduitSite` resources in the configured namespace,
/// and watches for changes.  Every add/modify/delete event rebuilds the full
/// [`AppConfig`] and sends it on the channel.
pub struct KubernetesProvider {
    /// Kubernetes namespace to watch. Use `"default"` for the default namespace
    /// or `"*"` to watch all namespaces.
    pub namespace: String,
    /// Address for the Admin API (forwarded into the generated GlobalConfig).
    pub admin_bind: String,
}

impl KubernetesProvider {
    /// Create a provider that watches the given namespace.
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            admin_bind: "127.0.0.1:2019".to_owned(),
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
impl Provider<AppConfig> for KubernetesProvider {
    fn name(&self) -> &'static str {
        "kubernetes"
    }

    async fn run(&self, tx: mpsc::Sender<AppConfig>) -> Result<()> {
        let client = Client::try_default().await?;
        let api: Api<ConduitSite> = make_api(&client, &self.namespace);

        // Build and send the initial config from the current list of CRDs.
        let list = api.list(&Default::default()).await?;
        let cfg = build_app_config(list.items.iter(), &self.admin_bind)?;
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
                    if !handle_watch_event(&client, &self.namespace, &self.admin_bind, &tx).await {
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

/// Re-list all `ConduitSite` CRDs and send a fresh [`AppConfig`] on the channel.
///
/// Returns `true` if the update was sent (or was a no-op), `false` when the
/// receiver has been dropped and the caller should shut down.
async fn handle_watch_event(
    client: &Client,
    namespace: &str,
    admin_bind: &str,
    tx: &mpsc::Sender<AppConfig>,
) -> bool {
    // Re-list to get a consistent snapshot after any change.
    // In production this could be optimised to maintain an in-memory cache
    // updated by the events.
    let api: Api<ConduitSite> = make_api(client, namespace);
    match api.list(&Default::default()).await {
        Ok(list) => {
            let cfg = match build_app_config(list.items.iter(), admin_bind) {
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

// ── CRD → AppConfig conversion ────────────────────────────────────────────────

/// Build an [`AppConfig`] from a list of `ConduitSite` CRDs.
///
/// Each spec is converted to a [`SiteConfig`] via a `serde_json` round-trip,
/// which works because `ConduitSiteSpec` field names mirror `SiteConfig`.
pub fn build_app_config<'a>(
    sites: impl Iterator<Item = &'a ConduitSite>,
    admin_bind: &str,
) -> Result<AppConfig> {
    let mut site_configs: Vec<SiteConfig> = Vec::new();

    for crd in sites {
        let site = spec_to_site_config(&crd.spec).map_err(|e| {
            anyhow::anyhow!(
                "ConduitSite '{}': {}",
                crd.metadata.name.as_deref().unwrap_or("<unnamed>"),
                e
            )
        })?;
        site_configs.push(site);
    }

    Ok(AppConfig {
        global: Some(GlobalConfig {
            admin: Some(AdminConfig {
                bind: Some(admin_bind.to_owned()),
                token: None,
            }),
            ..Default::default()
        }),
        sites: site_configs,
    })
}

/// Convert a [`ConduitSiteSpec`] to a [`SiteConfig`] via JSON round-trip.
///
/// This relies on `ConduitSiteSpec` using the same field names (and serde
/// renames) as `SiteConfig`, so serializing to JSON and deserializing as
/// `SiteConfig` produces the correct result.
pub fn spec_to_site_config(spec: &ConduitSiteSpec) -> Result<SiteConfig> {
    let json = serde_json::to_value(spec)?;
    let site: SiteConfig = serde_json::from_value(json)?;
    Ok(site)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

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
        let sites = vec![
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
