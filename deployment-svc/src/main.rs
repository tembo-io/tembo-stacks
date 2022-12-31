use deployment_svc::{CoreDBDeploymentService, PostgresCluster};
use kube::{Api, Client};
use log::info;
use std::fmt::Debug;
use std::{thread, time};

#[tokio::main]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Poll queue for new events  (format?)
    // Based on action in message, create, update, delete PostgresCluster
    // When do we need get_all()?

    // Infer the runtime environment and try to create a Kubernetes Client
    let client = Client::try_default().await?;

    loop {
        let vec = CoreDBDeploymentService::get_all(client.clone());

        for pg in vec.await.iter() {
            println!("found PostgresCluster");
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
