use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

use crate::{
    apis::postgres_parameters::PgConfig,
    defaults::default_image,
    extensions::types::{Extension, TrunkInstall},
    postgres_exporter::QueryConfig,
    stacks::config_engines::{olap_config_engine, standard_config_engine, ConfigEngine},
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

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, ToSchema)]
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
    // external application services
    pub services: Option<Vec<Service>>,
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

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, ToSchema)]
pub struct Infrastructure {
    // generic specs
    pub cpu: String,
    pub memory: String,
    pub storage: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq)]
pub struct ComputeTemplate {
    pub cpu: String,
    pub memory: String,
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
    use crate::stacks::{get_stack, types::Infrastructure, StackType};

    #[test]
    fn test_stacks_definitions() {
        let mut mq = get_stack(StackType::MessageQueue);
        let infra = Infrastructure {
            cpu: "1".to_string(),
            memory: "1Gi".to_string(),
            storage: "10Gi".to_string(),
        };
        mq.infrastructure = Some(infra);

        // testing the default instance configurations
        let runtime_configs = mq.runtime_config().expect("expected configs");
        assert_eq!(runtime_configs[0].name, "shared_buffers");
        assert_eq!(runtime_configs[0].value.to_string(), "256MB");
        assert_eq!(runtime_configs[1].name, "max_connections");
        assert_eq!(runtime_configs[1].value.to_string(), "107");
        assert!(mq.postgres_metrics.is_some());
        assert!(mq.postgres_config.is_some());
        let mq_metrics = mq.postgres_metrics.unwrap();
        assert_eq!(mq_metrics.queries.len(), 1);
        assert!(mq_metrics.queries.contains_key("pgmq"));
        assert!(mq_metrics.queries["pgmq"].master);
        assert_eq!(mq_metrics.queries["pgmq"].metrics.len(), 5);

        let mut std = get_stack(StackType::Standard);
        let infra = Infrastructure {
            cpu: "1".to_string(),
            memory: "2Gi".to_string(),
            storage: "10Gi".to_string(),
        };
        std.infrastructure = Some(infra);
        println!("STD: {:#?}", std);

        let runtime_configs = std.runtime_config().expect("expected configs");
        assert_eq!(runtime_configs[0].name, "shared_buffers");
        assert_eq!(runtime_configs[0].value.to_string(), "512MB");
    }
}
