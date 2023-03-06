// WARNING: generated by kopium - manual changes will be overwritten
// kopium command: kopium coredbs.coredb.io
// kopium version: 0.14.0

use kube::CustomResource;
use serde::{Deserialize, Serialize};

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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "extensions"
    )]
    pub extensions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<i32>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CoreDBStatus {
    pub running: bool,
}
