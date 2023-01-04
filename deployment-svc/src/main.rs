mod postgresclusters;

use deployment_svc::CoreDBDeploymentService;
use kube::{Api, Client, Resource, ResourceExt};
use log::info;
use postgresclusters::PostgresCluster;
use std::fmt::Debug;
use std::{thread, time};

#[tokio::main]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to queue
    // Poll queue for new events  (format?)
    // Based on action in message, create, update, delete PostgresCluster
    // When do we need get_all()?

    // Infer the runtime environment and try to create a Kubernetes Client
    let client = Client::try_default().await?;

    loop {
        let vec = CoreDBDeploymentService::get_all(client.clone());

        for pg in vec.await.iter() {
            println!("found PostgresCluster {}", pg.name_any());
        }

        // sleep for 10s
        thread::sleep(time::Duration::from_secs(5));
    }

    // // create PostgresCluster
    // CoreDBDeploymentService::create_or_update(pg_clusters.clone(), deployment.clone())
    //     .await
    //     .expect("error creating PostgresCluster");

    //
    // // delete PostgresCluster
    // CoreDBDeploymentService::delete(pg_clusters.clone(), deployment.clone())
    //     .await
    //     .expect("error deleting PostgresCluster");

    Ok(())
}

fn main() {
    env_logger::init();
    info!("starting");
    run().unwrap();
}
