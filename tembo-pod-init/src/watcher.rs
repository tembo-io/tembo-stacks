use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::core::v1::Namespace;
use kube::api::{Api, ListParams, WatchEvent};
use kube::Client;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

use crate::config::Config;

pub struct NamespaceWatcher {
    namespaces: Arc<RwLock<HashSet<String>>>,
    client: Client,
    config: Config,
}

impl NamespaceWatcher {
    pub fn new(client: Client, config: Config) -> Self {
        Self {
            namespaces: Arc::new(RwLock::new(HashSet::new())),
            client,
            config,
        }
    }

    pub async fn watch(&self) -> Result<(), kube::Error> {
        let namespaces = self.namespaces.clone();
        let client = self.client.clone();
        let lp = ListParams::default().labels(&self.config.namespace_label);

        let api: Api<Namespace> = Api::all(client.clone());

        // Get all the namespaces and add the ones with the correct label
        let ns_list = api.list(&lp).await?;
        for ns in ns_list {
            if let Some(name) = ns.metadata.name {
                namespaces.write().await.insert(name.clone());
                debug!("Added namespaces: {}", name);
            }
        }

        // Disable the use to WatchParams for now until we can upgrade the kube-rs
        // crate to >= 0.83
        //let wp = WatchParams::default().labels(&self.config.namespace_label);
        let mut stream = api.watch(&lp, "0").await?.boxed();

        while let Some(status) = stream.try_next().await? {
            debug!("Got event: {:?}", status);
            match status {
                WatchEvent::Added(ns) | WatchEvent::Modified(ns) => {
                    let name = ns.metadata.name.clone().unwrap();
                    if ns.metadata.labels.is_some()
                        && ns
                            .metadata
                            .labels
                            .clone()
                            .expect("expected to find labels")
                            .contains_key(&self.config.namespace_label)
                        && ns.metadata.labels.expect("expected to find labels")
                            [&self.config.namespace_label]
                            == "true"
                    {
                        debug!("Added namespace: {}", name.clone());
                        namespaces.write().await.insert(name.clone());
                    } else {
                        debug!("Deleted namespace: {}", name.clone());
                        namespaces.write().await.remove(&name.clone());
                    }
                }
                WatchEvent::Deleted(ns) => {
                    let name = ns.metadata.name.clone().unwrap();
                    namespaces.write().await.remove(&name.clone());
                    debug!("Deleted namespace: {}", name);
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn get_namespaces(&self) -> Arc<RwLock<HashSet<String>>> {
        self.namespaces.clone()
    }
}
