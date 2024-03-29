use crate::{
    apis::postgres_parameters::PgConfig,
    app_service::types::AppService,
    defaults::default_image,
    extensions::types::{Extension, TrunkInstall},
    postgres_exporter::QueryConfig,
    stacks::config_engines::{mq_config_engine, olap_config_engine, standard_config_engine, ConfigEngine},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;


#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, ToSchema)]
pub enum StackType {
    DataWarehouse,
    Standard,
    MessageQueue,
    MachineLearning,
    OLAP,
    #[default]
    OLTP,
    VectorDB,
}

impl std::str::FromStr for StackType {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "DataWarehouse" => Ok(StackType::DataWarehouse),
            "Standard" => Ok(StackType::Standard),
            "MessageQueue" => Ok(StackType::MessageQueue),
            "MachineLearning" => Ok(StackType::MachineLearning),
            "OLAP" => Ok(StackType::OLAP),
            "OLTP" => Ok(StackType::OLTP),
            "VectorDB" => Ok(StackType::VectorDB),
            _ => Err("invalid value"),
        }
    }
}

impl StackType {
    pub fn as_str(&self) -> &str {
        match self {
            StackType::DataWarehouse => "DataWarehouse",
            StackType::Standard => "Standard",
            StackType::MessageQueue => "MessageQueue",
            StackType::MachineLearning => "MachineLearning",
            StackType::OLAP => "OLAP",
            StackType::OLTP => "OLTP",
            StackType::VectorDB => "VectorDB",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, ToSchema)]
pub struct Stack {
    pub name: String,
    pub compute_templates: Option<Vec<ComputeTemplate>>,
    pub description: Option<String>,
    #[serde(default = "default_stack_image")]
    pub image: Option<String>,
    pub stack_version: Option<String>,
    pub trunk_installs: Option<Vec<TrunkInstall>>,
    pub extensions: Option<Vec<Extension>>,
    // postgres metric definition specific to the Stack
    pub postgres_metrics: Option<QueryConfig>,
    // configs are strongly typed so that they can be programmatically transformed
    pub postgres_config: Option<Vec<PgConfig>>,
    #[serde(default = "default_config_engine")]
    pub postgres_config_engine: Option<ConfigEngine>,
    // external application services
    pub infrastructure: Option<Infrastructure>,
    #[serde(rename = "appServices")]
    pub app_services: Option<Vec<AppService>>,
}

impl Stack {
    // https://www.postgresql.org/docs/current/runtime-config-resource.html#RUNTIME-CONFIG-RESOURCE-MEMORY
    pub fn runtime_config(&self) -> Option<Vec<PgConfig>> {
        match &self.postgres_config_engine {
            Some(ConfigEngine::Standard) => Some(standard_config_engine(self)),
            Some(ConfigEngine::OLAP) => Some(olap_config_engine(self)),
            Some(ConfigEngine::MQ) => Some(mq_config_engine(self)),
            None => Some(standard_config_engine(self)),
        }
    }
}

fn default_stack_image() -> Option<String> {
    Some(default_image())
}

fn default_config_engine() -> Option<ConfigEngine> {
    Some(ConfigEngine::Standard)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, ToSchema)]
pub struct Infrastructure {
    // generic specs
    #[serde(default = "default_cpu")]
    pub cpu: String,
    #[serde(default = "default_memory")]
    pub memory: String,
    #[serde(default = "default_storage")]
    pub storage: String,
}

fn default_cpu() -> String {
    "1".to_owned()
}

fn default_memory() -> String {
    "1Gi".to_owned()
}

fn default_storage() -> String {
    "10Gi".to_owned()
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq)]
pub struct ComputeTemplate {
    pub cpu: String,
    pub memory: String,
}

#[cfg(test)]
mod tests {
    use super::*;
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
        // convert to vec to hashmap because order is not guaranteed
        let hm: std::collections::HashMap<String, PgConfig> =
            runtime_configs.into_iter().map(|c| (c.name.clone(), c)).collect();
        let shared_buffers = hm.get("shared_buffers").unwrap();
        assert_eq!(shared_buffers.name, "shared_buffers");
        assert_eq!(shared_buffers.value.to_string(), "614MB");
        let max_connections = hm.get("max_connections").unwrap();
        assert_eq!(max_connections.name, "max_connections");
        assert_eq!(max_connections.value.to_string(), "107");
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
        let hm: std::collections::HashMap<String, PgConfig> =
            runtime_configs.into_iter().map(|c| (c.name.clone(), c)).collect();
        let shared_buffers = hm.get("shared_buffers").unwrap();
        assert_eq!(shared_buffers.name, "shared_buffers");
        assert_eq!(shared_buffers.value.to_string(), "512MB");
    }
}
