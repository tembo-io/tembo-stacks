use crate::{cloudnativepg, extensions::types::ExtensionStatus, stacks::types::Stack};
use k8s_openapi::{
    api::core::v1::ResourceRequirements,
    apimachinery::pkg::{api::resource::Quantity, apis::meta::v1::ObjectMeta},
};

use crate::{
    apis::postgres_parameters::{
        merge_pg_configs, MergeError, PgConfig, DISALLOWED_CONFIGS, MULTI_VAL_CONFIGS,
    },
    defaults,
    postgres_exporter::PostgresMetrics,
};
use kube::CustomResource;

use crate::{
    apis::postgres_parameters::ConfigValue,
    extensions::types::{Extension, TrunkInstall, TrunkInstallStatus},
};
use cloudnativepg::poolers::PoolerSpec;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, Default)]
pub struct ServiceAccountTemplate {
    pub metadata: Option<ObjectMeta>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, Default)]
#[allow(non_snake_case)]
pub struct Backup {
    #[serde(default = "defaults::default_destination_path")]
    pub destinationPath: Option<String>,
    #[serde(default = "defaults::default_encryption")]
    pub encryption: Option<String>,
    #[serde(default = "defaults::default_retention_policy")]
    pub retentionPolicy: Option<String>,
    #[serde(default = "defaults::default_backup_schedule")]
    pub schedule: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, Default)]
#[allow(non_snake_case)]
pub struct ConnPooler {
    pub enabled: Option<bool>,
    pub pooler: Option<PoolerSpec>,
}

/// Generate the Kubernetes wrapper struct `CoreDB` from our Spec and Status struct
///
/// This provides a hook for generating the CRD yaml (in crdgen.rs)
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(kind = "CoreDB", group = "coredb.io", version = "v1alpha1", namespaced)]
#[kube(status = "CoreDBStatus", shortname = "cdb")]
#[allow(non_snake_case)]
pub struct CoreDBSpec {
    #[serde(default = "defaults::default_replicas")]
    pub replicas: i32,

    #[serde(default = "defaults::default_resources")]
    pub resources: ResourceRequirements,

    #[serde(default = "defaults::default_storage")]
    pub storage: Quantity,

    #[serde(default = "defaults::default_sharedir_storage")]
    pub sharedirStorage: Quantity,

    #[serde(default = "defaults::default_pkglibdir_storage")]
    pub pkglibdirStorage: Quantity,

    #[serde(default = "defaults::default_postgres_exporter_enabled")]
    pub postgresExporterEnabled: bool,

    #[serde(default = "defaults::default_image")]
    pub image: String,

    #[serde(default = "defaults::default_postgres_exporter_image")]
    pub postgresExporterImage: String,

    #[serde(default = "defaults::default_port")]
    pub port: i32,

    #[serde(default = "defaults::default_uid")]
    pub uid: i32,

    #[serde(default = "defaults::default_extensions")]
    pub extensions: Vec<Extension>,

    #[serde(default = "defaults::default_trunk_installs")]
    pub trunk_installs: Vec<TrunkInstall>,

    #[serde(default = "defaults::default_stop")]
    pub stop: bool,

    #[serde(default = "defaults::default_service_account_template")]
    pub serviceAccountTemplate: ServiceAccountTemplate,

    #[serde(default = "defaults::default_backup")]
    pub backup: Backup,

    pub metrics: Option<PostgresMetrics>,

    pub extra_domains_rw: Option<Vec<String>>,

    pub stack: Option<Stack>,
    // dynamic runtime configs
    pub runtime_config: Option<Vec<PgConfig>>,
    // configuration overrides, typically defined by the user
    pub override_configs: Option<Vec<PgConfig>>,
    // Connection pooler configuration
    pub connPooler: Option<ConnPooler>,
}

impl CoreDBSpec {
    // extracts all postgres configurations
    // configs can be defined in several different places (from a stack, user override, from an extension installation, user overrides, etc)
    pub fn get_pg_configs(&self, requires_load: Vec<String>) -> Result<Option<Vec<PgConfig>>, MergeError> {
        let stack_configs = self
            .stack
            .as_ref()
            .and_then(|s| s.postgres_config.clone())
            .unwrap_or_default();
        let mut runtime_configs = self.runtime_config.clone().unwrap_or_default();
        // TODO: configs that come with extension installation
        // e.g. let extension_configs = ...
        // these extensions could be set by the operator, or trunk + operator
        // trunk install pg_partman could come with something like `pg_partman_bgw.dbname = xxx`

        // Get list of extension names that require load
        let mut include_with_shared_preload_libraries = BTreeSet::new();
        for ext in self.extensions.iter() {
            'loc: for location in ext.locations.iter() {
                if location.clone().enabled && requires_load.contains(&ext.name) {
                    include_with_shared_preload_libraries.insert(ext.name.clone());
                    break 'loc;
                }
            }
        }

