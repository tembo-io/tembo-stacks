# Reconciler

The reconciler is responsible for creating, updating, deleting database instances (custom resource) on a kubernetes cluster.
It runs in each data plane and performs these actions based on messages written to a queue in the control plane.
Upon connecting to this queue, it will continuously poll for new messages posted by the `cp-service` component. 
These messages are expected to be in the following format:
```json
{
    "body": {
      "cpu": "1",
      "memory": "2Gi",
      "postgres_image": "registry.developers.crunchydata.com/crunchydata/crunchy-postgres:ubi8-14.6-2",
      "resource_name": "example",
      "resource_type": "CoreDB",
      "storage": "1Gi"
    },
    "data_plane_id": "org_02s3owPQskuGXHE8vYsGSY",
    "event_id": "coredb-poc1.org_02s3owPQskuGXHE8vYsGSY.CoreDB.inst_02s4UKVbRy34SAYVSwZq2H",
    "message_type": "Create"
}
```

The reconciler will perform the following actions based on `message_type`:
- `Create` or `Update`
  - Create a namespace if it does not already exist.
  - Create an `IngressRouteTCP` object if it does not already exist.
  - Create or update `PostgresCluster` object.
- `Delete`
  - Delete `PostgresCluster`.
  - Delete namespace.

Once the reconciler performs these actions, the reconciler will send the following information back to a queue from which
`cp-service` will ready and flow back up to the UI:
```json
{
  "data_plane_id": "org_02s3owPQskuGXHE8vYsGSY",
  "event_id": "coredb-poc1.org_02s3owPQskuGXHE8vYsGSY.CoreDB.inst_02s4UKVbRy34SAYVSwZq2H",
  "event_meta": {
    "connection": "postgresql"
  }
}
```

## Local development
- env vars
- how to run locally
- how to run in cluster

- example application for posting messages to queue
```rust
use pgmq::{PGMQueue};

#[tokio::main]
async fn run() -> Result<(), sqlx::Error> {
    let queue: PGMQueue =
        PGMQueue::new("postgres://postgres:postgres@0.0.0.0:5433".to_owned()).await;

    let myqueue = "myqueue".to_owned();
    queue.create(&myqueue).await?;


    let msg = serde_json::json!({
    "body": {
      "cpu": "1",
      "memory": "2Gi",
      "postgres_image": "registry.developers.crunchydata.com/crunchydata/crunchy-postgres:ubi8-14.6-2",
      "resource_name": "example",
      "resource_type": "CoreDB",
      "storage": "1Gi"
    },
    "data_plane_id": "org_02s3owPQskuGXHE8vYsGSY",
    "event_id": "coredb-poc1.org_02s3owPQskuGXHE8vYsGSY.CoreDB.inst_02s4UKVbRy34SAYVSwZq2H",
    "message_type": "Create"
});
    let msg_id = queue.enqueue(&myqueue, &msg).await;
    println!("msg_id: {:?}", msg_id);

    Ok(())
}

fn main() {
    run().unwrap();
}
```
