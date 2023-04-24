use conductor::{
    create_ing_route_tcp, create_namespace, create_networkpolicy, create_or_update, delete,
    delete_namespace, generate_spec, get_all, get_coredb_status, get_pg_conn, restart_statefulset,
    types,
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

    // Connect to pgmq
    let queue: PGMQueue = PGMQueue::new(pg_conn_url).await?;

    // Create queues if they do not exist
    queue.create(&control_plane_events_queue).await?;
    queue.create(&data_plane_events_queue).await?;

    // Infer the runtime environment and try to create a Kubernetes Client
    let client = Client::try_default().await?;

    // amount of time to wait after requeueing a message
    const REQUEUE_VT_SEC: i64 = 5;
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

        // TODO(chuckhend): recycled messages should get archived, logged, alerted
        // note: messages are recycled on purpose, so alerting probably needs to be
        // at some recycle count >= 20
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
                create_namespace(client.clone(), &namespace)
                    .await
                    .expect("error creating namespace");

                // create NetworkPolicy to allow internet access only
                create_networkpolicy(client.clone(), &namespace)
                    .await
                    .expect("error creating networkpolicy");

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

                let result = get_coredb_status(client.clone(), &namespace).await;

                // requeue if no status, no extensions, or if extensions are currently being updated
                let num_desired_extensions = match read_msg.message.spec.extensions {
                    Some(extensions) => extensions.len(),
                    None => 0,
                };
                let requeue: bool = match &result {
                    Ok(current_spec) => {
                        // if the coredb is still updating the extensions, requeue this task and try again in a few seconds
                        let status = current_spec.clone().status.expect("no status present");
                        let updating_extension = status.extensions_updating;
                        let no_extensions = match status.extensions {
                            Some(extensions) => {
                                // requeue if there are less extensions than desired
                                // likely means that the extensions are still being updated
                                extensions.len() < num_desired_extensions
                            }
                            None => true,
                        };
                        if updating_extension || no_extensions {
                            warn!(
                                "extensions updating: {}, no extensions: {}",
                                updating_extension, no_extensions
                            );
                            true
                        } else {
                            false
                        }
                    }
                    Err(err) => {
                        error!("error getting CoreDB status: {:?}", err);
                        true
                    }
                };

                if requeue {
                    // requeue then continue loop from beginning
                    let vt: chrono::DateTime<chrono::Utc> =
                        chrono::Utc::now() + chrono::Duration::seconds(REQUEUE_VT_SEC);
                    let _ = queue
                        .set_vt::<CRUDevent>(&control_plane_events_queue, &read_msg.msg_id, &vt)
                        .await?;
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