        let shared_preload_from_extensions = ConfigValue::Multiple(include_with_shared_preload_libraries);
        let extension_settings_config = vec![PgConfig {
            name: "shared_preload_libraries".to_string(),
            value: shared_preload_from_extensions,
        }];

        match merge_pg_configs(
            &runtime_configs,
            &extension_settings_config,
            "shared_preload_libraries",
        )? {
            None => {}
            Some(new_shared_preload_libraries) => {
                // check by name attribute if runtime_configs already has shared_preload_libraries
                // if so replace the value. Otherwise add this PgConfig into the vector.
                let mut found = false;
                for cfg in &mut runtime_configs {
                    if cfg.name == "shared_preload_libraries" {
                        cfg.value = new_shared_preload_libraries.value.clone();
                        found = true;
                        break;
                    }
                }
                if !found {
                    runtime_configs.push(new_shared_preload_libraries);
                }
            }
        }

        // handle merge of any of the settings that are multi-value.
        // e.g. stack defines shared_preload_libraries = pg_cron, then operator installs pg_stat_statements at runtime
        // we need to merge the two configs into one,  shared_preload_libraries = pg_cron, pg_stat_statements
        let mut merged_multival_configs: Vec<PgConfig> = Vec::new();
        for cfg_name in MULTI_VAL_CONFIGS {
            let merged_config = merge_pg_configs(&stack_configs, &runtime_configs, cfg_name)?;
            if let Some(merged_config) = merged_config {
                merged_multival_configs.push(merged_config);
            }
        }

        // Order matters - to ensure anything down stream does not have to worry about ordering,
        // set these into a BTreeSet now
        // 1. stack configs
        // 2. runtime configs
        // 3. merged multivals
        // 4. overrides
        let mut pg_configs: BTreeMap<String, PgConfig> = BTreeMap::new();

        for p in stack_configs {
            pg_configs.insert(p.name.clone(), p);
        }
        for p in runtime_configs {
            pg_configs.insert(p.name.clone(), p);
        }
        for p in merged_multival_configs {
            pg_configs.insert(p.name.clone(), p);
        }
        if let Some(override_configs) = &self.override_configs {
            for p in override_configs {
                pg_configs.insert(p.name.clone(), p.clone());
            }
        }

        // remove any configs that are not allowed
        for key in DISALLOWED_CONFIGS {
            pg_configs.remove(key);
        }

        if pg_configs.is_empty() {
            Ok(None)
        } else {
            Ok(Some(pg_configs.values().cloned().collect()))
        }
    }

    pub fn get_pg_config_by_name(
        &self,
        config_name: &str,
        requires_load: Vec<String>,
    ) -> Result<Option<PgConfig>, MergeError> {
        let all_configs = self.get_pg_configs(requires_load)?;
        for config in all_configs.unwrap_or_default() {
            if config.name == config_name {
                return Ok(Some(config));
            }
        }
        Ok(None)
    }
}

