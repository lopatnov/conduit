//! `KubernetesProvider<B>` — list+watch `ConduitSite` CRDs and stream config
//! updates through a [`conduit_config_core::provider::Provider`], generic
//! over the config schema via [`CrdConfigBuilder`].

use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;

use futures::StreamExt as _;
use kube::api::{Patch, PatchParams};
use kube::runtime::watcher::{watcher, Config as WatcherConfig, Event};
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

        // Build and send the initial config from the current list of CRDs. A site that does not
        // convert is left out and logged; it does not stop the others.
        let mut set = SiteSet::<B>::new();
        let list = api.list(&Default::default()).await?;
        let (cfg, reports) = set.build(list.items.iter(), &self.admin_bind);
        let mut status = StatusWriter::default();
        status.write(&client, &list.items, &reports).await;
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
                Ok(event) if triggers_rebuild(&event) => {
                    if !handle_watch_event::<B>(
                        &client,
                        &self.namespace,
                        &self.admin_bind,
                        &tx,
                        &mut set,
                        &mut status,
                    )
                    .await
                    {
                        return Ok(());
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "ConduitSite watch error");
                }
            }
        }

        Ok(())
    }
}

// ── Internal watch helpers ────────────────────────────────────────────────────

/// Whether a watch event should trigger a re-list and config rebuild.
///
/// A (re)start of the watch streams `Init`, one `InitApply` per existing object, then
/// `InitDone`. Only `InitDone` marks a complete resync, so rebuilding on `Init`/`InitApply`
/// would run the list+rebuild+send cycle M+2 times for M sites at every start and recovery (#408).
fn triggers_rebuild<K>(event: &Event<K>) -> bool {
    matches!(event, Event::Apply(_) | Event::Delete(_) | Event::InitDone)
}

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
    set: &mut SiteSet<B>,
    status: &mut StatusWriter,
) -> bool {
    // Re-list to get a consistent snapshot after any change.
    // In production this could be optimised to maintain an in-memory cache
    // updated by the events.
    let api: Api<ConduitSite> = make_api(client, namespace);
    match api.list(&Default::default()).await {
        Ok(list) => {
            let (cfg, reports) = set.build(list.items.iter(), admin_bind);
            status.write(client, &list.items, &reports).await;
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

/// The sites built from `ConduitSite` objects, one object at a time.
///
/// An object whose spec does not convert (a host path, a field of the wrong type, a hard validation error of the
/// site itself) must not hold back the other sites: it is logged and left out. When it had converted before, its last
/// good version stays in the config instead, so a bad edit of a working site does not take that site down.
/// What no single site can show (two sites with one host and port) is still checked on the whole config by the
/// consumer of the channel, which then keeps the config it has.
pub struct SiteSet<B: CrdConfigBuilder> {
    last_good: HashMap<String, B::Site>,
}

impl<B: CrdConfigBuilder> Default for SiteSet<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B: CrdConfigBuilder> SiteSet<B> {
    /// An empty set: nothing has converted yet.
    pub fn new() -> Self {
        Self {
            last_good: HashMap::new(),
        }
    }

    /// Build the full config from every object of the list, in the order given, and say what became of each object.
    pub fn build<'a>(
        &mut self,
        crds: impl Iterator<Item = &'a ConduitSite>,
        admin_bind: &str,
    ) -> (B::Config, Vec<SiteReport>) {
        let mut sites: Vec<B::Site> = Vec::new();
        let mut reports: Vec<SiteReport> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for crd in crds {
            let name = crd.metadata.name.as_deref().unwrap_or("<unnamed>");
            let namespace = crd.metadata.namespace.as_deref().unwrap_or("");
            let key = format!("{namespace}/{name}");
            let report = |ready: bool, message: String| SiteReport {
                namespace: namespace.to_owned(),
                name: name.to_owned(),
                ready,
                message: message.chars().take(500).collect(),
            };
            match B::site_from_spec(&crd.spec) {
                Ok(site) => {
                    self.last_good.insert(key.clone(), site.clone());
                    sites.push(site);
                    reports.push(report(true, "Active".to_owned()));
                }
                Err(e) => match self.last_good.get(&key) {
                    Some(previous) => {
                        tracing::error!(
                            site = %key,
                            error = %e,
                            "ConduitSite {name} is invalid — keeping its previous version"
                        );
                        sites.push(previous.clone());
                        reports.push(report(
                            false,
                            format!("Invalid, still serving the previous version: {e}"),
                        ));
                    }
                    None => {
                        tracing::error!(
                            site = %key,
                            error = %e,
                            "ConduitSite {name} is invalid — left out of the config"
                        );
                        reports.push(report(false, format!("Invalid, not active: {e}")));
                    }
                },
            }
            seen.insert(key);
        }
        // An object that is gone from the list takes its last good version with it.
        self.last_good.retain(|key, _| seen.contains(key));
        (B::build_config(sites, admin_bind), reports)
    }
}

/// What became of one `ConduitSite` object in a build: the value written to its `status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteReport {
    /// The namespace of the object.
    pub namespace: String,
    /// The name of the object.
    pub name: String,
    /// `true` when the object's own spec is the one in the config.
    pub ready: bool,
    /// `Active`, or why the spec is not in the config. It holds no time, so that an unchanged state is an unchanged status.
    pub message: String,
}

/// Writes the `status` of the objects (`ready`, `message`), only where it differs from what the object holds, so that
/// a write does not start another round of the watch for ever.
#[derive(Default)]
pub struct StatusWriter {
    /// The API server refused a write once (no `patch` on `conduitsites/status`); it is said once and not tried again.
    denied: bool,
}

impl StatusWriter {
    async fn write(&mut self, client: &Client, crds: &[ConduitSite], reports: &[SiteReport]) {
        if self.denied {
            return;
        }
        for report in reports {
            let current = crds
                .iter()
                .find(|c| {
                    c.metadata.name.as_deref() == Some(report.name.as_str())
                        && c.metadata.namespace.as_deref().unwrap_or("") == report.namespace
                })
                .and_then(|c| c.status.as_ref());
            if current.is_some_and(|st| {
                st.ready == Some(report.ready) && st.message.as_deref() == Some(&report.message)
            }) {
                continue;
            }
            let api: Api<ConduitSite> = Api::namespaced(client.clone(), &report.namespace);
            let patch = serde_json::json!({
                "status": { "ready": report.ready, "message": report.message }
            });
            if let Err(e) = api
                .patch_status(&report.name, &PatchParams::default(), &Patch::Merge(&patch))
                .await
            {
                if matches!(&e, kube::Error::Api(r) if r.code == 403 || r.code == 404) {
                    tracing::warn!(
                        "cannot write the status of ConduitSite objects: grant `patch` on `conduitsites/status` \
                         to the service account and apply the current contrib/k8s/conduitsite-crd.yaml, to see the \
                         state of each site in `kubectl get conduitsites`"
                    );
                    self.denied = true;
                    return;
                }
                tracing::warn!(site = %report.name, error = %e, "failed to write the status of a ConduitSite");
            }
        }
    }
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

    /// #408: a resync's `Init` and per-object `InitApply` events must not each trigger a
    /// full re-list; only the events that change state, or complete the resync, do.
    #[test]
    fn only_state_changes_and_the_end_of_a_resync_trigger_a_rebuild() {
        assert!(triggers_rebuild(&Event::Apply(1)));
        assert!(triggers_rebuild(&Event::Delete(1)));
        assert!(triggers_rebuild::<u8>(&Event::InitDone));
        assert!(!triggers_rebuild::<u8>(&Event::Init));
        assert!(!triggers_rebuild(&Event::InitApply(1)));
    }

    /// Minimal `CrdConfigBuilder` binding used only by this crate's own
    /// tests — proves the generic mechanism works for *some* schema without
    /// depending on the root crate's real one.
    struct TestSchema;

    #[derive(Clone, Debug, PartialEq)]
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
            extra: Default::default(),
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

    // ── SiteSet: one bad object does not hold back the others ───────────────

    /// A schema that refuses port 0, as a stand-in for a spec that does not convert.
    struct PickySchema;

    impl CrdConfigBuilder for PickySchema {
        type Site = TestSite;
        type Config = Vec<TestSite>;

        fn site_from_spec(spec: &crate::ConduitSiteSpec) -> Result<Self::Site> {
            anyhow::ensure!(spec.port != Some(0), "port 0");
            Ok(TestSite { port: spec.port })
        }

        fn build_config(sites: Vec<Self::Site>, _admin_bind: &str) -> Self::Config {
            sites
        }
    }

    fn ports(sites: &[TestSite]) -> Vec<Option<u16>> {
        sites.iter().map(|s| s.port).collect()
    }

    #[test]
    fn an_invalid_site_is_left_out_and_the_others_are_built() {
        let crds = [
            make_crd("a", make_spec(8080)),
            make_crd("bad", make_spec(0)),
            make_crd("b", make_spec(8081)),
        ];
        let (built, reports) = SiteSet::<PickySchema>::new().build(crds.iter(), "x");
        assert_eq!(ports(&built), [Some(8080), Some(8081)]);
        let state: Vec<(&str, bool)> = reports.iter().map(|r| (r.name.as_str(), r.ready)).collect();
        assert_eq!(state, [("a", true), ("bad", false), ("b", true)]);
        assert_eq!(reports[0].message, "Active");
        assert!(reports[1].message.contains("not active") && reports[1].message.contains("port 0"));
    }

    #[test]
    fn a_bad_edit_of_a_working_site_keeps_its_previous_version() {
        let mut set = SiteSet::<PickySchema>::new();
        let good = [
            make_crd("a", make_spec(8080)),
            make_crd("b", make_spec(8081)),
        ];
        assert_eq!(
            ports(&set.build(good.iter(), "x").0),
            [Some(8080), Some(8081)]
        );
        // "a" is edited into something that does not convert
        let edited = [make_crd("a", make_spec(0)), make_crd("b", make_spec(8082))];
        let (built, reports) = set.build(edited.iter(), "x");
        assert_eq!(
            ports(&built),
            [Some(8080), Some(8082)],
            "a keeps 8080, b takes its new value"
        );
        assert!(!reports[0].ready && reports[0].message.contains("previous version"));
        assert!(reports[1].ready);
    }

    #[test]
    fn a_deleted_site_does_not_leave_its_last_good_version_behind() {
        let mut set = SiteSet::<PickySchema>::new();
        let good = [make_crd("a", make_spec(8080))];
        set.build(good.iter(), "x");
        set.build(std::iter::empty(), "x"); // "a" is deleted
        let bad_again = [make_crd("a", make_spec(0))];
        assert!(
            set.build(bad_again.iter(), "x").0.is_empty(),
            "a new object named a must not inherit the deleted one's version"
        );
    }
}
