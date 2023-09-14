use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use schemars::JsonSchema;
use utoipa::ToSchema;

use crate::apis::postgres_parameters::PgConfig;
use crate::extensions::types::{Extension, TrunkInstall};
use crate::defaults::default_image;
use crate::postgres_exporter::QueryConfig;
use crate::stacks::config_engines::{
    standard_config_engine,
    olap_config_engine,
    ConfigEngine
};


#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct StackLeg {
    pub name: StackType,
    pub postgres_config: Option<Vec<PgConfig>>,
}


#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, ToSchema)]
pub enum StackType {
    Standard,
    MessageQueue,
    MachineLearning,
    OLAP,
    OLTP,
}


#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, ToSchema)]
pub struct Stack {
    pub name: String,
    pub compute_templates: Option<Vec<ComputeTemplate>>,
    pub description: Option<String>,
    #[serde(default = "default_image")]
    pub image: String,
    pub stack_version: String,
    pub trunk_installs: Option<Vec<TrunkInstall>>,
    pub extensions: Option<Vec<Extension>>,
    // postgres metric definition specific to the Stack
    pub postgres_metrics: Option<QueryConfig>,
    // configs are strongly typed so that they can be programmatically transformed
    pub postgres_config: Option<Vec<PgConfig>>,
    #[serde(default = "default_config_engine")]
    pub postgres_config_engine: Option<ConfigEngine>,
    // sidecar containers
    pub services: Option<Vec<Service>>,
    // kubernetes specific
    pub infrastructure: Option<Infrastructure>,
}


impl Stack {
    // https://www.postgresql.org/docs/current/runtime-config-resource.html#RUNTIME-CONFIG-RESOURCE-MEMORY
    pub fn runtime_config(&self) -> Option<Vec<PgConfig>> {
        match &self.postgres_config_engine {
            Some(ConfigEngine::Standard) => Some(standard_config_engine(self)),
            Some(ConfigEngine::OLAP) => Some(olap_config_engine(self)),
            None => Some(standard_config_engine(self)),
        }
    }
}


fn default_config_engine() -> Option<ConfigEngine> {
    Some(ConfigEngine::Standard)
}


#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq)]
pub struct ComputeTemplate {
    pub cpu: String,
    pub memory: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, ToSchema)]
pub struct Infrastructure {
    #[serde(default = "default_provider")]
    pub provider: CloudProvider,
    #[serde(default = "default_region")]
    pub region: String,

    // generic specs
    pub cpu: String,
    pub memory: String,
    pub storage_size: String,
}

fn default_provider() -> CloudProvider {
    CloudProvider::Aws
}

fn default_region() -> String {
    "us-east-1".to_owned()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloudProvider {
    #[default]
    Aws,
    // Gcp,
}

// storage class
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StorageClass {
    #[default]
    Gp3,
}

// e.g. ec2 instance type
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, ToSchema)]
pub enum InstanceTypes {
    #[default]
    GeneralPurpose,
}

// defines a sidecar container
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq)]
pub struct Service {
    pub image: String,
    pub command: String,
    pub ports: Vec<BTreeMap<String, String>>,
    pub config: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use crate::stacks::StackType;
    use crate::stacks::get_stack;

    #[test]
    fn test_stacks_definitions() {
        let mq = get_stack(StackType::MessageQueue);
        println!("MQ: {:#?}", mq);

        // testing the default instance configurations
        let runtime_configs = mq.runtime_config().expect("expected configs");
        assert_eq!(runtime_configs[0].name, "shared_buffers");
        assert_eq!(runtime_configs[0].value, "256MB");
        assert_eq!(runtime_configs[1].name, "max_connections");
        assert_eq!(runtime_configs[1].value, "107");
        assert!(mq.postgres_metrics.is_some());
        assert!(mq.postgres_config.is_some());
        let mq_metrics = mq.postgres_metrics.unwrap();
        assert_eq!(mq_metrics.queries.len(), 1);
        assert!(mq_metrics.queries.contains_key("pgmq"));
        assert!(mq_metrics.queries["pgmq"].master);
        assert_eq!(mq_metrics.queries["pgmq"].metrics.len(), 5);

        let std = get_stack(StackType::Standard);

        println!("STD: {:#?}", std);

        let runtime_configs = std.runtime_config().expect("expected configs");
        assert_eq!(runtime_configs[0].name, "shared_buffers");
        assert_eq!(runtime_configs[0].value, "512MB");
    }
}
