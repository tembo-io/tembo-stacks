use crate::{
    apis::{coredb_types::CoreDB, postgres_parameters::MergeError},
    cloudnativepg::clusters::{
        Cluster, ClusterAffinity, ClusterBackup, ClusterBackupBarmanObjectStore,
        ClusterBackupBarmanObjectStoreData, ClusterBackupBarmanObjectStoreDataCompression,
        ClusterBackupBarmanObjectStoreDataEncryption, ClusterBackupBarmanObjectStoreS3Credentials,
        ClusterBackupBarmanObjectStoreWal, ClusterBackupBarmanObjectStoreWalCompression,
        ClusterBackupBarmanObjectStoreWalEncryption, ClusterBootstrap, ClusterBootstrapPgBasebackup,
        ClusterExternalClusters, ClusterExternalClustersPassword, ClusterLogLevel, ClusterMonitoring,
        ClusterMonitoringCustomQueriesConfigMap, ClusterPostgresql,
        ClusterPostgresqlSyncReplicaElectionConstraint, ClusterPrimaryUpdateMethod,
        ClusterPrimaryUpdateStrategy, ClusterResources, ClusterServiceAccountTemplate,
        ClusterServiceAccountTemplateMetadata, ClusterSpec, ClusterStorage, ClusterSuperuserSecret,
    },
    Context, Error,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::{
    api::{Patch, PatchParams},
    Api, Resource, ResourceExt,
};
use std::{collections::BTreeMap, sync::Arc};
use tracing::{debug, error, warn};

pub fn cnpg_backup_configuration(
    cdb: &CoreDB,
) -> (Option<ClusterBackup>, Option<ClusterServiceAccountTemplate>) {
    let backup_path = cdb.spec.backup.destinationPath.clone();
    if backup_path.is_some() {
        warn!("Backups are disabled because we don't have an S3 backup path");
        return (None, None);
    }
    let service_account_metadata = cdb.spec.serviceAccountTemplate.metadata.clone();
    if service_account_metadata.is_some() {
        warn!("Backups are disabled because we don't have a service account template");
        return (None, None);
    }
    let service_account_annotations = service_account_metadata
        .expect("Expected service account template metadata")
        .annotations;
    if service_account_annotations.is_some() {
        warn!("Backups are disabled because we don't have a service account template with annotations");
        return (None, None);
    }
    let service_account_annotations =
        service_account_annotations.expect("Expected service account template annotations");
    let service_account_role_arn = service_account_annotations.get("eks.amazonaws.com/role-arn");
    if service_account_role_arn.is_none() {
        warn!("Backups are disabled because we don't have a service account template with an EKS role ARN");
        return (None, None);
    }
    let role_arn = service_account_role_arn
        .expect("Expected service account template annotations to contain an EKS role ARN")
        .clone();

    let cluster_backup = Some(ClusterBackup {
        barman_object_store: Some(ClusterBackupBarmanObjectStore {
            data: Some(ClusterBackupBarmanObjectStoreData {
                compression: Some(ClusterBackupBarmanObjectStoreDataCompression::Bzip2),
                encryption: Some(ClusterBackupBarmanObjectStoreDataEncryption::Aes256),
                immediate_checkpoint: Some(true),
                ..ClusterBackupBarmanObjectStoreData::default()
            }),
            destination_path: backup_path.expect("Expected to find S3 path"),
            s3_credentials: Some(ClusterBackupBarmanObjectStoreS3Credentials {
                inherit_from_iam_role: Some(true),
                ..ClusterBackupBarmanObjectStoreS3Credentials::default()
            }),
            wal: Some(ClusterBackupBarmanObjectStoreWal {
                compression: Some(ClusterBackupBarmanObjectStoreWalCompression::Bzip2),
                encryption: Some(ClusterBackupBarmanObjectStoreWalEncryption::Aes256),
                max_parallel: Some(5),
            }),
            ..ClusterBackupBarmanObjectStore::default()
        }),
        ..ClusterBackup::default()
    });

    let service_account_template = Some(ClusterServiceAccountTemplate {
        metadata: ClusterServiceAccountTemplateMetadata {
            annotations: Some(BTreeMap::from([(
                "eks.amazonaws.com/role-arn".to_string(),
                role_arn,
            )])),
            ..ClusterServiceAccountTemplateMetadata::default()
        },
    });

    (cluster_backup, service_account_template)
}

pub fn cnpg_cluster_bootstrap_from_cdb(
    cdb: &CoreDB,
) -> (
    Option<ClusterBootstrap>,
    Option<Vec<ClusterExternalClusters>>,
    Option<ClusterSuperuserSecret>,
) {
    let cluster_bootstrap = ClusterBootstrap {
        pg_basebackup: Some(ClusterBootstrapPgBasebackup {
            source: "coredb".to_string(),
            ..ClusterBootstrapPgBasebackup::default()
        }),
        ..ClusterBootstrap::default()
    };
    let cluster_name = cdb.name_any();

    let mut coredb_connection_parameters = BTreeMap::new();
    coredb_connection_parameters.insert("user".to_string(), "postgres".to_string());
    // The CoreDB operator rw service name is the CoreDB cluster name
    coredb_connection_parameters.insert("host".to_string(), cluster_name.clone());

    let superuser_secret_name = format!("{}-connection", cluster_name);

    let coredb_cluster = ClusterExternalClusters {
        name: "coredb".to_string(),
        connection_parameters: Some(coredb_connection_parameters),
        password: Some(ClusterExternalClustersPassword {
            // The CoreDB operator connection secret is named as the cluster
            // name, suffixed by -connection
            name: Some(superuser_secret_name.clone()),
            key: "password".to_string(),
            ..ClusterExternalClustersPassword::default()
        }),
        ..ClusterExternalClusters::default()
    };

    let superuser_secret = ClusterSuperuserSecret {
        name: superuser_secret_name,
    };

    (
        Some(cluster_bootstrap),
        Some(vec![coredb_cluster]),
        Some(superuser_secret),
    )
}

// Get PGConfig from CoreDB and convert it to a postgres_parameters and shared_preload_libraries
fn cnpg_postgres_config(
    cdb: &CoreDB,
) -> Result<(Option<BTreeMap<String, String>>, Option<Vec<String>>), MergeError> {
    match cdb.spec.get_pg_configs() {
        Ok(Some(pg_configs)) => {
            let mut postgres_parameters: BTreeMap<String, String> = BTreeMap::new();
            let mut shared_preload_libraries: Vec<String> = Vec::new();

            for pg_config in pg_configs {
                match &pg_config.name[..] {
                    "shared_preload_libraries" => {
                        shared_preload_libraries.push(pg_config.value.to_string());
                    }
                    _ => {
                        postgres_parameters.insert(pg_config.name.clone(), pg_config.value.to_string());
                    }
                }
            }

            let params = if postgres_parameters.is_empty() {
                None
            } else {
                Some(postgres_parameters)
            };

            let libs = if shared_preload_libraries.is_empty() {
                None
            } else {
                Some(shared_preload_libraries)
            };

            Ok((params, libs))
        }
        Ok(None) => {
            // Return None, None when no pg_config is set
            Ok((None, None))
        }
        Err(e) => Err(e),
    }
}

fn cnpg_cluster_storage(cdb: &CoreDB) -> Option<ClusterStorage> {
    let storage = cdb.spec.storage.clone().0;
    Some(ClusterStorage {
        resize_in_use_volumes: Some(true),
        size: Some(storage),
        // TODO: pass storage class from cdb
        // storage_class: Some("gp3-enc".to_string()),
        storage_class: None,
        ..ClusterStorage::default()
    })
}

pub fn cnpg_cluster_from_cdb(cdb: &CoreDB) -> Cluster {
    let name = cdb.name_any();
    let namespace = cdb.namespace().unwrap();
    let owner_reference = cdb.controller_owner_ref(&()).unwrap();

    let mut annotations = BTreeMap::new();
    annotations.insert("tembo-pod-init.tembo.io/inject".to_string(), "true".to_string());

    let (bootstrap, external_clusters, superuser_secret) = cnpg_cluster_bootstrap_from_cdb(cdb);

    let (backup, service_account_template) = cnpg_backup_configuration(cdb);

    let storage = cnpg_cluster_storage(cdb);

    let (postgres_parameters, shared_preload_libraries) = match cnpg_postgres_config(cdb) {
        Ok((postgres_parameters, shared_preload_libraries)) => {
            (postgres_parameters, shared_preload_libraries)
        }
        Err(e) => {
            error!("Error generating postgres parameters: {}", e);
            (None, None)
        }
    };

    Cluster {
        metadata: ObjectMeta {
            name: Some(name),
            namespace: Some(namespace),
            annotations: Some(annotations),
            owner_references: Some(vec![owner_reference]),
            ..ObjectMeta::default()
        },
        spec: ClusterSpec {
            affinity: Some(ClusterAffinity {
                pod_anti_affinity_type: Some("preferred".to_string()),
                topology_key: Some("topology.kubernetes.io/zone".to_string()),
                ..ClusterAffinity::default()
            }),
            backup,
            service_account_template,
            bootstrap,
            superuser_secret,
            external_clusters,
            enable_superuser_access: Some(true),
            failover_delay: Some(0),
            image_name: Some("quay.io/tembo/tembo-pg-cnpg:15.3.0-1-3953e4e".to_string()),
            instances: 1,
            log_level: Some(ClusterLogLevel::Info),
            max_sync_replicas: Some(0),
            min_sync_replicas: Some(0),
            monitoring: Some(ClusterMonitoring {
                custom_queries_config_map: Some(vec![ClusterMonitoringCustomQueriesConfigMap {
                    key: "queries".to_string(),
                    name: "cnpg-default-monitoring".to_string(),
                }]),
                disable_default_queries: Some(false),
                enable_pod_monitor: Some(true),
                ..ClusterMonitoring::default()
            }),
            postgres_gid: Some(26),
            postgres_uid: Some(26),
            postgresql: Some(ClusterPostgresql {
                ldap: None,
                parameters: postgres_parameters,
                sync_replica_election_constraint: Some(ClusterPostgresqlSyncReplicaElectionConstraint {
                    enabled: false,
                    ..ClusterPostgresqlSyncReplicaElectionConstraint::default()
                }),
                shared_preload_libraries,
                pg_hba: None,
                ..ClusterPostgresql::default()
            }),
            primary_update_method: Some(ClusterPrimaryUpdateMethod::Restart),
            primary_update_strategy: Some(ClusterPrimaryUpdateStrategy::Unsupervised),
            resources: Some(ClusterResources {
                claims: None,
                limits: cdb.spec.resources.clone().limits,
                requests: cdb.spec.resources.clone().requests,
            }),
            // The time in seconds that is allowed for a PostgreSQL instance to successfully start up
            start_delay: Some(30),
            // The time in seconds that is allowed for a PostgreSQL instance to gracefully shutdown
            stop_delay: Some(30),
            storage,
            // The time in seconds that is allowed for a primary PostgreSQL instance
            // to gracefully shutdown during a switchover
            switchover_delay: Some(60),
            // Set this to match when the cluster consolidation happens
            node_maintenance_window: None,
            ..ClusterSpec::default()
        },
        status: None,
    }
}

pub async fn reconcile_cnpg(cdb: &CoreDB, ctx: Arc<Context>) -> Result<(), Error> {
    debug!("Generating CNPG spec");
    let cluster = cnpg_cluster_from_cdb(cdb);
    debug!("Getting namespace of cluster");
    let namespace = cluster
        .metadata
        .namespace
        .clone()
        .expect("CNPG Cluster should always have a namespace");
    debug!("Getting name of cluster");
    let name = cluster
        .metadata
        .name
        .clone()
        .expect("CNPG Cluster should always have a name");
    debug!("Patching cluster");
    let cluster_api: Api<Cluster> = Api::namespaced(ctx.client.clone(), namespace.as_str());
    let ps = PatchParams::apply("cntrlr");
    let _o = cluster_api
        .patch(&name, &ps, &Patch::Apply(&cluster))
        .await
        .map_err(|err| {
            debug!("Error patching cluster: {}", err);
            Error::KubeError(err)
        })?;
    debug!("Applied");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_cluster() {
        let json_str = r#"
        {
          "apiVersion": "postgresql.cnpg.io/v1",
          "kind": "Cluster",
          "metadata": {
            "annotations": {
              "tembo-pod-init.tembo.io/inject": "true"
            },
            "creationTimestamp": "2023-07-03T18:15:32Z",
            "generation": 1,
            "managedFields": [
              {
                "apiVersion": "postgresql.cnpg.io/v1",
                "fieldsType": "FieldsV1",
                "fieldsV1": {
                  "f:metadata": {
                    "f:annotations": {
                      "f:tembo-pod-init.tembo.io/inject": {}
                    },
                    "f:ownerReferences": {
                      "k:{\"uid\":\"d8efe1ff-b09a-43ca-a568-def96235e754\"}": {}
                    }
                  },
                  "f:spec": {
                    "f:affinity": {
                      "f:podAntiAffinityType": {},
                      "f:topologyKey": {}
                    },
                    "f:bootstrap": {
                      "f:pg_basebackup": {
                        "f:source": {}
                      }
                    },
                    "f:enableSuperuserAccess": {},
                    "f:externalClusters": {},
                    "f:failoverDelay": {},
                    "f:imageName": {},
                    "f:instances": {},
                    "f:logLevel": {},
                    "f:maxSyncReplicas": {},
                    "f:minSyncReplicas": {},
                    "f:monitoring": {
                      "f:customQueriesConfigMap": {},
                      "f:disableDefaultQueries": {},
                      "f:enablePodMonitor": {}
                    },
                    "f:postgresGID": {},
                    "f:postgresUID": {},
                    "f:postgresql": {
                      "f:parameters": {
                        "f:archive_mode": {},
                        "f:archive_timeout": {},
                        "f:dynamic_shared_memory_type": {},
                        "f:log_destination": {},
                        "f:log_directory": {},
                        "f:log_filename": {},
                        "f:log_rotation_age": {},
                        "f:log_rotation_size": {},
                        "f:log_truncate_on_rotation": {},
                        "f:logging_collector": {},
                        "f:max_parallel_workers": {},
                        "f:max_replication_slots": {},
                        "f:max_worker_processes": {},
                        "f:shared_memory_type": {},
                        "f:wal_keep_size": {},
                        "f:wal_receiver_timeout": {},
                        "f:wal_sender_timeout": {}
                      },
                      "f:syncReplicaElectionConstraint": {
                        "f:enabled": {}
                      }
                    },
                    "f:primaryUpdateMethod": {},
                    "f:primaryUpdateStrategy": {},
                    "f:startDelay": {},
                    "f:stopDelay": {},
                    "f:storage": {
                      "f:resizeInUseVolumes": {},
                      "f:size": {}
                    },
                    "f:superuserSecret": {
                      "f:name": {}
                    },
                    "f:switchoverDelay": {}
                  }
                },
                "manager": "cntrlr",
                "operation": "Apply",
                "time": "2023-07-03T18:15:32Z"
              },
              {
                "apiVersion": "postgresql.cnpg.io/v1",
                "fieldsType": "FieldsV1",
                "fieldsV1": {
                  "f:status": {
                    ".": {},
                    "f:certificates": {
                      ".": {},
                      "f:clientCASecret": {},
                      "f:expirations": {
                        ".": {},
                        "f:test-coredb-ca": {},
                        "f:test-coredb-replication": {},
                        "f:test-coredb-server": {}
                      },
                      "f:replicationTLSSecret": {},
                      "f:serverAltDNSNames": {},
                      "f:serverCASecret": {},
                      "f:serverTLSSecret": {}
                    },
                    "f:cloudNativePGCommitHash": {},
                    "f:cloudNativePGOperatorHash": {},
                    "f:conditions": {},
                    "f:configMapResourceVersion": {
                      ".": {},
                      "f:metrics": {
                        ".": {},
                        "f:cnpg-default-monitoring": {}
                      }
                    },
                    "f:healthyPVC": {},
                    "f:instanceNames": {},
                    "f:instances": {},
                    "f:instancesStatus": {
                      ".": {},
                      "f:failed": {}
                    },
                    "f:jobCount": {},
                    "f:latestGeneratedNode": {},
                    "f:managedRolesStatus": {},
                    "f:phase": {},
                    "f:phaseReason": {},
                    "f:poolerIntegrations": {
                      ".": {},
                      "f:pgBouncerIntegration": {}
                    },
                    "f:pvcCount": {},
                    "f:readService": {},
                    "f:secretsResourceVersion": {
                      ".": {},
                      "f:clientCaSecretVersion": {},
                      "f:replicationSecretVersion": {},
                      "f:serverCaSecretVersion": {},
                      "f:serverSecretVersion": {},
                      "f:superuserSecretVersion": {}
                    },
                    "f:targetPrimary": {},
                    "f:targetPrimaryTimestamp": {},
                    "f:topology": {
                      ".": {},
                      "f:instances": {
                        ".": {},
                        "f:test-coredb-1": {}
                      },
                      "f:successfullyExtracted": {}
                    },
                    "f:writeService": {}
                  }
                },
                "manager": "Go-http-client",
                "operation": "Update",
                "subresource": "status",
                "time": "2023-07-03T18:16:49Z"
              }
            ],
            "name": "test-coredb",
            "namespace": "default",
            "ownerReferences": [
              {
                "apiVersion": "coredb.io/v1alpha1",
                "controller": true,
                "kind": "CoreDB",
                "name": "test-coredb",
                "uid": "d8efe1ff-b09a-43ca-a568-def96235e754"
              }
            ],
            "resourceVersion": "7675",
            "uid": "7bfae8f4-bc86-481b-8f7c-7a7a659da265"
          },
          "spec": {
            "affinity": {
              "podAntiAffinityType": "preferred",
              "topologyKey": "topology.kubernetes.io/zone"
            },
            "bootstrap": {
              "pg_basebackup": {
                "database": "",
                "owner": "",
                "source": "coredb"
              }
            },
            "enableSuperuserAccess": true,
            "externalClusters": [
              {
                "connectionParameters": {
                  "host": "test-coredb",
                  "user": "postgres"
                },
                "name": "coredb",
                "password": {
                  "key": "password",
                  "name": "test-coredb-connection"
                }
              }
            ],
            "failoverDelay": 0,
            "imageName": "quay.io/tembo/tembo-pg-cnpg:15.3.0-1-3953e4e",
            "instances": 1,
            "logLevel": "info",
            "maxSyncReplicas": 0,
            "minSyncReplicas": 0,
            "monitoring": {
              "customQueriesConfigMap": [
                {
                  "key": "queries",
                  "name": "cnpg-default-monitoring"
                }
              ],
              "disableDefaultQueries": false,
              "enablePodMonitor": true
            },
            "postgresGID": 26,
            "postgresUID": 26,
            "postgresql": {
              "parameters": {
                "archive_mode": "on",
                "archive_timeout": "5min",
                "dynamic_shared_memory_type": "posix",
                "log_destination": "csvlog",
                "log_directory": "/controller/log",
                "log_filename": "postgres",
                "log_rotation_age": "0",
                "log_rotation_size": "0",
                "log_truncate_on_rotation": "false",
                "logging_collector": "on",
                "max_parallel_workers": "32",
                "max_replication_slots": "32",
                "max_worker_processes": "32",
                "shared_memory_type": "mmap",
                "shared_preload_libraries": "",
                "wal_keep_size": "512MB",
                "wal_receiver_timeout": "5s",
                "wal_sender_timeout": "5s"
              },
              "syncReplicaElectionConstraint": {
                "enabled": false
              }
            },
            "primaryUpdateMethod": "restart",
            "primaryUpdateStrategy": "unsupervised",
            "resources": {},
            "startDelay": 30,
            "stopDelay": 30,
            "storage": {
              "resizeInUseVolumes": true,
              "size": "1Gi"
            },
            "superuserSecret": {
              "name": "test-coredb-connection"
            },
            "switchoverDelay": 60
          },
          "status": {
            "certificates": {
              "clientCASecret": "test-coredb-ca",
              "expirations": {
                "test-coredb-ca": "2023-10-01 18:10:32 +0000 UTC",
                "test-coredb-replication": "2023-10-01 18:10:32 +0000 UTC",
                "test-coredb-server": "2023-10-01 18:10:32 +0000 UTC"
              },
              "replicationTLSSecret": "test-coredb-replication",
              "serverAltDNSNames": [
                "test-coredb-rw",
                "test-coredb-rw.default",
                "test-coredb-rw.default.svc",
                "test-coredb-r",
                "test-coredb-r.default",
                "test-coredb-r.default.svc",
                "test-coredb-ro",
                "test-coredb-ro.default",
                "test-coredb-ro.default.svc"
              ],
              "serverCASecret": "test-coredb-ca",
              "serverTLSSecret": "test-coredb-server"
            },
            "cloudNativePGCommitHash": "9bf74c9e",
            "cloudNativePGOperatorHash": "5d5f339b30506db0996606d61237dcf639c1e0d3009c0399e87e99cc7bc2caf0",
            "conditions": [
              {
                "lastTransitionTime": "2023-07-03T18:15:32Z",
                "message": "Cluster Is Not Ready",
                "reason": "ClusterIsNotReady",
                "status": "False",
                "type": "Ready"
              }
            ],
            "configMapResourceVersion": {
              "metrics": {
                "cnpg-default-monitoring": "7435"
              }
            },
            "healthyPVC": [
              "test-coredb-1"
            ],
            "instanceNames": [
              "test-coredb-1"
            ],
            "instances": 1,
            "instancesStatus": {
              "failed": [
                "test-coredb-1"
              ]
            },
            "jobCount": 1,
            "latestGeneratedNode": 1,
            "managedRolesStatus": {},
            "phase": "Setting up primary",
            "phaseReason": "Creating primary instance test-coredb-1",
            "poolerIntegrations": {
              "pgBouncerIntegration": {}
            },
            "pvcCount": 1,
            "readService": "test-coredb-r",
            "secretsResourceVersion": {
              "clientCaSecretVersion": "7409",
              "replicationSecretVersion": "7411",
              "serverCaSecretVersion": "7409",
              "serverSecretVersion": "7410",
              "superuserSecretVersion": "7107"
            },
            "targetPrimary": "test-coredb-1",
            "targetPrimaryTimestamp": "2023-07-03T18:15:32.464538Z",
            "topology": {
              "instances": {
                "test-coredb-1": {}
              },
              "successfullyExtracted": true
            },
            "writeService": "test-coredb-rw"
          }
        }

        "#;

        let _result: Cluster = serde_json::from_str(json_str).expect("Should be able to deserialize");
    }
}
