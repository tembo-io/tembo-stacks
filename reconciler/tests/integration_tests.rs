// Include the #[ignore] macro on slow tests.
// That way, 'cargo test' does not run them by default.
// To run just these tests, use 'cargo test -- --ignored'
// To run all tests, use 'cargo test -- --include-ignored'
//
// https://doc.rust-lang.org/book/ch11-02-running-tests.html
//
// These tests assume there is already kubernetes running and you have a context configured.
// It also assumes that the CRD(s) and operator are already installed for this cluster.
// In this way, it can be used as a conformance test on a target, separate from installation.

#[cfg(test)]
mod test {

    use pgmq::PGMQueue;
    use k8s_openapi::{
        api::core::v1::{Namespace, Pod},
        apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition,
    };
    use kube::{
        api::{Patch, PatchParams},
        runtime::wait::{await_condition, conditions, Condition},
        Api, Client, Config,
    };
    use std::str;

    const API_VERSION: &str = "coredb.io/v1alpha1";

    fn is_pod_ready() -> impl Condition<Pod> + 'static {
        move |obj: Option<&Pod>| {
            if let Some(pod) = &obj {
                if let Some(status) = &pod.status {
                    if let Some(conds) = &status.conditions {
                        if let Some(pcond) = conds.iter().find(|c| c.type_ == "ContainersReady") {
                            return pcond.status == "True";
                        }
                    }
                }
            }
            false
        }
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_basic_create() {
        let queue: PGMQueue =
            PGMQueue::new("postgres://postgres:postgres@0.0.0.0:5432".to_owned()).await;

        let myqueue = "myqueue_control_plane".to_owned();
        queue.create(&myqueue).await;


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
    }
}
