use std::collections::BTreeMap;
use std::env;
use std::time::Duration;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, Client};
use kube::runtime::controller::Action;
use reqwest::Error;
use tracing::log::{error, warn};
use crate::configmap::apply_configmap;
use crate::controller;

const DEFAULT_TRUNK_REGISTRY_DOMAIN: &str = "registry.pgtrunk.io";

// One configmap per namespace
// multiple DBs in the same namespace can share the same configmap
const TRUNK_CONFIGMAP_NAME: &str = "trunk-metadata";

// This could be fetched from trunk
pub const MULTI_VAL_CONFIGS_PRIORITY_LIST: [&str; 2] = ["pg_stat_statements", "pg_stat_kcache"];

async fn update_extensions_libraries_list() -> Result<Vec<String>, reqwest::Error> {
    let domain = env::var("TRUNK_REGISTRY_DOMAIN").unwrap_or_else(|_| DEFAULT_TRUNK_REGISTRY_DOMAIN.to_string());
    let url = format!("https://{}/extensions/libraries", domain);

    let response = reqwest::get(&url).await?;

    if response.status().is_success() {
        let libraries: Vec<String> = serde_json::from_str(&response.text().await?)?;
        Ok(libraries)
    } else {
        Err(reqwest::Error::from_http_status(response.status()))
    }
}

pub async fn extensions_that_require_load(client: Client, namespace: &str) -> Result<Vec<&str>, kube::Error> {
    let cm_api: Api<ConfigMap> = Api::namespaced(client, namespace);

    // Get the ConfigMap
    let cm = cm_api.get(TRUNK_CONFIGMAP_NAME).await?;

    // Extract libraries from ConfigMap data
    match cm.data.and_then(|data| data.get("libraries")) {
        Some(libraries_str) => {
            let libraries: Vec<&str> = libraries_str.split(',').map(|s| s.to_string()).collect();
            Ok(libraries)
        },
        None => {
            // Handle the case where "libraries" key isn't present in the ConfigMap
            Err(kube::Error::Custom("Libraries key not found in the ConfigMap".into()))
        }
    }
}

pub async fn reconcile_trunk_configmap(client: Client, namespace: &str) -> Result<(), Action> {
    let libraries = match update_extensions_libraries_list().await{
        Ok(libraries) => {libraries}
        Err(e) => {
            error!("Failed to update extensions libraries list from trunk: {:?}", e);
            let cm_api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
            match cm_api.get(TRUNK_CONFIGMAP_NAME).await {
                Ok(_) => {
                    // If the configmap is already present, we can just log the error and continue
                    return Ok(());
                }
                Err(e) => {
                    // If the configmap is not already present, then we can requeue the request
                    return Err(Action::requeue(Duration::from_secs(30)))
                }
            }
        }
    };

    let mut data = BTreeMap::new();
    data.insert("libraries".to_string(), libraries.join(","));

    match apply_configmap(client, namespace, TRUNK_CONFIGMAP_NAME, data).await {
        Ok(_) => {
            Ok(())
        }
        Err(e) => {
            error!("Failed to update trunk configmap: {:?}", e);
            return Err(Action::requeue(Duration::from_secs(300)))
        }
    }
}