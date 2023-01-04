mod postgresclusters;

use deployment_svc::CoreDBDeploymentService;
use kube::{Client, ResourceExt};
use log::info;
use std::env;
use std::{thread, time};
use tokio_postgres::{NoTls};

#[tokio::main]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Read connection info from environment variables
    let user = env::var("PG_USER").expect("PG_USER must be set");
    let password = env::var("PG_PASSWORD").expect("PG_PASSWORD must be set");
    let host = env::var("PG_HOST").expect("PG_HOST must be set");
    let port = env::var("PG_PORT").expect("PG_PORT must be set");
    let dbname = env::var("PG_DBNAME").expect("PG_DBNAME must be set");

    // Connect to postgres queue
    let (client, connection) = tokio_postgres::connect(
        &*format!(
            "host={} port={} user={} password={} dbname={}",
            host, port, user, password, dbname
        ),
        NoTls,
    )
    .await?;

    // The connection object performs the actual communication with the database,
    // so spawn it off to run on its own.
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("connection error: {}", e);
        }
    });

    // Read from queue (check for new message)
    // Based on action in message, create, update, delete PostgresCluster
    // When do we need get_all()?

    // Infer the runtime environment and try to create a Kubernetes Client
    let client = Client::try_default().await?;

    loop {
        let vec = CoreDBDeploymentService::get_all(client.clone());

        for pg in vec.await.iter() {
            println!("found PostgresCluster {}", pg.name_any());
        }
        thread::sleep(time::Duration::from_secs(1));
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
