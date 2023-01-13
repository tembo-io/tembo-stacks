mod pg_cluster_crd;

use base64::{
    alphabet,
    engine::{self, general_purpose},
    Engine as _,
};
use k8s_openapi::api::core::v1::{Namespace, Secret};
use kube::api::{DeleteParams, ListParams, Patch, PatchParams};
use kube::{Api, Client};
use log::info;
use pg_cluster_crd::PostgresCluster;
use serde_json::Value;
use std::fmt::Debug;
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
    let storage: String = serde_json::from_value(body["storage"].clone()).unwrap();

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
                        "resources": {"requests": {"storage": format!("{}", storage)}},
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
    info!("\nCreating or updating PostgresCluster: {}", name);
    let _o = pg_cluster_api
        .patch(&name, &params, &Patch::Apply(&deployment))
        .await
        .map_err(Error::KubeError)?;
    Ok(())
}

pub async fn delete(client: Client, namespace: String, name: String) -> Result<(), Error> {
    let pg_cluster_api: Api<PostgresCluster> = Api::namespaced(client, &namespace);
    let params = DeleteParams::default();
    info!("\nDeleting PostgresCluster: {}", name);
    let _o = pg_cluster_api
        .delete(&name, &params)
        .await
        .map_err(Error::KubeError);
    Ok(())
}

pub async fn create_namespace(client: Client, name: String) -> Result<(), Error> {
    let ns_api: Api<Namespace> = Api::all(client);
    let params = PatchParams::apply("reconciler").force();
    let ns = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Namespace",
        "metadata": {
            "name": format!("{}", name),
        }
    });
    info!("\nCreating namespace {} if it does not exist", name);
    let _o = ns_api
        .patch(&name, &params, &Patch::Apply(&ns))
        .await
        .map_err(Error::KubeError)?;
    Ok(())
}

pub async fn delete_namespace(client: Client, name: String) -> Result<(), Error> {
    let ns_api: Api<Namespace> = Api::all(client);
    let params = DeleteParams::default();
    info!("\nDeleting namespace: {}", name);
    let _o = ns_api
        .delete(&name, &params)
        .await
        .map_err(Error::KubeError);
    Ok(())
}

pub async fn get_pg_conn(client: Client, name: String) -> Result<String, Error> {
    // read secret <name>-pguser-name
    // let secret_name = format!("{}-pguser-{}", name, name);
    // let secret_api: Api<Secret> = Api::namespaced(client, &name.clone());
    // let secret = secret_api
    //     .get(secret_name.as_str())
    //     .await
    //     .expect("error getting Secret");
    // let data = secret.data.unwrap();
    // let user = data.get("user");
    // let new_user = user.unwrap().to_owned();
    // // let another_new_user: String = serde_json::from_str(&new_user).unwrap();
    // println!("\nnew_user {}", new_user.to_string());
    //
    // let decoded_user = general_purpose::STANDARD.decode(new_user);
    // let password = data.get("password").unwrap();
    //
    // println!("\nUser {:?}", decoded_user.unwrap());
    // // get values from secret:
    // // user
    // // password
    // // host will be <name>.coredb-development.com
    // // wait for secret
    // // decode values
    // // string will look like postgresql://<user>:<password>@<host>:5432

    Ok("conn-string".to_owned())
}
