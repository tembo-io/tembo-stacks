use crate::ingress_route_tcp_crd::IngressRouteTCPSpec;
use k8s_openapi::api::core::v1::{Probe, ResourceRequirements};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

// defines a app container
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, JsonSchema)]
pub struct AppService {
    pub image: String,
    pub args: Option<Vec<String>>,
    pub command: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>,
    pub ports: Option<Vec<String>>,
    pub resources: Option<ResourceRequirements>,
    pub probes: Option<Probe>,
    pub metrics: Option<Metrics>,
    pub ingress: Option<IngressRouteTCPSpec>,
}

#[allow(non_snake_case)]
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq)]
pub struct Metrics {
    enabled: bool,
    port: String,
    path: String,
}
