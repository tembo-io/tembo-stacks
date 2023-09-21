use crate::ingress_route_tcp_crd::IngressRouteTCPSpec;
use k8s_openapi::api::core::v1::ResourceRequirements;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, str::FromStr};
use utoipa::ToSchema;

// defines a app container
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, JsonSchema)]
pub struct AppService {
    pub name: String,
    pub image: String,
    pub args: Option<Vec<String>>,
    pub command: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>,
    pub ports: Option<Vec<PortMapping>>,
    pub resources: Option<ResourceRequirements>,
    pub probes: Option<Probes>,
    pub metrics: Option<Metrics>,
    pub ingress: Option<IngressRouteTCPSpec>,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, JsonSchema)]
pub struct PortMapping {
    pub host: u16,
    pub container: u16,
}

// this allows us to construct ports as a Vec<String>, then work with them as a Vec<PortMapping>
impl FromStr for PortMapping {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 2 {
            return Err("Invalid format");
        }

        let host = parts[0].parse::<u16>().map_err(|_| "Invalid host port")?;
        let container = parts[1].parse::<u16>().map_err(|_| "Invalid container port")?;

        Ok(PortMapping { host, container })
    }
}

fn main() {
    let input = vec!["8080:8080", "8081:8081"];
    let port_mappings: Result<Vec<PortMapping>, &str> = input.iter().map(|s| s.parse()).collect();

    match port_mappings {
        Ok(mappings) => println!("{:?}", mappings),
        Err(e) => println!("Error: {}", e),
    }
}

#[allow(non_snake_case)]
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq)]
pub struct Metrics {
    enabled: bool,
    port: String,
    path: String,
}

#[allow(non_snake_case)]
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq)]
pub struct Probes {
    pub readiness: Probe,
    pub liveness: Probe,
}

#[allow(non_snake_case)]
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq)]
pub struct Probe {
    pub path: String,
    pub port: String,
    // this should never be negative
    pub initial_delay_seconds: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_port_mapping() {
        let input = vec!["8080:8080", "8081:8081"];
        let port_mappings: Result<Vec<PortMapping>, &str> = input.iter().map(|s| s.parse()).collect();

        match port_mappings {
            Ok(mappings) => println!("{:?}", mappings),
            Err(e) => println!("Error: {}", e),
        }
    }
}
