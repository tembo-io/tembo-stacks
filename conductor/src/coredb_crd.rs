use kube::CustomResource;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug)]
#[kube(
    group = "coredb.io",
    version = "v1alpha1",
    kind = "CoreDB",
    plural = "coredbs"
)]
#[kube(namespaced)]
#[kube(status = "CoreDBStatus")]
#[kube(schema = "disabled")]
pub struct CoreDBSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<CoreDBExtensions>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<i32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "postgresExporterEnabled"
    )]
    pub postgres_exporter_enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "postgresExporterImage"
    )]
    pub postgres_exporter_image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<CoreDBResources>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<i32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, Hash, PartialEq)]
pub struct CoreDBExtensions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub locations: Vec<CoreDBExtensionsLocations>,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct CoreDBExtensionsLocations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CoreDBResources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests: Option<BTreeMap<String, String>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CoreDBStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<CoreDBExtensions>>,
    #[serde(rename = "extensionsUpdating")]
    pub extensions_updating: bool,
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
}
