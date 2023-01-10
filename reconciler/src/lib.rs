mod pg_cluster_crd;

use kube::api::{DeleteParams, ListParams, Patch, PatchParams};
use kube::{Api, Client};
use pg_cluster_crd::PostgresCluster;
use serde_json::Value;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Kube Error: {0}")]
    KubeError(#[source] kube::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub async fn generate_spec(body: Value) -> Value {
    let name: String = serde_json::from_value(body["resource_name"].clone()).unwrap();
    let image: String = serde_json::from_value(body["postgres_image"].clone()).unwrap();
    let cpu: String = serde_json::from_value(body["cpu"].clone()).unwrap();
    let memory: String = serde_json::from_value(body["memory"].clone()).unwrap();

    let spec = serde_json::json!({
        "apiVersion": "postgres-operator.crunchydata.com/v1beta1",
        "kind": "PostgresCluster",
        "metadata": {
            "name": format!("{}", name),
        },
        "spec": {
            "image": format!("{}", image),
            "postgresVersion": 14,
            "instances": [
                {
                    "name": "instance1",
                    "dataVolumeClaimSpec": {
                        "accessModes": ["ReadWriteOnce"],
                        "resources": {"requests": {"storage": "1Gi"}},
                    },
                    "resources": {
                        "limits": {
                            "cpu": format!("{}", cpu),
                            "memory": format!("{}", memory),
                        },
                        "requests": {
                            "cpu": format!("{}", cpu),
                            "memory": format!("{}", memory),
                        },
                    },
                },
            ],
            "backups": {
                "pgbackrest": {
                    "image": "registry.developers.crunchydata.com/crunchydata/crunchy-pgbackrest:ubi8-2.41-2",
                    "repos": [
                        {
                            "name": "repo1",
                            "volume": {
                                "volumeClaimSpec": {
                                    "accessModes": ["ReadWriteOnce"],
                                    "resources": {"requests": {"storage": "1Gi"}},
                                },
                            },
                        },
                    ],
                }
            },
        },
    });
    spec
}

pub async fn get_all(client: Client, namespace: String) -> Vec<PostgresCluster> {
    let pg_cluster_api: Api<PostgresCluster> = Api::namespaced(client, &namespace);
    let pg_list = pg_cluster_api
        .list(&ListParams::default())
        .await
        .expect("could not get PostgresClusters");
    pg_list.items
}

pub async fn create_or_update(
    client: Client,
    namespace: String,
    deployment: serde_json::Value,
) -> Result<(), Error> {
    let pg_cluster_api: Api<PostgresCluster> = Api::namespaced(client, &namespace);
    let params = PatchParams::apply("reconciler").force();
    let name: String = serde_json::from_value(deployment["metadata"]["name"].clone()).unwrap();
    println!("\nCreating or updating PostgresCluster: {}", name);
    let _o = pg_cluster_api
        .patch(&name, &params, &Patch::Apply(&deployment))
        .await
        .map_err(Error::KubeError)?;
    Ok(())
}

pub async fn delete(client: Client, namespace: String, name: String) -> Result<(), Error> {
    let pg_cluster_api: Api<PostgresCluster> = Api::namespaced(client, &namespace);
    let params = DeleteParams::default();
    println!("\nDeleting PostgresCluster: {}", name);
    let _o = pg_cluster_api
        .delete(&name, &params)
        .await
        .map_err(Error::KubeError);
    Ok(())
}
