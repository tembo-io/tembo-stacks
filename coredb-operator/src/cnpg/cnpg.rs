use crate::{
    apis::coredb_types::CoreDB,
    cnpg::clusters::{
        Cluster, ClusterAffinity, ClusterAffinityAdditionalPodAffinity,
        ClusterAffinityAdditionalPodAntiAffinity, ClusterBackup, ClusterBackupBarmanObjectStore,
        ClusterBackupBarmanObjectStoreData, ClusterBackupBarmanObjectStoreDataCompression,
        ClusterBackupBarmanObjectStoreDataEncryption, ClusterBackupBarmanObjectStoreS3Credentials,
        ClusterBackupBarmanObjectStoreWal, ClusterBackupBarmanObjectStoreWalCompression,
        ClusterBackupBarmanObjectStoreWalEncryption, ClusterBootstrap, ClusterBootstrapPgBasebackup,
        ClusterExternalClusters, ClusterExternalClustersPassword, ClusterLogLevel, ClusterMonitoring,
        ClusterMonitoringCustomQueriesConfigMap, ClusterPostgresql,
        ClusterPostgresqlSyncReplicaElectionConstraint, ClusterPrimaryUpdateMethod,
        ClusterPrimaryUpdateStrategy, ClusterServiceAccountTemplate, ClusterServiceAccountTemplateMetadata,
        ClusterSpec, ClusterStorage, ClusterSuperuserSecret,
    },
    Context, Error,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use kube::{
    api::{Patch, PatchParams},
    Api, Resource, ResourceExt,
};
use std::{collections::BTreeMap, sync::Arc};
use tracing::log::{debug, warn};

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

fn cnpg_postgres_config(_cdb: &CoreDB) -> (Option<BTreeMap<String, String>>, Option<Vec<String>>) {
    let mut postgres_parameters = BTreeMap::new();
    postgres_parameters.insert("archive_mode".to_string(), "on".to_string());
    postgres_parameters.insert("archive_timeout".to_string(), "5min".to_string());
    postgres_parameters.insert("dynamic_shared_memory_type".to_string(), "posix".to_string());
    postgres_parameters.insert("log_destination".to_string(), "csvlog".to_string());
    postgres_parameters.insert("log_directory".to_string(), "/controller/log".to_string());
    postgres_parameters.insert("log_filename".to_string(), "postgres".to_string());
    postgres_parameters.insert("log_rotation_age".to_string(), "0".to_string());
    postgres_parameters.insert("log_rotation_size".to_string(), "0".to_string());
    postgres_parameters.insert("log_truncate_on_rotation".to_string(), "false".to_string());
    postgres_parameters.insert("logging_collector".to_string(), "on".to_string());
    postgres_parameters.insert("max_parallel_workers".to_string(), "32".to_string());
    postgres_parameters.insert("max_replication_slots".to_string(), "32".to_string());
    postgres_parameters.insert("max_worker_processes".to_string(), "32".to_string());
    postgres_parameters.insert("shared_memory_type".to_string(), "mmap".to_string());
    postgres_parameters.insert("wal_keep_size".to_string(), "512MB".to_string());
    postgres_parameters.insert("wal_receiver_timeout".to_string(), "5s".to_string());
    postgres_parameters.insert("wal_sender_timeout".to_string(), "5s".to_string());
    // TODO: right here, overlay other postgres configs
    let shared_preload_libraries = None;
    (Some(postgres_parameters), shared_preload_libraries)
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

    let (postgres_parameters, shared_preload_libraries) = cnpg_postgres_config(cdb);

    let (backup, service_account_template) = cnpg_backup_configuration(cdb);

    let storage = cnpg_cluster_storage(cdb);

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
            // TODO: before merge
            resources: None,
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
        .map_err(Error::KubeError)?;
    debug!("Applied");
    Ok(())
}