/// The status object of `CoreDB`
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[allow(non_snake_case)]
pub struct CoreDBStatus {
    pub running: bool,
    #[serde(default = "defaults::default_extensions_updating")]
    pub extensionsUpdating: bool,
    pub extensions: Option<Vec<ExtensionStatus>>,
    pub trunk_installs: Option<Vec<TrunkInstallStatus>>,
    pub storage: Option<Quantity>,
    pub resources: Option<ResourceRequirements>,
    pub runtime_config: Option<Vec<PgConfig>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_core_db_spec() {
        let json_str = r#"
        {
          "image": "quay.io/tembo/tembo-pg-cnpg:15.3.0-5-cede445",
          "stack": {
            "name": "MessageQueue",
            "image": "quay.io/tembo/tembo-pg-cnpg:15.3.0-5-cede445",
            "services": null,
            "extensions": [
              {
                "name": "pgmq",
                "locations": [
                  {
                    "schema": null,
                    "enabled": true,
                    "version": "0.10.2",
                    "database": "postgres"
                  }
                ],
                "description": null
              },
              {
                "name": "pg_partman",
                "locations": [
                  {
                    "schema": null,
                    "enabled": true,
                    "version": "4.7.3",
                    "database": "postgres"
                  }
                ],
                "description": null
              }
            ],
            "description": "A Tembo Postgres Stack optimized for Message Queue workloads.",
            "stack_version": "0.2.0",
            "infrastructure": {
              "cpu": "1",
              "memory": "1Gi",
              "region": "us-east-1",
              "provider": "aws",
              "storage_size": "10Gi",
              "instance_type": "GeneralPurpose",
              "storage_class": "gp3"
            },
            "trunk_installs": [
              {
                "name": "pgmq",
                "version": "0.10.2"
              },
              {
                "name": "pg_partman",
                "version": "4.7.3"
              }
            ],
            "postgres_config": [
              {
                "name": "shared_preload_libraries",
                "value": "pg_stat_statements,pg_partman_bgw"
              },
              {
                "name": "pg_partman_bgw.dbname",
                "value": "postgres"
              },
              {
                "name": "pg_partman_bgw.interval",
                "value": "60"
              },
              {
                "name": "pg_partman_bgw.role",
                "value": "postgres"
              },
              {
                "name": "random_page_cost",
                "value": "1.1"
              },
              {
                "name": "autovacuum_vacuum_cost_limit",
                "value": "-1"
              },
              {
                "name": "autovacuum_vacuum_scale_factor",
                "value": "0.05"
              },
              {
                "name": "autovacuum_vacuum_insert_scale_factor",
                "value": "0.05"
              },
              {
                "name": "autovacuum_analyze_scale_factor",
                "value": "0.05"
              },
              {
                "name": "checkpoint_timeout",
                "value": "10min"
              },
              {
                "name": "pg_stat_statements.track",
                "value": "all"
              }
            ],
            "postgres_metrics": {
              "pgmq": {
                "query": "select queue_name, queue_length, oldest_msg_age_sec, newest_msg_age_sec, total_messages from public.pgmq_metrics_all()",
                "master": true,
                "metrics": [
                  {
                    "queue_name": {
                      "usage": "LABEL",
                      "description": "Name of the queue"
                    }
                  },
                  {
                    "queue_length": {
                      "usage": "GAUGE",
                      "description": "Number of messages in the queue"
                    }
                  },
                  {
                    "oldest_msg_age_sec": {
                      "usage": "GAUGE",
                      "description": "Age of the oldest message in the queue, in seconds."
                    }
                  },
                  {
                    "newest_msg_age_sec": {
                      "usage": "GAUGE",
                      "description": "Age of the newest message in the queue, in seconds."
                    }
                  },
                  {
                    "total_messages": {
                      "usage": "GAUGE",
                      "description": "Total number of messages that have passed into the queue."
                    }
                  }
                ]
              }
            },
            "postgres_config_engine": "standard"
          },
          "metrics": {
            "image": "quay.io/prometheuscommunity/postgres-exporter:v0.12.0",
            "enabled": true,
            "queries": {
              "pgmq": {
                "query": "select queue_name, queue_length, oldest_msg_age_sec, newest_msg_age_sec, total_messages from public.pgmq_metrics_all()",
                "master": true,
                "metrics": [
                  {
                    "queue_name": {
                      "usage": "LABEL",
                      "description": "Name of the queue"
                    }
                  },
                  {
                    "queue_length": {
                      "usage": "GAUGE",
                      "description": "Number of messages in the queue"
                    }
                  },
                  {
                    "oldest_msg_age_sec": {
                      "usage": "GAUGE",
                      "description": "Age of the oldest message in the queue, in seconds."
                    }
                  },
                  {
                    "newest_msg_age_sec": {
                      "usage": "GAUGE",
                      "description": "Age of the newest message in the queue, in seconds."
                    }
                  },
                  {
                    "total_messages": {
                      "usage": "GAUGE",
                      "description": "Total number of messages that have passed into the queue."
                    }
                  }
                ]
              }
            }
          },
          "storage": "10Gi",
          "resources": {
            "limits": {
              "cpu": "1",
              "memory": "1Gi"
            }
          },
          "extensions": [
            {
              "name": "pgmq",
              "locations": [
                {
                  "schema": null,
                  "enabled": true,
                  "version": "0.10.2",
                  "database": "postgres"
                }
              ],
              "description": null
            },
            {
              "name": "pg_partman",
              "locations": [
                {
                  "schema": null,
                  "enabled": true,
                  "version": "4.7.3",
                  "database": "postgres"
                }
              ],
              "description": null
            }
          ],
          "runtime_config": [
            {
              "name": "shared_buffers",
              "value": "256MB"
            },
            {
              "name": "max_connections",
              "value": "107"
            },
            {
              "name": "work_mem",
              "value": "5MB"
            },
            {
              "name": "bgwriter_delay",
              "value": "200ms"
            },
            {
              "name": "effective_cache_size",
              "value": "716MB"
            },
            {
              "name": "maintenance_work_mem",
              "value": "64MB"
            },
            {
              "name": "max_wal_size",
              "value": "2GB"
            }
          ],
          "trunk_installs": [
            {
              "name": "pgmq",
              "version": "0.10.2"
            },
            {
              "name": "pg_partman",
              "version": "4.7.3"
            }
          ],
          "postgresExporterEnabled": true
        }
        "#;

        let _deserialized_spec: CoreDBSpec = serde_json::from_str(json_str).unwrap();
    }
}
