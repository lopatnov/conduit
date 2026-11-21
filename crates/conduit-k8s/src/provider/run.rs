use anyhow::Result;
use async_trait::async_trait;
use conduit_config_core::provider::Provider;
use futures::StreamExt as _;
use kube::runtime::watcher::{watcher, Config as WatcherConfig};
use kube::{Api, Client};
use tokio::sync::mpsc;

use crate::CrdConfigBuilder;

use super::{
    handle_watch_event, make_api, triggers_rebuild, KubernetesProvider, SiteSet, StatusWriter,
    WatchRetry,
};
use crate::ConduitSite;

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
        let mut stream = watcher(api.clone(), WatcherConfig::default()).boxed();
        let mut retry = WatchRetry::default();
        while let Some(event) = stream.next().await {
            match event {
                Ok(event) if triggers_rebuild(&event) => {
                    retry.observe(&event);
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
                    tokio::time::sleep(retry.next_delay()).await;
                    // A failed stream may keep yielding the same transport error. Recreate the watcher so it
                    // performs a fresh list and resumes from the current resource version after connectivity returns.
                    stream = watcher(api.clone(), WatcherConfig::default()).boxed();
                }
            }
        }

        Ok(())
    }
}
