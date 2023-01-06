extern crate core;

mod postgresclusters;

use deployment_svc::CoreDBDeploymentService;
use kube::{Client, ResourceExt};
use log::info;
use pgmq::{Message, PGMQueue, PGMQueueConfig};
use std::env;
use std::future::Future;
use std::{thread, time};
use tokio_postgres::NoTls;

#[tokio::main]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // // Read connection info from environment variable
    let pg_conn_url = env::var("PG_CONN_URL").expect("PG_CONN_URL must be set");

    // Connect to postgres queue
    let qconfig = PGMQueueConfig {
        queue_name: "myqueue".to_owned(),
        url: pg_conn_url,
        vt: 30,
        delay: 0,
    };
    let queue: PGMQueue = qconfig.init().await;

    // Infer the runtime environment and try to create a Kubernetes Client
    let client = Client::try_default().await?;

    loop {
        // Read from queue (check for new message)
        let read_msg = match queue.read().await {
            Some(Message) => {
                print!("read_msg: {:?}", Message);
                Message
            }
            None => {
                thread::sleep(time::Duration::from_secs(1));
                continue;
            }
        };

        // Based on action in message, create, update, delete PostgresCluster
        match serde_json::from_str(&read_msg.message["action"].to_string()).unwrap() {
            Some("create") | Some("update") => {
                let spec = read_msg.message["spec"].clone();

                // create PostgresCluster
                CoreDBDeploymentService::create_or_update(
                    client.clone(),
                    "default".to_owned(),
                    spec,
                )
                .await
                .expect("error creating PostgresCluster");
            }
            Some("delete") => {
                let name: String = serde_json::from_value(read_msg.message["spec"]["metadata"]["name"].clone()).unwrap();

                // delete PostgresCluster
                CoreDBDeploymentService::delete(
                    client.clone(),
                    "default".to_owned(),
                    name,
                )
                .await
                .expect("error deleting PostgresCluster");
            }
            None | _ => println!("action was not in expected format"),
        }

        // Get all existing PostgresClusters
        let vec = CoreDBDeploymentService::get_all(client.clone());
        for pg in vec.await.iter() {
            println!("found PostgresCluster {}", pg.name_any());
        }
        thread::sleep(time::Duration::from_secs(1));

        // Delete message from queue
        let deleted = queue.delete(&read_msg.msg_id).await;
        println!("deleted: {:?}", deleted);
    }

    Ok(())
}

fn main() {
    env_logger::init();
    info!("starting");
    run().unwrap();
}
