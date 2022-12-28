use deployment_svc::{CoreDBDeploymentService, PostgresCluster};
use kube::{Api, Client};
use std::{thread, time};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Infer the runtime environment and try to create a Kubernetes Client
    let client = Client::try_default().await?;
    let pg_clusters: Api<PostgresCluster> = Api::default_namespaced(client);
    // let vec = CoreDBDeploymentService::get_all(pg_clusters.clone());

    // for pg in vec.await.iter() {
    //     println!("found PostgresCluster {}", pg.spec.name);
    // }
    let deployment = serde_json::json!({
        "apiVersion": "postgres-operator.crunchydata.com/v1beta1",
        "kind": "PostgresCluster",
        "metadata": {
            "name": "dummy",
        },
        "spec": {
            "image": "registry.developers.crunchydata.com/crunchydata/crunchy-postgres:ubi8-14.6-2",
            "postgresVersion": 14,
            "instances": [
                {
                    "name": "instance1",
                    "dataVolumeClaimSpec": {
                        "accessModes": ["ReadWriteOnce"],
                        "resources": {"requests": {"storage": "1Gi"}},
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

    // create PostgresCluster
    CoreDBDeploymentService::create(pg_clusters.clone(), deployment.clone())
        .await
        .expect("error creating PostgresCluster");

    // sleep for 10s
    thread::sleep(time::Duration::from_secs(10));

    // delete PostgresCluster
    CoreDBDeploymentService::delete(pg_clusters.clone(), deployment.clone())
        .await
        .expect("error deleting PostgresCluster");

    Ok(())
}
