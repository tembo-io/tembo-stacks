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

    use k8s_openapi::{
        api::core::v1::Pod,
        apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition,
    };
    use kube::{
        runtime::wait::{await_condition, conditions},
        Api, Client, Config,
    };
    use pgmq::PGMQueue;
    use rand::Rng;
    use reconciler::{coredb_crd as crd, types};
    use std::collections::BTreeMap;

    #[tokio::test]
    #[ignore]
    async fn functional_test_basic_create() {
        let queue: PGMQueue = PGMQueue::new("postgres://postgres:postgres@0.0.0.0:5432".to_owned())
            .await
            .unwrap();

        let myqueue = "myqueue_control_plane".to_owned();
        let _ = queue.create(&myqueue).await;

        // Configurations
        let mut rng = rand::thread_rng();
        let name = &format!("test-coredb-{}", rng.gen_range(0..100000));
        let namespace = name.clone();

        let limits: BTreeMap<String, String> = BTreeMap::from([
            ("cpu".to_owned(), "100m".to_string()),
            ("memory".to_owned(), "500Mi".to_string()),
        ]);

        // reconciler receives a CRUDevent from control plane
        let msg = types::CRUDevent {
            data_plane_id: "org_02s3owPQskuGXHE8vYsGSY".to_owned(),
            event_id: format!(
                "{name}.org_02s3owPQskuGXHE8vYsGSY.CoreDB.inst_02s4UKVbRy34SAYVSwZq2H",
                name = name
            ),
            event_type: types::Event::Create,
            dbname: name.clone(),
            spec: crd::CoreDBSpec {
                image: None,
                postgres_exporter_enabled: None,
                postgres_exporter_image: None,
                extensions: Some(vec![crd::CoreDBExtensions {
                    name: "postgis".to_owned(),
                    locations: vec![crd::CoreDBExtensionsLocations {
                        enabled: true,
                        version: Some("1.1.1".to_owned()),
                        schema: Some("public".to_owned()),
                        database: Some("postgres".to_owned())
                    }
                    ]
                }]),
                storage: Some("1Gi".to_owned()),
                port: None,
                replicas: Some(1),
                resources: Some(crd::CoreDBResources {
                    limits: Some(limits),
                    requests: None,
                }),
                uid: None,
            },
        };

        let msg_id = queue.send(&myqueue, &msg).await;
        println!("msg_id: {msg_id:?}");

        let client = kube_client().await;

        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);

        let timeout_seconds_start_pod = 90;

        let pod_name = format!("{name}-0");

        let _check_for_pod = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_seconds_start_pod),
            await_condition(pods.clone(), &pod_name, conditions::is_pod_running()),
        )
        .await
        .unwrap_or_else(|_| panic!("Did not find the pod {pod_name} to be running after waiting {timeout_seconds_start_pod} seconds"));
    }

    async fn kube_client() -> kube::Client {
        // Get the name of the currently selected namespace
        let kube_config = Config::infer()
            .await
            .expect("Please configure your Kubernetes context.");

        // Initialize the Kubernetes client
        let client =
            Client::try_from(kube_config.clone()).expect("Failed to initialize Kubernetes client");

        // Next, check that the currently selected namespace is labeled
        // to allow the running of tests.

        // Check that the CRD is installed
        let custom_resource_definitions: Api<CustomResourceDefinition> = Api::all(client.clone());

        let _check_for_crd = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            await_condition(
                custom_resource_definitions,
                "coredbs.coredb.io",
                conditions::is_crd_established(),
            ),
        )
        .await
        .expect("Custom Resource Definition for CoreDB was not found, do you need to install that before running the tests?");

        client
    }
}
