// WARNING: generated by kopium - manual changes will be overwritten
// kopium command: kopium --derive Default middlewaretcps.traefik.containo.us
// kopium version: 0.14.0

use kube::CustomResource;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, Default)]
#[kube(
    group = "traefik.containo.us",
    version = "v1alpha1",
    kind = "MiddlewareTCP",
    plural = "middlewaretcps"
)]
#[kube(namespaced)]
#[kube(schema = "disabled")]
pub struct MiddlewareTCPSpec {
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "inFlightConn")]
    pub in_flight_conn: Option<MiddlewareTCPInFlightConn>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "ipWhiteList")]
    pub ip_white_list: Option<MiddlewareTCPIpWhiteList>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MiddlewareTCPInFlightConn {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MiddlewareTCPIpWhiteList {
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sourceRange")]
    pub source_range: Option<Vec<String>>,
}
