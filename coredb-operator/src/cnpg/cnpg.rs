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
        ClusterSpec, ClusterSuperuserSecret,
    },
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use kube::{Resource, ResourceExt};
use std::collections::BTreeMap;
use crate::cnpg::clusters::{ClusterMonitoringCustomQueriesConfigMap, ClusterPostgresql};
// apiVersion: postgresql.cnpg.io/v1
// kind: Cluster
// metadata:
//   annotations:
//     tembo-pod-init.tembo.io/inject: "true"
//   name: org-coredb-inst-test-1205
//   namespace: org-coredb-inst-test-1205
// spec:
//   affinity:
//     podAntiAffinityType: preferred
//     topologyKey: ""
//   backup:
//     barmanObjectStore:
//       data:
//         compression: bzip2
//         encryption: AES256
//         immediateCheckpoint: true
//       destinationPath: s3://cdb-plat-use1-dev-instance-backups/coredb/coredb/org-coredb-inst-test-1205/cnpg-backups-v1
//       s3Credentials:
//         inheritFromIAMRole: true
//       wal:
//         compression: bzip2
//         encryption: AES256
//         maxParallel: 5
//     target: prefer-standby
//   externalClusters:
//   - name: coredb
//     connectionParameters:
//       host: org-coredb-inst-test-1205
//       user: postgres
//     password:
//       name: org-coredb-inst-test-1205-connection
//       key: password
//   bootstrap:
//     pg_basebackup:
//       source: coredb
//   superuserSecret:
//     name: org-coredb-inst-test-1205-connection
//   enableSuperuserAccess: true
//   failoverDelay: 0
//   imageName: quay.io/tembo/tembo-pg-cnpg:15.3.0-1-3953e4e
//   instances: 1
//   logLevel: info
//   maxSyncReplicas: 0
//   minSyncReplicas: 0
//   monitoring:
//     customQueriesConfigMap:
//     - key: queries
//       name: cnpg-default-monitoring
//     disableDefaultQueries: false
//     enablePodMonitor: true
//   postgresGID: 26
//   postgresUID: 26
//   postgresql:
//     parameters:
//       archive_mode: "on"
//       archive_timeout: 5min
//       dynamic_shared_memory_type: posix
//       log_destination: csvlog
//       log_directory: /controller/log
//       log_filename: postgres
//       log_rotation_age: "0"
//       log_rotation_size: "0"
//       log_truncate_on_rotation: "false"
//       logging_collector: "on"
//       max_parallel_workers: "32"
//       max_replication_slots: "32"
//       max_worker_processes: "32"
//       shared_memory_type: mmap
//       shared_preload_libraries: ""
//       wal_keep_size: 512MB
//       wal_receiver_timeout: 5s
//       wal_sender_timeout: 5s
//     syncReplicaElectionConstraint:
//       enabled: false
//   primaryUpdateMethod: restart
//   primaryUpdateStrategy: unsupervised
//   resources: {}
//   serviceAccountTemplate:
//     metadata:
//       annotations:
//         eks.amazonaws.com/role-arn: arn:aws:iam::484221059514:role/org-coredb-inst-test-1205-iam
//   startDelay: 30
//   stopDelay: 30
//   storage:
//     resizeInUseVolumes: true
//     size: 5Gi
//     storageClass: gp3-enc
//   switchoverDelay: 40000000

pub fn cnpg_cluster_backup_from_cdb(cdb: &CoreDB) -> Option<ClusterBackup> {
    let backup_path = cdb.spec.backup.destinationPath.clone();
    if backup_path.is_empty() {
        return None;
    }
    Some(ClusterBackup {
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
    })
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

    let mut coredb_connection_parameters = BtreeMap::new();
    coredb_connection_parameters.insert("user".to_string(), "postgres".to_string());
    // The CoreDB operator rw service name is the CoreDB cluster name
    coredb_connection_parameters.insert("host".to_string(), cluster_name.clone());

    let superuser_secret_name = format!("{}-connection", cluster_name.clone());

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

    return (
        Some(cluster_bootstrap),
        Some(vec![coredb_cluster]),
        Some(superuser_secret),
    );
}

fn cnpg_cluster_parameters_from_cdb(cdb: &CoreDB) -> Option<BTreeMap<String, String>> {
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
    Some(postgres_parameters)
}

pub fn cnpg_cluster_from_cdb(cdb: &CoreDB) -> Cluster {
    let name = cdb.name_any();
    let namespace = cdb.namespace().unwrap();
    let owner_reference = cdb.controller_owner_ref(&()).unwrap();

    let mut annotations = BTreeMap::new();
    annotations.insert("tembo-pod-init.tembo.io/inject".to_string(), "true".to_string());

    let (bootstrap, external_clusters, superuser_secret) = cnpg_cluster_bootstrap_from_cdb(cdb);

    let postgres_parameters = cnpg_cluster_parameters_from_cdb(cdb);

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
                pod_anti_affinity_type: Some("required".to_string()),
                topology_key: Some("topology.kubernetes.io/zone".to_string()),
                ..ClusterAffinity::default()
            }),
            backup: cnpg_cluster_backup_from_cdb(cdb),
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
                custom_queries_config_map: Some(vec![
                   ClusterMonitoringCustomQueriesConfigMap{
                       key: "queries".to_string(),
                       name: "cnpg-default-monitoring".to_string(),
                   }
                ]),
                disable_default_queries: Some(false),
                enable_pod_monitor: Some(true),
                ..ClusterMonitoring::default()
            }),
            postgres_gid: Some(26),
            postgres_uid: Some(26),
            postgresql: Some(ClusterPostgresql {
                parameters: postgres_parameters,
                ..ClusterPostgresql::default()
            }),
            certificates: None,
            description: None,
            env: None,
            env_from: None,
            image_pull_policy: None,
            image_pull_secrets: None,
            inherited_metadata: None,
            managed: None,
            node_maintenance_window: None,
            primary_update_method: None,
            primary_update_strategy: None,
            projected_volume_template: None,
            replica: None,
            replication_slots: None,
            resources: None,
            scheduler_name: None,
            seccomp_profile: None,
            service_account_template: None,
            start_delay: None,
            stop_delay: None,
            storage: None,
            switchover_delay: None,
            topology_spread_constraints: None,
            wal_storage: None,
        },
        status: None,
    }
}
