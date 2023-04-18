use aws_sdk_cloudformation::config::Region;
use conductor::{
    aws::cloudformation::AWSConfigState, aws::cloudformation::CloudFormationParams,
    create_ing_route_tcp, create_namespace, create_or_update, delete, delete_namespace,
    generate_spec, get_all, get_coredb_status, get_pg_conn, restart_statefulset, types,
};
use kube::{Client, ResourceExt};
use log::{debug, error, info, warn};
use pgmq::{Message, PGMQueue};
use std::env;
use std::{thread, time};
use tokio_retry::strategy::FixedInterval;
use tokio_retry::Retry;
use types::{CRUDevent, Event};

#[tokio::main]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Read connection info from environment variable
    let pg_conn_url = env::var("PG_CONN_URL").expect("PG_CONN_URL must be set");
    let control_plane_events_queue =
        env::var("CONTROL_PLANE_EVENTS_QUEUE").expect("CONTROL_PLANE_EVENTS_QUEUE must be set");
    let data_plane_events_queue =
        env::var("DATA_PLANE_EVENTS_QUEUE").expect("DATA_PLANE_EVENTS_QUEUE must be set");
    let data_plane_basedomain =
        env::var("DATA_PLANE_BASEDOMAIN").expect("DATA_PLANE_BASEDOMAIN must be set");
    let backup_archive_bucket =
        env::var("BACKUP_ARCHIVE_BUCKET").expect("BACKUP_ARCHIVE_BUCKET must be set");

    // Connect to pgmq
    let queue: PGMQueue = PGMQueue::new(pg_conn_url).await?;

    // Create queues if they do not exist
    queue.create(&control_plane_events_queue).await?;
    queue.create(&data_plane_events_queue).await?;

    // Infer the runtime environment and try to create a Kubernetes Client
    let client = Client::try_default().await?;

    loop {
        // Read from queue (check for new message)
        // messages that dont fit a CRUDevent will error
        // set visibility timeout to 90 seconds
        let read_msg = queue
            .read::<CRUDevent>(&control_plane_events_queue, Some(&90_i32))
            .await?;
        let read_msg: Message<CRUDevent> = match read_msg {
            Some(message) => {
                info!("read_msg: {:?}", message);
                message
            }
            None => {
                thread::sleep(time::Duration::from_secs(1));
                continue;
            }
        };

        // TODO: recycled messages should get archived, logged, alerted
        // this auto-archive of bad messages should only get implemented after
        // control-plane has a scheduled conductor process implemented
        // if read_msg.read_ct >= 2 {
        //     warn!("recycled message: {:?}", read_msg);
        //     queue.archive(queue_name, &read_msg.msg_id).await?;
        //     continue;
        // }

        let namespace = format!(
            "org-{}-inst-{}",
            read_msg.message.organization_name, read_msg.message.dbname
        );
        // Based on message_type in message, create, update, delete CoreDB
        let event_msg: types::StateToControlPlane = match read_msg.message.event_type {
            // every event is for a single namespace
            Event::Create | Event::Update => {
                // Create Cloudformation Stack only for Create event
                if let Event::Create = read_msg.message.event_type {
                    let region = Region::new("us-east-1");
                    let aws_config_state = AWSConfigState::new(region).await;
                    let stack_name = format!(
                        "org-{}-inst-{}-cf",
                        read_msg.message.organization_name, read_msg.message.dbname
                    );
                    let s3_bucket_path = format!(
                        "org-{}/inst-{}",
                        read_msg.message.organization_name, read_msg.message.dbname
                    );
                    let iam_role_name = format!(
                        "org-{}-inst-{}-iam",
                        read_msg.message.organization_name, read_msg.message.dbname
                    );
                    let params = CloudFormationParams::new(
                        // BucketName
                        String::from(&backup_archive_bucket),
                        // S3BucketPath
                        String::from(&s3_bucket_path),
                        // IAMRoleName
                        String::from(&iam_role_name),
                        // ExpirationInDays
                        Some(90),
                    );
                    aws_config_state
                        .create_cloudformation_stack(&stack_name, &params)
                        .await
                        .expect("error creating CloudFormation stack");
                }
                create_namespace(client.clone(), &namespace)
                    .await
                    .expect("error creating namespace");

                // create IngressRouteTCP
                create_ing_route_tcp(client.clone(), &namespace, &data_plane_basedomain)
                    .await
                    .expect("error creating IngressRouteTCP");

                // create /metrics ingress
                //
                // Disable this feature until IP allow list
                //
                // create_metrics_ingress(client.clone(), &namespace, &data_plane_basedomain)
                //     .await
                //     .expect("error creating ingress for /metrics");

                // generate CoreDB spec based on values in body
                let spec = generate_spec(&namespace, &read_msg.message.spec).await;

                let spec_js = serde_json::to_string(&spec).unwrap();
                debug!("spec: {}", spec_js);
                // create or update CoreDB
                create_or_update(client.clone(), &namespace, spec)
                    .await
                    .expect("error creating or updating CoreDB");
                // get connection string values from secret
                let conn_info = get_pg_conn(client.clone(), &namespace, &data_plane_basedomain)
                    .await
                    .expect("error getting secret");

                // read spec.status from CoreDB
                // this should wait until it is able to receive an actual update from the cluster
                // retrying actions with kube
                // limit to 60 seconds - 20 retries, 5 seconds between retries
                // TODO: need a better way to handle this
                let retry_strategy = FixedInterval::from_millis(5000).take(20);
                let result = Retry::spawn(retry_strategy.clone(), || {
                    get_coredb_status(client.clone(), &namespace)
                })
                .await;
                if result.is_err() {
                    error!("error getting CoreDB status: {:?}", result);
                    continue;
                }
                let mut current_spec = result?;
                let spec_js = serde_json::to_string(&current_spec.spec).unwrap();
                debug!("dbname: {}, current_spec: {:?}", &namespace, spec_js);

                // get actual extensions from crd status
                let actual_extension = match current_spec.status {
                    Some(status) => status.extensions,
                    None => {
                        warn!("No extensions in: {:?}", &namespace);
                        None
                    }
                };
                // UPDATE SPEC OBJECT WITH ACTUAL EXTENSIONS
                current_spec.spec.extensions = actual_extension;

                let report_event = match read_msg.message.event_type {
                    Event::Create => Event::Created,
                    Event::Update => Event::Updated,
                    _ => unreachable!(),
                };
                types::StateToControlPlane {
                    data_plane_id: read_msg.message.data_plane_id,
                    event_id: read_msg.message.event_id,
                    event_type: report_event,
                    spec: Some(current_spec.spec),
                    connection: Some(conn_info),
                }
            }
            Event::Delete => {
                // delete CoreDB
                delete(client.clone(), &namespace, &namespace)
                    .await
                    .expect("error deleting CoreDB");

                // delete namespace
                delete_namespace(client.clone(), &namespace)
                    .await
                    .expect("error deleting namespace");

                // delete Cloudformation Stack
                let region = Region::new("us-east-1");
                let aws_config_state = AWSConfigState::new(region).await;
                let stack_name = format!(
                    "org-{}-inst-{}-cf",
                    read_msg.message.organization_name, read_msg.message.dbname
                );
                aws_config_state
                    .delete_cloudformation_stack(&stack_name)
                    .await
                    .expect("error deleting CloudFormation stack");

                // report state
                types::StateToControlPlane {
                    data_plane_id: read_msg.message.data_plane_id,
                    event_id: read_msg.message.event_id,
                    event_type: Event::Deleted,
                    spec: None,
                    connection: None,
                }
            }
            Event::Restart => {
                // TODO: refactor to be more DRY
                // Restart and Update events share a lot of the same code.
                // move some operations after the Event match
                info!("handling instance restart");
                restart_statefulset(client.clone(), &namespace, &namespace)
                    .await
                    .expect("error restarting statefulset");
                let retry_strategy = FixedInterval::from_millis(5000).take(20);
                let result = Retry::spawn(retry_strategy.clone(), || {
                    get_coredb_status(client.clone(), &namespace)
                })
                .await;
                if result.is_err() {
                    error!("error getting CoreDB status: {:?}", result);
                    continue;
                }
                let mut current_spec = result?;
                let spec_js = serde_json::to_string(&current_spec.spec).unwrap();
                debug!("dbname: {}, current_spec: {:?}", &namespace, spec_js);

                // get actual extensions from crd status
                let actual_extension = match current_spec.status {
                    Some(status) => status.extensions,
                    None => {
                        warn!("No extensions in: {:?}", &namespace);
                        None
                    }
                };
                // UPDATE SPEC OBJECT WITH ACTUAL EXTENSIONS
                current_spec.spec.extensions = actual_extension;

                let conn_info =
                    get_pg_conn(client.clone(), &namespace, &data_plane_basedomain).await;

                types::StateToControlPlane {
                    data_plane_id: read_msg.message.data_plane_id,
                    event_id: read_msg.message.event_id,
                    event_type: Event::Restarted,
                    spec: Some(current_spec.spec),
                    connection: conn_info.ok(),
                }
            }
            _ => {
                warn!("Unhandled event_type: {:?}", read_msg.message.event_type);
                continue;
            }
        };
        let msg_id = queue.send(&data_plane_events_queue, &event_msg).await?;
        debug!("sent msg_id: {:?}", msg_id);

        // TODO (ianstanton) This is here as an example for now. We want to use
        //  this to ensure a CoreDB exists before we attempt to delete it.
        // Get all existing CoreDB
        let vec = get_all(client.clone(), "default");
        for pg in vec.await.iter() {
            info!("found CoreDB {}", pg.name_any());
        }
        thread::sleep(time::Duration::from_secs(1));

        // archive message from queue
        let archived = queue
            .archive(&control_plane_events_queue, &read_msg.msg_id)
            .await
            .expect("error archiving message from queue");
        // TODO(ianstanton) Improve logging everywhere
        info!("archived: {:?}", archived);
    }
}

fn main() {
    env_logger::init();
    info!("starting");
    run().unwrap();
}
