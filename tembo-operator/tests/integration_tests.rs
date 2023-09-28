// Include the #[gnore] macro on slow tests
// That way, 'cargo test' does not run them by default.
// To run just these tests, use 'cargo test -- --ignored'
// To run all tests, use 'cargo test -- --include-ignored'
//
// https://doc.rust-lang.org/book/ch11-02-running-tests.html
//
// These tests assume there is already kubernetes running and you have a context configured.
// It also assumes that the CRD(s) and operator are already installed for this cluster.
// In this way, it can be used as a conformance test on a target, separate from installation.
//
// Do your best to keep the function names as unique as possible.  This will help with
// debugging and troubleshooting and also Rust seems to match like named tests and will run them
// at the same time.  This can cause issues if they are not independent.

#[cfg(test)]
mod test {
    use chrono::{DateTime, SecondsFormat, Utc};
    use controller::{
        apis::coredb_types::CoreDB,
        cloudnativepg::clusters::Cluster,
        defaults::{default_resources, default_storage},
        ingress_route_tcp_crd::IngressRouteTCP,
        is_pod_ready,
        psql::PsqlOutput,
        Context, State,
    };
    use k8s_openapi::{
        api::{
            apps::v1::Deployment,
            core::v1::{
                Container, Namespace, PersistentVolumeClaim, Pod, PodSpec, ResourceRequirements, Secret,
                Service,
            },
        },
        apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition,
        apimachinery::pkg::{api::resource::Quantity, apis::meta::v1::ObjectMeta},
    };
    use kube::{
        api::{AttachParams, DeleteParams, ListParams, Patch, PatchParams, PostParams},
        runtime::wait::{await_condition, conditions, Condition},
        Api, Client, Config, Error,
    };
    use rand::Rng;
    use std::{collections::BTreeSet, ops::Not, str, sync::Arc, thread, time::Duration};

    use tokio::io::AsyncReadExt;

    const API_VERSION: &str = "coredb.io/v1alpha1";
    // Timeout settings while waiting for an event
    const TIMEOUT_SECONDS_START_POD: u64 = 600;
    const TIMEOUT_SECONDS_POD_READY: u64 = 600;
    const TIMEOUT_SECONDS_SECRET_PRESENT: u64 = 120;
    const TIMEOUT_SECONDS_NS_DELETED: u64 = 120;
    const TIMEOUT_SECONDS_POD_DELETED: u64 = 120;
    const TIMEOUT_SECONDS_COREDB_DELETED: u64 = 120;

    async fn kube_client() -> Client {
        // Get the name of the currently selected namespace
        let kube_config = Config::infer()
            .await
            .expect("Please configure your Kubernetes context.");
        let selected_namespace = &kube_config.default_namespace;

        // Initialize the Kubernetes client
        let client = Client::try_from(kube_config.clone()).expect("Failed to initialize Kubernetes client");

        // Next, check that the currently selected namespace is labeled
        // to allow the running of tests.

        // List the namespaces with the specified labels
        let namespaces: Api<Namespace> = Api::all(client.clone());
        let namespace = namespaces.get(selected_namespace).await.unwrap();
        let labels = namespace.metadata.labels.unwrap();
        assert!(
            labels.contains_key("safe-to-run-coredb-tests"),
            "expected to find label 'safe-to-run-coredb-tests'"
        );
        assert_eq!(
            labels["safe-to-run-coredb-tests"], "true",
            "expected to find label 'safe-to-run-coredb-tests' with value 'true'"
        );

        // Check that the CRD is installed
        let custom_resource_definitions: Api<CustomResourceDefinition> = Api::all(client.clone());

        let _check_for_crd = tokio::time::timeout(
            Duration::from_secs(2),
            await_condition(
                custom_resource_definitions,
                "coredbs.coredb.io",
                conditions::is_crd_established(),
            ),
        )
        .await
        .expect("Custom Resource Definition for CoreDB was not found.");

        client
    }

    fn wait_for_secret() -> impl Condition<Secret> {
        |obj: Option<&Secret>| {
            if let Some(secret) = &obj {
                if let Some(t) = &secret.type_ {
                    return t == "Opaque";
                }
            }
            false
        }
    }

    async fn create_test_buddy(pods_api: Api<Pod>, name: String) -> String {
        // Launch a pod we can connect to if we want to
        // run commands inside the cluster.
        let test_pod_name = format!("test-buddy-{}", name);
        let pod = Pod {
            metadata: ObjectMeta {
                name: Some(test_pod_name.clone()),
                ..ObjectMeta::default()
            },
            spec: Some(PodSpec {
                containers: vec![Container {
                    command: Some(vec!["sleep".to_string()]),
                    args: Some(vec!["1200".to_string()]),
                    name: "test-connection".to_string(),
                    image: Some("curlimages/curl:latest".to_string()),
                    ..Container::default()
                }],
                restart_policy: Some("Never".to_string()),
                ..PodSpec::default()
            }),
            ..Pod::default()
        };

        let _pod = pods_api.create(&PostParams::default(), &pod).await.unwrap();

        test_pod_name
    }

    async fn run_command_in_container(
        pods_api: Api<Pod>,
        pod_name: String,
        command: Vec<String>,
        container: Option<String>,
    ) -> String {
        let attach_params = AttachParams {
            container: container.clone(),
            tty: false,
            stdin: true,
            stdout: true,
            stderr: true,
            max_stdin_buf_size: Some(1024),
            max_stdout_buf_size: Some(1024),
            max_stderr_buf_size: Some(1024),
        };

        let max_retries = 10;
        let millisec_between_tries = 5;

        for _i in 1..max_retries {
            let attach_res = pods_api.exec(pod_name.as_str(), &command, &attach_params).await;
            let mut attached_process = match attach_res {
                Ok(ap) => ap,
                Err(e) => {
                    println!(
                        "Error attaching to pod: {}, container: {:?}, error: {}",
                        pod_name, container, e
                    );
                    thread::sleep(Duration::from_millis(millisec_between_tries));
                    continue;
                }
            };
            let mut stdout_reader = attached_process.stdout().unwrap();
            let mut result_stdout = String::new();
            stdout_reader.read_to_string(&mut result_stdout).await.unwrap();

            return result_stdout;
        }
        panic!("Failed to run command in container");
    }

    async fn psql_with_retry(context: Arc<Context>, coredb_resource: CoreDB, query: String) -> PsqlOutput {
        // Wait up to 100 seconds
        for _ in 1..20 {
            // Assert extension no longer created
            if let Ok(result) = coredb_resource
                .psql(query.clone(), "postgres".to_string(), context.clone())
                .await
            {
                return result;
            }
            println!(
                "Waiting for psql result on DB {}...",
                coredb_resource.metadata.name.clone().unwrap()
            );
            thread::sleep(Duration::from_millis(5000));
        }
        panic!("Timed out waiting for psql result of '{}'", query);
    }

    async fn wait_until_psql_contains(
        context: Arc<Context>,
        coredb_resource: CoreDB,
        query: String,
        expected: String,
        inverse: bool,
    ) -> PsqlOutput {
        // Wait up to 200 seconds
        for _ in 1..40 {
            thread::sleep(Duration::from_millis(5000));
            // Assert extension no longer created
            let result = coredb_resource
                .psql(query.clone(), "postgres".to_string(), context.clone())
                .await;
            if let Ok(output) = result {
                match inverse {
                    true => {
                        if !output.stdout.clone().unwrap().contains(expected.clone().as_str()) {
                            return output;
                        }
                    }
                    false => {
                        if output.stdout.clone().unwrap().contains(expected.clone().as_str()) {
                            return output;
                        }
                    }
                }
            }
            println!(
                "Waiting for psql result on DB {}...",
                coredb_resource.metadata.name.clone().unwrap()
            );
        }
        if inverse {
            panic!(
                "Timed out waiting for psql result of '{}' to not contain {}",
                query, expected
            );
        }
        panic!(
            "Timed out waiting for psql result of '{}' to contain {}",
            query, expected
        );
    }

    async fn pod_ready_and_running(pods: Api<Pod>, pod_name: String) {
        println!("Waiting for pod to be running: {}", pod_name);
        let _check_for_pod = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_START_POD),
            await_condition(pods.clone(), &pod_name, conditions::is_pod_running()),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "Did not find the pod {} to be running after waiting {} seconds",
                pod_name, TIMEOUT_SECONDS_START_POD
            )
        });
        println!("Waiting for pod to be ready: {}", pod_name);
        let _check_for_pod_ready = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_POD_READY),
            await_condition(pods.clone(), &pod_name, is_pod_ready()),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "Did not find the pod {} to be ready after waiting {} seconds",
                pod_name, TIMEOUT_SECONDS_POD_READY
            )
        });
        println!("Found pod ready: {}", pod_name);
    }

    // Create namespace for the test to run in
    async fn create_namespace(client: Client, name: &str) -> Result<String, Error> {
        let ns_api: Api<Namespace> = Api::all(client);
        let params = ListParams::default().fields(&format!("metadata.name={}", name));
        let ns_list = ns_api.list(&params).await.unwrap();
        if !ns_list.items.is_empty() {
            return Ok(name.to_string());
        }

        println!("Creating namespace {}", name);
        let params = PatchParams::apply("tembo-integration-tests");
        let ns = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Namespace",
            "metadata": {
                "name": name,
                "labels": {
                    "tembo-pod-init.tembo.io/watch": "true",
                    "safe-to-run-coredb-tests": "true",
                    "kubernetes.io/metadata.name": name
                }
            }
        });
        let _o = ns_api.patch(name, &params, &Patch::Apply(ns)).await?;

        // return the name of the namespace
        Ok(name.to_string())
    }

    // Delete namespace
    async fn delete_namespace(client: Client, name: &str) -> Result<(), Error> {
        let ns_api: Api<Namespace> = Api::all(client);
        let params = ListParams::default().fields(&format!("metadata.name={}", name));
        let ns_list = ns_api.list(&params).await?;
        if ns_list.items.is_empty() {
            return Ok(());
        }

        println!("Deleting namespace {}", name);
        let params = DeleteParams::default();
        let _o = ns_api.delete(name, &params).await?;

        Ok(())
    }

    use controller::{apis::postgres_parameters::ConfigValue, errors};
    use k8s_openapi::NamespaceResourceScope;
    use serde::de::DeserializeOwned;
    use controller::apis::postgres_parameters::PgConfig;

    // helper function retrieve all instances of a resource in namespace
    // used repeatedly in appService tests
    // handles retries
    async fn list_resources<R>(
        client: Client,
        cdb_name: &str,
        namespace: &str,
        num_expected: usize,
    ) -> Result<Vec<R>, errors::OperatorError>
    where
        R: kube::api::Resource<Scope = NamespaceResourceScope>
            + std::fmt::Debug
            + 'static
            + Clone
            + DeserializeOwned
            + for<'de> serde::Deserialize<'de>,
        R::DynamicType: Default,
    {
        let api: Api<R> = Api::namespaced(client, namespace);
        let lp = ListParams::default().labels(format!("coredb.io/name={}", cdb_name).as_str());
        let retry = 10;
        let mut passed_retry = false;
        let mut resource_list: Vec<R> = Vec::new();
        for _ in 0..retry {
            let resources = api.list(&lp).await?;
            if resources.items.len() == num_expected {
                resource_list.extend(resources.items);
                passed_retry = true;
                break;
            }
            thread::sleep(Duration::from_millis(2000));
        }
        if passed_retry {
            Ok(resource_list)
        } else {
            Err(errors::ValueError::Invalid("Failed to get all resources in namespace".to_string()).into())
        }
    }


    #[tokio::test]
    #[ignore]
    async fn functional_test_basic_cnpg() {
        // Initialize the Kubernetes client
        let client = kube_client().await;
        let state = State::default();
        let _context = state.create_context(client.clone());

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let name = &format!("test-coredb-{}", suffix);
        let namespace = match create_namespace(client.clone(), name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };

        let kind = "CoreDB";
        let replicas = 1;

        // Create a pod we can use to run commands in the cluster
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);

        // Apply a basic configuration of CoreDB
        println!("Creating CoreDB resource {}", name);
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        // Generate basic CoreDB resource to start with
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
                "extensions": [{
                        // Try including an extension
                        // without specifying a schema
                        "name": "pg_jsonschema",
                        "description": "fake description",
                        "locations": [{
                            "enabled": true,
                            "version": "0.1.4",
                            "database": "postgres",
                        }],
                    }],
                "trunk_installs": [{
                        "name": "pg_jsonschema",
                        "version": "0.1.4",
                }]
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let _coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for CNPG Pod to be created
        let pod_name = format!("{}-1", name);

        pod_ready_and_running(pods.clone(), pod_name.clone()).await;

        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);
        let lp =
            ListParams::default().labels(format!("app=postgres-exporter,coredb.io/name={}", name).as_str());
        let exporter_pods = pods.list(&lp).await.expect("could not get pods");
        let exporter_pod_name = exporter_pods.items[0].metadata.name.as_ref().unwrap();
        println!("Exporter pod name: {}", &exporter_pod_name);

        pod_ready_and_running(pods.clone(), exporter_pod_name.clone()).await;

        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();
        let mut found_extension = false;
        for extension in coredb_resource.status.unwrap().extensions.unwrap() {
            for location in extension.locations {
                if extension.name == "pg_jsonschema" && location.enabled.unwrap() {
                    found_extension = true;
                    assert!(location.database == "postgres");
                    // Even though we set the schema to be empty, it should report public
                    // in the status
                    assert!(location.schema.unwrap() == "public");
                }
            }
        }
        assert!(found_extension);

        // CLEANUP TEST
        // Cleanup CoreDB
        coredbs.delete(name, &Default::default()).await.unwrap();
        println!("Waiting for CoreDB to be deleted: {}", &name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });
        println!("CoreDB resource deleted {}", name);

        // Delete namespace
        let _ = delete_namespace(client.clone(), &namespace).await;
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_cnpg_metrics_create() {
        // Initialize the Kubernetes client
        let client = kube_client().await;
        let state = State::default();
        let context = state.create_context(client.clone());

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let name = &format!("test-coredb-{}", suffix);
        let namespace = match create_namespace(client.clone(), name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };
        let kind = "CoreDB";
        let replicas = 1;

        // Create a pod we can use to run commands in the cluster
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);
        let test_pod_name = create_test_buddy(pods.clone(), name.to_string()).await;

        // Apply a basic configuration of CoreDB
        println!("Creating CoreDB resource {}", name);
        let test_metric_decr = format!("coredb_integration_test_{}", rng.gen_range(0..100000));
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
                "extensions": [
                    {
                        "name": "aggs_for_vecs",
                        "description": "aggs_for_vecs extension",
                        "locations": [{
                            "enabled": true,
                            "version": "1.3.0",
                            "database": "postgres",
                            "schema": "public"}
                        ]
                    }],
                "trunk_installs": [
                    {
                        "name": "aggs_for_vecs",
                        "version": "1.3.0",
                    }],
                "metrics": {
                    "enabled": true,
                    "queries": {
                        "test_ns": {
                            "query": "SELECT 10 as my_metric, 'cat' as animal",
                            "master": true,
                            "metrics": [
                              {
                                "my_metric": {
                                  "usage": "GAUGE",
                                  "description": test_metric_decr
                                }
                              },
                              {
                                "animal": {
                                    "usage": "LABEL",
                                    "description": "Animal type"
                                }
                              }
                            ]
                        },
                    }
                }
            }
        });
        let params = PatchParams::apply("coredb-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for secret to be created
        let secret_api: Api<Secret> = Api::namespaced(client.clone(), &namespace);
        let secret_name = format!("{}-exporter", name);
        println!("Waiting for secret to be created: {}", secret_name);
        let establish = await_condition(secret_api.clone(), &secret_name, wait_for_secret());
        let _ = tokio::time::timeout(Duration::from_secs(TIMEOUT_SECONDS_SECRET_PRESENT), establish)
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "Did not find the secret {} present after waiting {} seconds",
                    secret_name, TIMEOUT_SECONDS_SECRET_PRESENT
                )
            });
        println!("Found secret: {}", secret_name);

        // assert for postgres-exporter secret to be created
        let exporter_name = format!("{}-metrics", name);
        let exporter_secret_name = format!("{}-exporter", name);
        let exporter_secret = secret_api.get(&exporter_secret_name).await;
        match exporter_secret {
            Ok(secret) => {
                // assert for non-empty data in the secret
                assert!(
                    secret.data.map_or(false, |data| !data.is_empty()),
                    "postgres-exporter secret is empty!"
                );
            }
            Err(e) => panic!("Error getting postgres-exporter secret: {}", e),
        }

        // Wait for Pod to be created
        // This is the CNPG pod
        let pod_name = format!("{}-1", name);

        pod_ready_and_running(pods.clone(), pod_name.clone()).await;

        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);
        let lp =
            ListParams::default().labels(format!("app=postgres-exporter,coredb.io/name={}", name).as_str());
        let exporter_pods = pods.list(&lp).await.expect("could not get pods");
        let exporter_pod_name = exporter_pods.items[0].metadata.name.as_ref().unwrap();
        println!("Exporter pod name: {}", &exporter_pod_name);

        pod_ready_and_running(pods.clone(), exporter_pod_name.clone()).await;

        // assert that the postgres-exporter deployment was created
        let deploy_api: Api<Deployment> = Api::namespaced(client.clone(), &namespace);
        let exporter_deployment = deploy_api.get(exporter_name.clone().as_str()).await;
        assert!(
            exporter_deployment.is_ok(),
            "postgres-exporter Deployment does not exist: {:?}",
            exporter_deployment.err()
        );

        // Assert default storage values are applied to PVC
        let pvc_api: Api<PersistentVolumeClaim> = Api::namespaced(client.clone(), &namespace);
        let default_storage: Quantity = default_storage();

        // In CNPG, the PVC name is the same as the pod name
        let pvc = pvc_api.get(&pod_name.to_string()).await.unwrap();
        let storage = pvc.spec.unwrap().resources.unwrap().requests.unwrap();
        let s = storage.get("storage").unwrap().to_owned();
        assert_eq!(default_storage, s);

        // Assert default resource values are applied to postgres container
        let default_resources: ResourceRequirements = default_resources();
        let pg_pod = pods.get(&pod_name).await.unwrap();
        let resources = pg_pod.spec.unwrap().containers[0].clone().resources;
        assert_eq!(default_resources, resources.unwrap());

        // Assert no tables found
        let result = psql_with_retry(context.clone(), coredb_resource.clone(), "\\dt".to_string()).await;
        println!("psql out: {}", result.stdout.clone().unwrap());
        assert!(!result.stdout.clone().unwrap().contains("customers"));

        let result = psql_with_retry(
            context.clone(),
            coredb_resource.clone(),
            "
                CREATE TABLE customers (
                   id serial PRIMARY KEY,
                   name VARCHAR(50) NOT NULL,
                   email VARCHAR(50) NOT NULL UNIQUE,
                   created_at TIMESTAMP DEFAULT NOW()
                );
            "
            .to_string(),
        )
        .await;
        println!("{}", result.stdout.clone().unwrap());
        assert!(result.stdout.clone().unwrap().contains("CREATE TABLE"));

        // Assert table 'customers' exists
        let result = psql_with_retry(context.clone(), coredb_resource.clone(), "\\dt".to_string()).await;
        println!("{}", result.stdout.clone().unwrap());
        assert!(result.stdout.clone().unwrap().contains("customers"));

        let result = wait_until_psql_contains(
            context.clone(),
            coredb_resource.clone(),
            "select * from pg_extension;".to_string(),
            "aggs_for_vecs".to_string(),
            false,
        )
        .await;

        println!("{}", result.stdout.clone().unwrap());
        assert!(result.stdout.clone().unwrap().contains("aggs_for_vecs"));

        // Assert role 'postgres_exporter' was created
        let result = psql_with_retry(
            context.clone(),
            coredb_resource.clone(),
            "SELECT rolname FROM pg_roles;".to_string(),
        )
        .await;
        assert!(
            result.stdout.clone().unwrap().contains("postgres_exporter"),
            "results must contain postgres_exporter: {}",
            result.stdout.clone().unwrap()
        );

        // Assert we can curl the metrics from the service
        let metrics_service_name = format!("{}-metrics", name);
        let command = vec![
            String::from("curl"),
            format!("http://{metrics_service_name}/metrics"),
        ];
        let result_stdout =
            run_command_in_container(pods.clone(), test_pod_name.clone(), command, None).await;
        assert!(result_stdout.contains("pg_up 1"));
        println!("Found metrics when curling the metrics service");

        // assert custom queries made it to metric server
        let c = vec![
            "wget".to_owned(),
            "-qO-".to_owned(),
            "http://localhost:9187/metrics".to_owned(),
        ];
        let result_stdout = run_command_in_container(
            pods.clone(),
            exporter_pod_name.to_string(),
            c,
            Some("postgres-exporter".to_string()),
        )
        .await;
        assert!(result_stdout.contains(&test_metric_decr));

        // Assert we can drop an extension after its been created
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
                "extensions": [
                    {
                        "name": "aggs_for_vecs",
                        "description": "aggs_for_vecs extension",
                        "locations": [{
                            "enabled": false,
                            "version": "1.3.0",
                            "database": "postgres",
                            "schema": "public"}
                        ]
                    }]
            }
        });

        // Apply crd with extension disabled
        let params = PatchParams::apply("coredb-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        let result = wait_until_psql_contains(
            context.clone(),
            coredb_resource.clone(),
            "select extname from pg_catalog.pg_extension;".to_string(),
            "aggs_for_vecs".to_string(),
            true,
        )
        .await;

        assert!(
            !result.stdout.clone().unwrap().contains("aggs_for_vecs"),
            "results should not contain aggs_for_vecs: {}",
            result.stdout.clone().unwrap()
        );

        // assert extensions made it into the status
        let spec = coredbs.get(name).await.expect("spec not found");
        let status = spec.status.expect("no status on coredb");
        let extensions = status.extensions;
        assert!(!extensions.clone().expect("expected extensions").is_empty());
        assert!(!extensions.expect("expected extensions")[0]
            .description
            .clone()
            .expect("expected a description")
            .is_empty());

        // Change size of a PVC
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "storage": "10Gi",
            }
        });
        let params = PatchParams::apply("coredb-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let _ = coredbs.patch(name, &params, &patch).await.unwrap();
        thread::sleep(Duration::from_millis(10000));
        let pvc = pvc_api.get(&pod_name.to_string()).await.unwrap();
        // checking that the request is set, but its not the status
        // https://github.com/rancher/local-path-provisioner/issues/323
        let storage = pvc.spec.unwrap().resources.unwrap().requests.unwrap();
        let s = storage.get("storage").unwrap().to_owned();
        assert_eq!(Quantity("10Gi".to_owned()), s);

        // Cleanup test buddy pod resource
        pods.delete(&test_pod_name, &Default::default()).await.unwrap();
        println!("Waiting for test buddy pod to be deleted: {}", &test_pod_name);
        let _assert_test_buddy_pod_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_POD_DELETED),
            await_condition(pods.clone(), &test_pod_name, conditions::is_deleted("")),
        );

        // Cleanup CoreDB resource
        coredbs.delete(name, &Default::default()).await.unwrap();
        println!("Waiting for CoreDB to be deleted: {}", &name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });
        println!("CoreDB resource deleted {}", name);

        // Delete namespace
        let _ = delete_namespace(client, &namespace).await;
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_cnpg_pgparams() {
        // Initialize the Kubernetes client
        let client = kube_client().await;
        let state = State::default();
        let context = state.create_context(client.clone());

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let name = &format!("test-coredb-{}", suffix);
        let namespace = match create_namespace(client.clone(), name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };

        let kind = "CoreDB";
        let replicas = 1;

        // Create a pod we can use to run commands in the cluster
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);

        // Apply a basic configuration of CoreDB
        println!("Creating CoreDB resource {}", name);
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        // Generate CoreDB resource with params
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
                "trunk_installs": [
                    {
                        "name": "pg_partman",
                        "version": "4.7.3",
                    },
                    {
                        "name": "pgmq",
                        "version": "0.10.0",
                    },
                    {
                        "name": "pg_stat_statements",
                        "version": "1.10.0",
                    },
                ],
                "extensions": [
                    {
                        "name": "pg_partman",
                        "locations": [
                        {
                          "enabled": true,
                          "version": "4.7.3",
                          "database": "postgres",
                          "schema": "public"
                        }]
                    },
                    {
                        "name": "pgmq",
                        "locations": [
                        {
                          "enabled": true,
                          "version": "0.10.0",
                          "database": "postgres",
                          "schema": "public"
                        }]
                    },
                    {
                        "name": "pg_stat_statements",
                        "locations": [
                        {
                          "enabled": true,
                          "version": "1.10.0",
                          "database": "postgres",
                          "schema": "public"
                        }]
                    }
                ],
                "runtime_config": [
                    {
                        "name": "shared_preload_libraries",
                        "value": "pg_stat_statements,pg_partman_bgw"
                    },
                    {
                        "name": "pg_partman_bgw.interval",
                        "value": "60"
                    },
                    {
                        "name": "pg_partman_bgw.role",
                        "value": "postgres"
                    },
                    {
                        "name": "pg_partman_bgw.dbname",
                        "value": "postgres"
                    }
                ]
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for CNPG Pod to be created
        let pod_name = format!("{}-1", name);

        pod_ready_and_running(pods.clone(), pod_name.clone()).await;

        println!("Waiting to install extension pgmq");

        let result = wait_until_psql_contains(
            context.clone(),
            coredb_resource.clone(),
            "select extname from pg_catalog.pg_extension;".to_string(),
            "pgmq".to_string(),
            false,
        )
        .await;

        println!("{}", result.stdout.clone().unwrap());
        assert!(result.stdout.clone().unwrap().contains("pgmq"));

        println!("Restarting CNPG pod");
        // Restart the CNPG instance
        let cluster: Api<Cluster> = Api::namespaced(client.clone(), &namespace);
        let restart = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true).to_string();

        // To restart the CNPG pod we need to annotate the Cluster resource with
        // kubectl.kubernetes.io/restartedAt: <timestamp>
        let patch_json = serde_json::json!({
            "metadata": {
                "annotations": {
                    "kubectl.kubernetes.io/restartedAt": restart
                }
            }
        });

        // Use the patch method to update the Cluster resource
        let params = PatchParams::default();
        let patch = Patch::Merge(patch_json);
        let _patch = cluster.patch(name, &params, &patch);

        let _result = wait_until_psql_contains(
            context.clone(),
            coredb_resource.clone(),
            "show shared_preload_libraries;".to_string(),
            "pg_stat_statements,pg_partman_bgw".to_string(),
            false,
        )
        .await;

        // Assert that shared_preload_libraries contains pg_stat_statements
        // and pg_partman_bgw

        let result = psql_with_retry(
            context.clone(),
            coredb_resource.clone(),
            "show shared_preload_libraries;".to_string(),
        )
        .await;

        let stdout = match result.stdout {
            Some(output) => output,
            None => panic!("stdout is None"),
        };

        assert!(stdout.contains("pg_stat_statements,pg_partman_bgw"));

        // CLEANUP TEST
        // Cleanup CoreDB
        coredbs.delete(name, &Default::default()).await.unwrap();
        println!("Waiting for CoreDB to be deleted: {}", &name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });
        println!("CoreDB resource deleted {}", name);

        // Delete namespace
        let _ = delete_namespace(client.clone(), &namespace).await;
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_skip_reconciliation() {
        // Initialize the Kubernetes client
        let client = kube_client().await;
        let state = State::default();
        let _context = state.create_context(client.clone());

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let name = &format!("test-coredb-{}", suffix);
        let namespace = match create_namespace(client.clone(), name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };
        let kind = "CoreDB";
        let replicas = 1;

        // Create a pod we can use to run commands in the cluster
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);

        // Apply a basic configuration of CoreDB
        println!("Creating CoreDB resource {}", name);
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name,
                "annotations": {
                    "coredbs.coredb.io/watch": "false"
                }
            },
            "spec": {
                "replicas": replicas,
            }
        });
        let params = PatchParams::apply("coredb-integration-test-skip-reconciliation");
        let patch = Patch::Apply(&coredb_json);
        let _coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for the pod to be created (it shouldn't be created)
        thread::sleep(Duration::from_millis(5000));

        // Assert that the CoreDB object contains the correct annotation
        let coredb = coredbs.get(name).await.unwrap();
        let annotations = coredb.metadata.annotations.as_ref().unwrap();
        assert_eq!(
            annotations.get("coredbs.coredb.io/watch"),
            Some(&String::from("false"))
        );

        // Assert that the pod was not created
        let expected_pod_name = format!("{}-{}", name, 0);
        let pod = pods.get(&expected_pod_name).await;
        assert!(pod.is_err());

        // Cleanup CoreDB resource
        coredbs.delete(name, &Default::default()).await.unwrap();
        println!("Waiting for CoreDB to be deleted: {}", &name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });
        println!("CoreDB resource deleted {}", name);

        // Delete namespace
        let _ = delete_namespace(client.clone(), &namespace).await;
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_delete_namespace() {
        // Initialize the Kubernetes client
        let client = kube_client().await;
        let state = State::default();
        let _context = state.create_context(client.clone());

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let name = &format!("test-coredb-{}", suffix);
        let namespace = match create_namespace(client.clone(), name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };
        let replicas = 1;

        let ns_api: Api<Namespace> = Api::all(client.clone());

        // Create coredb
        println!("Creating CoreDB resource {}", name);
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": "CoreDB",
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
                "trunk_installs": [
                    {
                        "name": "aggs_for_vecs",
                        "version": "1.3.0",
                    },
                ],
                "extensions": [
                    {
                        "name": "aggs_for_vecs",
                        "description": "aggs_for_vecs extension",
                        "locations": [{
                            "enabled": false,
                            "version": "1.3.0",
                            "database": "postgres",
                            "schema": "public"}
                        ]
                    }]
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        coredbs.patch(name, &params, &patch).await.unwrap();

        // Assert coredb is running
        let pod_name = format!("{}-1", name);
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);
        pod_ready_and_running(pods.clone(), pod_name.clone()).await;

        // Delete namespace
        let _ = delete_namespace(client.clone(), &namespace).await;

        // Assert coredb has been deleted
        // TODO(ianstanton) This doesn't assert the object is gone for good. Tried implementing something
        //  similar to the loop used in namespace delete assertion, but received a comparison error.
        println!("Waiting for CoreDB to be deleted: {}", &name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });

        // Assert namespace has been deleted
        println!("Waiting for namespace to be deleted: {}", &namespace);
        let namespace_clone = namespace.clone();
        tokio::time::timeout(Duration::from_secs(TIMEOUT_SECONDS_NS_DELETED), async move {
            loop {
                let get_ns = ns_api.get_opt(&namespace_clone).await.unwrap();
                if get_ns.is_none() {
                    break;
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "Namespace {} was not deleted after waiting {} seconds",
                namespace, TIMEOUT_SECONDS_NS_DELETED
            )
        });
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_ingress_route_tcp() {
        // Initialize the Kubernetes client
        let client = kube_client().await;

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let name = &format!("test-coredb-{}", suffix.clone());
        let namespace = match create_namespace(client.clone(), name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };
        let kind = "CoreDB";
        let replicas = 1;

        // Create a pod we can use to run commands in the cluster
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);

        // Apply a basic configuration of CoreDB
        println!("Creating CoreDB resource {}", name);
        let _test_metric_decr = format!("coredb_integration_test_{}", suffix.clone());
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
            }
        });
        let params = PatchParams::apply("functional-test-ingress-route-tcp");
        let patch = Patch::Apply(&coredb_json);
        let _coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for Pod to be created
        let pod_name = format!("{}-1", name);
        let _check_for_pod = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_START_POD),
            await_condition(pods.clone(), &pod_name, conditions::is_pod_running()),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "Did not find the pod {} to be running after waiting {} seconds",
                pod_name, TIMEOUT_SECONDS_START_POD
            )
        });

        let ing_route_tcp_name = format!("{}-rw-0", name);
        let ingress_route_tcp_api: Api<IngressRouteTCP> = Api::namespaced(client.clone(), &namespace);
        // Get the ingress route tcp
        let ing_route_tcp = ingress_route_tcp_api
            .get(&ing_route_tcp_name)
            .await
            .unwrap_or_else(|_| panic!("Expected to find ingress route TCP {}", ing_route_tcp_name));
        let service_name = ing_route_tcp.spec.routes[0]
            .services
            .clone()
            .expect("Ingress route has no services")[0]
            .name
            .clone();
        // Assert the ingress route tcp service points to coredb service
        // The coredb service is named the same as the coredb resource
        assert_eq!(&service_name, format!("{}-rw", name).as_str());

        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "extra_domains_rw": ["any-given-domain.com", "another-domain.com"]
            }
        });
        let params = PatchParams::apply("functional-test-ingress-route-tcp");
        let patch = Patch::Merge(&coredb_json);
        let _coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // extra domains should be created almost right away, within a few milliseconds
        tokio::time::sleep(Duration::from_secs(5)).await;

        let ing_route_tcp_name = format!("extra-{}-rw", name);
        let ingress_route_tcp_api: Api<IngressRouteTCP> = Api::namespaced(client.clone(), &namespace);
        // Get the ingress route tcp
        let ing_route_tcp = ingress_route_tcp_api
            .get(&ing_route_tcp_name)
            .await
            .unwrap_or_else(|_| panic!("Expected to find ingress route TCP {}", ing_route_tcp_name));
        let service_name = ing_route_tcp.spec.routes[0]
            .services
            .clone()
            .expect("Ingress route has no services")[0]
            .name
            .clone();
        // Assert the ingress route tcp service points to coredb service
        // The coredb service is named the same as the coredb resource
        assert_eq!(&service_name, format!("{}-rw", name).as_str());
        let matcher = ing_route_tcp.spec.routes[0].r#match.clone();
        assert_eq!(
            matcher,
            "HostSNI(`another-domain.com`) || HostSNI(`any-given-domain.com`)"
        );

        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "extra_domains_rw": ["new-domain.com"]
            }
        });
        let params = PatchParams::apply("functional-test-ingress-route-tcp");
        let patch = Patch::Merge(&coredb_json);
        let _coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // extra domains should be created almost right away, within a few milliseconds
        tokio::time::sleep(Duration::from_secs(5)).await;

        // Get the ingress route tcp
        let ing_route_tcp = ingress_route_tcp_api
            .get(&ing_route_tcp_name)
            .await
            .unwrap_or_else(|_| panic!("Expected to find ingress route TCP {}", ing_route_tcp_name));
        let service_name = ing_route_tcp.spec.routes[0]
            .services
            .clone()
            .expect("Ingress route has no services")[0]
            .name
            .clone();
        // Assert the ingress route tcp service points to coredb service
        // The coredb service is named the same as the coredb resource
        assert_eq!(&service_name, format!("{}-rw", name).as_str());
        let matcher = ing_route_tcp.spec.routes[0].r#match.clone();
        assert_eq!(matcher, "HostSNI(`new-domain.com`)");

        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
                "extra_domains_rw": [],
            }
        });
        let params = PatchParams::apply("functional-test-ingress-route-tcp").force();
        let patch = Patch::Apply(&coredb_json);
        let _coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        tokio::time::sleep(Duration::from_secs(5)).await;

        // Get the ingress route tcp
        let ing_route_tcp = ingress_route_tcp_api.get(&ing_route_tcp_name).await;
        // Should be deleted
        assert!(ing_route_tcp.is_err());

        // Cleanup CoreDB resource
        coredbs.delete(name, &Default::default()).await.unwrap();
        println!("Waiting for CoreDB to be deleted: {}", &name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });
        println!("CoreDB resource deleted {}", name);

        // Delete namespace
        let _ = delete_namespace(client.clone(), &namespace).await;
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_ingress_route_tcp_adopt_existing_ing_route_tcp() {
        // Initialize the Kubernetes client
        let client = kube_client().await;

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let name = &format!("test-coredb-{}", suffix.clone());
        let namespace = match create_namespace(client.clone(), name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };
        let kind = "CoreDB";
        let replicas = 1;

        // Create an ingress route tcp to be adopted
        let ing = serde_json::json!({
            "apiVersion": "traefik.containo.us/v1alpha1",
            "kind": "IngressRouteTCP",
            "metadata": {
                "name": name,
            },
            "spec": {
                "entryPoints": ["postgresql"],
                "routes": [
                    {
                        "match": format!("HostSNI(`{name}.localhost`)"),
                        "services": [
                            {
                                "name": format!("{name}"),
                                "port": 5432,
                            },
                        ],
                    },
                ],
                "tls": {
                    "passthrough": true,
                },
            },
        });

        let ingress_route_tcp_api: Api<IngressRouteTCP> = Api::namespaced(client.clone(), &namespace);
        let params = PatchParams::apply("functional-test-ingress-route-tcp");
        let _o = ingress_route_tcp_api
            .patch(name, &params, &Patch::Apply(&ing))
            .await
            .unwrap();

        // Create a pod we can use to run commands in the cluster
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);

        // Apply a basic configuration of CoreDB
        println!("Creating CoreDB resource {}", &name);
        let _test_metric_decr = format!("coredb_integration_test_{}", suffix.clone());
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
            }
        });
        let patch = Patch::Apply(&coredb_json);
        let _coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for Pod to be created
        let pod_name = format!("{}-1", name);
        pod_ready_and_running(pods.clone(), pod_name.clone()).await;

        // This TCP route should not exist, because instead we adopted the existing one
        let ing_route_tcp_name = format!("{}-rw-0", name);
        // Get the ingress route tcp
        let get_result = ingress_route_tcp_api.get(&ing_route_tcp_name).await;
        assert!(
            get_result.is_err(),
            "Expected to not find ingress route TCP with name {}",
            ing_route_tcp_name
        );

        // This TCP route is the one we adopted
        let _get_result = ingress_route_tcp_api
            .get(name)
            .await
            .unwrap_or_else(|_| panic!("Expected to find ingress route TCP {}", name));

        // Cleanup CoreDB resource
        coredbs.delete(name, &Default::default()).await.unwrap();
        println!("Waiting for CoreDB to be deleted: {}", &name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });
        println!("CoreDB resource deleted {}", name);

        // Delete namespace
        let _ = delete_namespace(client.clone(), &namespace).await;
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_ingress_route_tcp_adopt_existing_and_dont_break_domain_name() {
        // Initialize the Kubernetes client
        let client = kube_client().await;

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let name = &format!("test-coredb-{}", suffix.clone());
        let namespace = match create_namespace(client.clone(), name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };
        let kind = "CoreDB";
        let replicas = 1;

        let old_matcher = format!("HostSNI(`{name}.other-host`)");
        // Create an ingress route tcp to be adopted
        let ing = serde_json::json!({
            "apiVersion": "traefik.containo.us/v1alpha1",
            "kind": "IngressRouteTCP",
            "metadata": {
                "name": name,
            },
            "spec": {
                "entryPoints": ["postgresql"],
                "routes": [
                    {
                        "match": old_matcher,
                        "services": [
                            {
                                "name": "incorrect-service-name",
                                "port": 1234,
                            },
                        ],
                    },
                ],
                "tls": {
                    "passthrough": true,
                },
            },
        });

        let ingress_route_tcp_api: Api<IngressRouteTCP> = Api::namespaced(client.clone(), &namespace);
        let params = PatchParams::apply("functional-test-ingress-route-tcp");
        let _o = ingress_route_tcp_api
            .patch(name, &params, &Patch::Apply(&ing))
            .await
            .unwrap();

        // Create a pod we can use to run commands in the cluster
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);

        // Apply a basic configuration of CoreDB
        println!("Creating CoreDB resource {}", &name);
        let _test_metric_decr = format!("coredb_integration_test_{}", suffix.clone());
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
            }
        });
        let patch = Patch::Apply(&coredb_json);
        let _coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for Pod to be created
        let pod_name = format!("{}-1", name);
        pod_ready_and_running(pods.clone(), pod_name.clone()).await;

        // This TCP route is the one we adopted
        let ingress_route_tcp = ingress_route_tcp_api
            .get(name)
            .await
            .unwrap_or_else(|_| panic!("Expected to find ingress route TCP {}", name));

        let actual_matcher_adopted_route = ingress_route_tcp.spec.routes[0].r#match.clone();
        assert_eq!(actual_matcher_adopted_route, old_matcher);

        let new_matcher = format!("HostSNI(`{name}.localhost`)");
        // This TCP route is the new one
        let ing_route_tcp_name = format!("{}-rw-0", name);
        let ingress_route_tcp = ingress_route_tcp_api
            .get(ing_route_tcp_name.as_str())
            .await
            .unwrap_or_else(|_| panic!("Expected to find ingress route TCP {}", name));

        let actual_matcher_new_route = ingress_route_tcp.spec.routes[0].r#match.clone();
        assert_eq!(actual_matcher_new_route, new_matcher);

        // Delete ingress_route_tcp_api
        let _ = ingress_route_tcp_api.delete(name, &Default::default()).await;

        // Cleanup CoreDB resource
        coredbs.delete(name, &Default::default()).await.unwrap();
        println!("Waiting for CoreDB to be deleted: {}", &name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });
        println!("CoreDB resource deleted {}", name);

        // Delete namespace
        let _ = delete_namespace(client.clone(), &namespace).await;
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_ha_basic_cnpg() {
        // Initialize the Kubernetes client
        let client = kube_client().await;
        let state = State::default();
        let context = state.create_context(client.clone());

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let name = &format!("test-coredb-{}", suffix);
        let namespace = match create_namespace(client.clone(), name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };

        let kind = "CoreDB";
        let replicas = 2;

        // Create a pod we can use to run commands in the cluster
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);

        // Apply a basic configuration of CoreDB
        println!("Creating CoreDB resource {}", name);
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        // Generate basic CoreDB resource to start with
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for CNPG Pod to be created
        let pod_name_primary = format!("{}-1", name);
        let pod_name_secondary = format!("{}-2", name);

        pod_ready_and_running(pods.clone(), pod_name_primary.clone()).await;
        pod_ready_and_running(pods.clone(), pod_name_secondary.clone()).await;

        // Assert that we can query the database with \dt;
        let result = psql_with_retry(context.clone(), coredb_resource.clone(), "\\dx".to_string()).await;
        assert!(result.stdout.clone().unwrap().contains("plpgsql"));

        // Assert that both pods are replicating successfully
        let result = psql_with_retry(
            context.clone(),
            coredb_resource.clone(),
            "SELECT state FROM pg_stat_replication".to_string(),
        )
        .await;
        assert!(result.stdout.clone().unwrap().contains("streaming"));

        // CLEANUP TEST
        // Cleanup CoreDB
        coredbs.delete(name, &Default::default()).await.unwrap();
        println!("Waiting for CoreDB to be deleted: {}", &name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });
        println!("CoreDB resource deleted {}", name);

        // Delete namespace
        let _ = delete_namespace(client.clone(), &namespace).await;
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_ha_upgrade_cnpg() {
        // Initialize the Kubernetes client
        let client = kube_client().await;
        let state = State::default();
        let context = state.create_context(client.clone());

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let name = &format!("test-coredb-{}", suffix);
        let namespace = match create_namespace(client.clone(), name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };

        let kind = "CoreDB";
        let replicas = 1;

        // Create a pod we can use to run commands in the cluster
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);

        // Apply a basic configuration of CoreDB
        println!("Creating CoreDB resource {}", name);
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        // Generate basic CoreDB resource to start with
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for CNPG Pod to be created
        let pod_name_primary = format!("{}-1", name);
        pod_ready_and_running(pods.clone(), pod_name_primary.clone()).await;

        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);
        let lp =
            ListParams::default().labels(format!("app=postgres-exporter,coredb.io/name={}", name).as_str());
        let exporter_pods = pods.list(&lp).await.expect("could not get pods");
        let exporter_pod_name = exporter_pods.items[0].metadata.name.as_ref().unwrap();
        println!("Exporter pod name: {}", &exporter_pod_name);

        pod_ready_and_running(pods.clone(), exporter_pod_name.clone()).await;

        // Assert that we can query the database with \dx;
        let result = psql_with_retry(context.clone(), coredb_resource.clone(), "\\dx".to_string()).await;
        assert!(result.stdout.clone().unwrap().contains("plpgsql"));

        // Now upgrade the single instance to be HA
        let replicas = 2;
        // Generate HA CoreDB resource
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for new CNPG secondary Pod to be created and running
        let pod_name_secondary = format!("{}-2", name);
        pod_ready_and_running(pods.clone(), pod_name_secondary.clone()).await;

        // Assert that we can query the database again now that HA is enabled with \dx;
        let result = psql_with_retry(context.clone(), coredb_resource.clone(), "\\dx".to_string()).await;
        assert!(result.stdout.clone().unwrap().contains("plpgsql"));

        // Assert that both pods are replicating successfully
        let result = psql_with_retry(
            context.clone(),
            coredb_resource.clone(),
            "SELECT state FROM pg_stat_replication".to_string(),
        )
        .await;
        assert!(result.stdout.clone().unwrap().contains("streaming"));

        // Revert replicas back to 1 to disable HA
        let replicas = 1;
        // Generate HA CoreDB resource
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
            }
        });
        println!("Disabling HA by setting replicas to {}", replicas);
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for secondary CNPG pod to be deleted
        println!("Waiting for Pod {} to be deleted", pod_name_secondary);
        let _assert_secondary_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_POD_DELETED),
            await_condition(pods.clone(), &pod_name_secondary, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "Pod {} was not deleted after waiting {} seconds",
                pod_name_secondary, TIMEOUT_SECONDS_POD_DELETED
            )
        });

        // Query the database again to ensure that pg_replication_slots is empty
        wait_until_psql_contains(
            context.clone(),
            coredb_resource.clone(),
            "SELECT count(*) from pg_replication_slots".to_string(),
            "0".to_string(),
            false,
        )
        .await;

        // CLEANUP TEST
        // Cleanup CoreDB
        coredbs.delete(name, &Default::default()).await.unwrap();
        println!("Waiting for CoreDB to be deleted: {}", &name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });
        println!("CoreDB resource deleted {}", name);

        // Delete namespace
        let _ = delete_namespace(client.clone(), &namespace).await;
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_shared_preload_libraries() {
        // Initialize the Kubernetes client
        let client = kube_client().await;
        let state = State::default();
        let context = state.create_context(client.clone());

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let name = &format!("test-requires-load-{}", suffix);
        let namespace = match create_namespace(client.clone(), name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };

        let kind = "CoreDB";
        let replicas = 1;

        // Create a pod we can use to run commands in the cluster
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);

        // Apply a basic configuration of CoreDB
        println!("Creating CoreDB resource {}", name);
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        // Generate basic CoreDB resource to start with
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
                "extensions": [{
                        "name": "pg_cron",
                        "description": "cron",
                        "locations": [{
                            "enabled": true,
                            "version": "1.5.2",
                            "database": "postgres",
                        }],
                    },
                    {
                        "name": "citus",
                        "description": "citus",
                        "locations": [{
                            "enabled": true,
                            "version": "12.0.1",
                            "database": "postgres",
                        }],
                }],
                "trunk_installs": [{
                        "name": "pg_cron",
                        "version": "1.5.2",
                },
                {
                        "name": "citus",
                        "version": "12.0.1",
                }]
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for CNPG Pod to be created
        let pod_name = format!("{}-1", name);

        pod_ready_and_running(pods.clone(), pod_name.clone()).await;

        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);
        let lp =
            ListParams::default().labels(format!("app=postgres-exporter,coredb.io/name={}", name).as_str());
        let exporter_pods = pods.list(&lp).await.expect("could not get pods");
        let exporter_pod_name = exporter_pods.items[0].metadata.name.as_ref().unwrap();
        println!("Exporter pod name: {}", &exporter_pod_name);

        pod_ready_and_running(pods.clone(), exporter_pod_name.clone()).await;

        wait_until_psql_contains(
            context.clone(),
            coredb_resource.clone(),
            "\\dx".to_string(),
            "pg_cron".to_string(),
            false,
        )
        .await;

        wait_until_psql_contains(
            context.clone(),
            coredb_resource.clone(),
            "\\dx".to_string(),
            "citus".to_string(),
            false,
        )
        .await;

        let coredb_resource = coredbs.get(name).await.unwrap();
        let mut found_citus = false;
        let mut found_cron = false;
        for extension in coredb_resource.status.unwrap().extensions.unwrap() {
            for location in extension.locations {
                if extension.name == "citus" && location.enabled.unwrap() {
                    found_citus = true;
                    assert!(location.database == "postgres");
                    assert!(location.schema.clone().unwrap() == "pg_catalog");
                }
                if extension.name == "pg_cron" && location.enabled.unwrap() {
                    found_cron = true;
                    assert!(location.database == "postgres");
                    assert!(location.schema.unwrap() == "pg_catalog");
                }
            }
        }
        assert!(found_citus);
        assert!(found_cron);

        // CLEANUP TEST
        // Cleanup CoreDB
        coredbs.delete(name, &Default::default()).await.unwrap();
        println!("Waiting for CoreDB to be deleted: {}", &name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });
        println!("CoreDB resource deleted {}", name);

        // Delete namespace
        let _ = delete_namespace(client.clone(), &namespace).await;
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_ha_two_replicas() {
        // Initialize the Kubernetes client
        let client = kube_client().await;
        let state = State::default();
        let context = state.create_context(client.clone());

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let name = &format!("test-coredb-{}", suffix);
        let namespace = match create_namespace(client.clone(), name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };

        let kind = "CoreDB";
        let replicas = 2;

        // Create a pod we can use to run commands in the cluster
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);

        // Apply a basic configuration of CoreDB
        println!("Creating CoreDB resource {}", name);
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        // Generate basic CoreDB resource to start with
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for CNPG Pod to be created
        let pod_name_primary = format!("{}-1", name);
        pod_ready_and_running(pods.clone(), pod_name_primary.clone()).await;

        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);
        let lp =
            ListParams::default().labels(format!("app=postgres-exporter,coredb.io/name={}", name).as_str());
        let exporter_pods = pods.list(&lp).await.expect("could not get pods");
        let exporter_pod_name = exporter_pods.items[0].metadata.name.as_ref().unwrap();
        println!("Exporter pod name: {}", &exporter_pod_name);

        // Wait for CNPG Cluster to be created by looping over replicas until
        // they are in a running state
        for i in 1..=replicas {
            let pod_name = format!("{}-{}", name, i);
            pod_ready_and_running(pods.clone(), pod_name).await;
        }

        // Assert that we can query the database with \dx;
        let result = psql_with_retry(context.clone(), coredb_resource.clone(), "\\dx".to_string()).await;
        assert!(result.stdout.clone().unwrap().contains("plpgsql"));

        // Assert that both pods are replicating successfully
        let result = psql_with_retry(
            context.clone(),
            coredb_resource.clone(),
            "SELECT state FROM pg_stat_replication".to_string(),
        )
        .await;
        assert!(result.stdout.clone().unwrap().contains("streaming"));

        // Add in an extension and lets make sure it's installed on all pods
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
                "trunk_installs": [
                    {
                        "name": "aggs_for_vecs",
                        "version": "1.3.0",
                    },
                ],
                "extensions": [
                    {
                        "name": "aggs_for_vecs",
                        "description": "aggs_for_vecs extension",
                        "locations": [{
                            "enabled": false,
                            "version": "1.3.0",
                            "database": "postgres",
                            "schema": "public"}
                        ]
                    }]
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait until the extension is installed
        wait_until_psql_contains(
            context.clone(),
            coredb_resource.clone(),
            "select extname from pg_catalog.pg_extension;".to_string(),
            "aggs_for_vecs".to_string(),
            true,
        )
        .await;

        // Assert that the extensions are installed on both replicas
        let retrieved_pods_result = coredb_resource.pods_by_cluster(client.clone()).await;

        let retrieved_pods = match retrieved_pods_result {
            Ok(pods_list) => pods_list,
            Err(e) => {
                panic!("Failed to retrieve pods: {:?}", e);
            }
        };
        for pod in &retrieved_pods {
            let cmd = vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "ls /var/lib/postgresql/data/tembo/extension/aggs_for_vecs.control".to_owned(),
            ];
            let pod_name = pod.metadata.name.clone().expect("Pod should have a name");
            let result =
                run_command_in_container(pods.clone(), pod_name, cmd.clone(), Some("postgres".to_string()))
                    .await;
            assert!(result.contains("aggs_for_vecs.control"));
        }

        // CLEANUP TEST
        // Cleanup CoreDB
        coredbs.delete(name, &Default::default()).await.unwrap();
        println!("Waiting for CoreDB to be deleted: {}", &name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });
        println!("CoreDB resource deleted {}", name);

        // Delete namespace
        let _ = delete_namespace(client.clone(), &namespace).await;
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_ha_verify_extensions_ha_later() {
        // Initialize the Kubernetes client
        let client = kube_client().await;
        let state = State::default();
        let context = state.create_context(client.clone());

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let name = &format!("test-coredb-{}", suffix);
        let namespace = match create_namespace(client.clone(), name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };

        let kind = "CoreDB";
        let replicas = 1;

        // Create a pod we can use to run commands in the cluster
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);
        let lp =
            ListParams::default().labels(format!("app=postgres-exporter,coredb.io/name={}", name).as_str());

        // Apply a basic configuration of CoreDB
        println!("Creating CoreDB resource {}", name);
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        // Generate basic CoreDB resource to start with
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for CNPG Cluster to be created by looping over replicas until
        // they are in a running state
        for i in 1..=replicas {
            let pod_name = format!("{}-{}", name, i);
            pod_ready_and_running(pods.clone(), pod_name).await;
        }
        let exporter_pods = pods.list(&lp).await.expect("could not get pods");
        let exporter_pod_name = exporter_pods.items[0].metadata.name.as_ref().unwrap();
        pod_ready_and_running(pods.clone(), exporter_pod_name.clone()).await;

        let _result = wait_until_psql_contains(
            context.clone(),
            coredb_resource.clone(),
            "select extname from pg_catalog.pg_extension;".to_string(),
            "plpgsql".to_string(),
            false,
        )
        .await;

        // Add in an extension and lets make sure it's installed on all pods
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
                "trunk_installs": [
                    {
                        "name": "aggs_for_vecs",
                        "version": "1.3.0",
                    },
                ],
                "extensions": [
                    {
                        "name": "aggs_for_vecs",
                        "description": "aggs_for_vecs extension",
                        "locations": [{
                            "enabled": false,
                            "version": "1.3.0",
                            "database": "postgres",
                            "schema": "public"}
                        ]
                    }]
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait until the extension is installed
        wait_until_psql_contains(
            context.clone(),
            coredb_resource.clone(),
            "select extname from pg_catalog.pg_extension;".to_string(),
            "aggs_for_vecs".to_string(),
            true,
        )
        .await;

        // Assert that the extensions are installed on both replicas
        let retrieved_pods_result = coredb_resource.pods_by_cluster(client.clone()).await;

        let retrieved_pods = match retrieved_pods_result {
            Ok(pods_list) => pods_list,
            Err(e) => {
                panic!("Failed to retrieve pods: {:?}", e);
            }
        };
        for pod in &retrieved_pods {
            let cmd = vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "ls /var/lib/postgresql/data/tembo/extension/aggs_for_vecs.control".to_owned(),
            ];
            let pod_name = pod.metadata.name.clone().expect("Pod should have a name");
            let result =
                run_command_in_container(pods.clone(), pod_name, cmd.clone(), Some("postgres".to_string()))
                    .await;
            assert!(result.contains("aggs_for_vecs.control"));
        }

        // Now lets make the instance HA and ensure that all extenstions are present on both
        // replicas
        let replicas = 2;
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
                "trunk_installs": [
                    {
                        "name": "aggs_for_vecs",
                        "version": "1.3.0",
                    },
                ],
                "extensions": [
                    {
                        "name": "aggs_for_vecs",
                        "description": "aggs_for_vecs extension",
                        "locations": [{
                            "enabled": false,
                            "version": "1.3.0",
                            "database": "postgres",
                            "schema": "public"}
                        ]
                    }]
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for new replicas to be spun up before checking for extensions
        for i in 1..=replicas {
            let pod_name = format!("{}-{}", name, i);
            pod_ready_and_running(pods.clone(), pod_name).await;
        }

        // Wait until the extension is installed
        wait_until_psql_contains(
            context.clone(),
            coredb_resource.clone(),
            "select extname from pg_catalog.pg_extension;".to_string(),
            "aggs_for_vecs".to_string(),
            true,
        )
        .await;

        // Assert that the extensions are installed on both replicas
        let retrieved_pods_result = coredb_resource.pods_by_cluster(client.clone()).await;

        let retrieved_pods = match retrieved_pods_result {
            Ok(pods_list) => pods_list,
            Err(e) => {
                panic!("Failed to retrieve pods: {:?}", e);
            }
        };
        for pod in &retrieved_pods {
            let cmd = vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "ls /var/lib/postgresql/data/tembo/extension/aggs_for_vecs.control".to_owned(),
            ];
            let pod_name = pod.metadata.name.clone().expect("Pod should have a name");
            let result =
                run_command_in_container(pods.clone(), pod_name, cmd.clone(), Some("postgres".to_string()))
                    .await;
            assert!(result.contains("aggs_for_vecs.control"));
        }

        // CLEANUP TEST
        // Cleanup CoreDB
        coredbs.delete(name, &Default::default()).await.unwrap();
        println!("Waiting for CoreDB to be deleted: {}", &name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });
        println!("CoreDB resource deleted {}", name);

        // Delete namespace
        let _ = delete_namespace(client.clone(), &namespace).await;
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_ha_shared_preload_libraries() {
        // Initialize the Kubernetes client
        let client = kube_client().await;
        let state = State::default();
        let context = state.create_context(client.clone());

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let name = &format!("test-coredb-{}", suffix);
        let namespace = match create_namespace(client.clone(), name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };

        let kind = "CoreDB";
        let replicas = 1;

        // Create a pod we can use to run commands in the cluster
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);
        let lp =
            ListParams::default().labels(format!("app=postgres-exporter,coredb.io/name={}", name).as_str());

        // Apply a basic configuration of CoreDB
        println!("Creating CoreDB resource {}", name);
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        // Generate basic CoreDB resource to start with
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
                "trunk_installs": [
                    {
                        "name": "pg_partman",
                        "version": "4.7.3",
                    },
                    {
                        "name": "pgmq",
                        "version": "0.10.0",
                    },
                    {
                        "name": "pg_stat_statements",
                        "version": "1.10.0",
                    },
                ],
                "extensions": [
                    {
                        "name": "pg_partman",
                        "description": "pg_partman extension",
                        "locations": [{
                            "enabled": false,
                            "version": "4.7.3",
                            "database": "postgres",
                            "schema": "public"}
                        ]
                    },
                    {
                        "name": "pgmq",
                        "description": "pgmq extension",
                        "locations": [{
                            "enabled": false,
                            "version": "0.10.0",
                            "database": "postgres",
                            "schema": "public"}
                        ]
                    },
                    {
                        "name": "pg_stat_statements",
                        "description": "pg_stat_statements extension",
                        "locations": [{
                            "enabled": false,
                            "version": "1.10.0",
                            "database": "postgres",
                            "schema": "public"}
                        ]
                    },
                ]
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for CNPG Cluster to be created by looping over replicas until
        // they are in a running state
        for i in 1..=replicas {
            let pod_name = format!("{}-{}", name, i);
            pod_ready_and_running(pods.clone(), pod_name).await;
        }
        let exporter_pods = pods.list(&lp).await.expect("could not get pods");
        let exporter_pod_name = exporter_pods.items[0].metadata.name.as_ref().unwrap();
        pod_ready_and_running(pods.clone(), exporter_pod_name.clone()).await;

        // Assert that we can query the database with \dx;
        let result = psql_with_retry(context.clone(), coredb_resource.clone(), "\\dx".to_string()).await;

        let stdout = match result.stdout {
            Some(output) => output,
            None => panic!("stdout is None"),
        };

        assert!(stdout.contains("plpgsql"));

        // Wait until the extension is installed
        wait_until_psql_contains(
            context.clone(),
            coredb_resource.clone(),
            "select extname from pg_catalog.pg_extension;".to_string(),
            "pgmq".to_string(),
            true,
        )
        .await;

        // Assert that the extensions are installed on both replicas
        let retrieved_pods_result = coredb_resource.pods_by_cluster(client.clone()).await;

        let retrieved_pods = match retrieved_pods_result {
            Ok(pods_list) => pods_list,
            Err(e) => {
                panic!("Failed to retrieve pods: {:?}", e);
            }
        };

        for i in 1..=replicas {
            let pod_name = format!("{}-{}", name, i);
            pod_ready_and_running(pods.clone(), pod_name).await;
        }
        let exporter_pods = pods.list(&lp).await.expect("could not get pods");
        let exporter_pod_name = exporter_pods.items[0].metadata.name.as_ref().unwrap();
        pod_ready_and_running(pods.clone(), exporter_pod_name.clone()).await;

        for pod in &retrieved_pods {
            let cmd = vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "ls /var/lib/postgresql/data/tembo/extension/pgmq.control".to_owned(),
            ];
            let pod_name = pod.metadata.name.clone().expect("Pod should have a name");
            let result =
                run_command_in_container(pods.clone(), pod_name, cmd.clone(), Some("postgres".to_string()))
                    .await;
            println!("result: {}", result);
            assert!(result.contains("pgmq.control"));
        }

        // Now lets make the instance HA and ensure that all extenstions are present on both
        // replicas
        let replicas = 2;
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
                "trunk_installs": [
                    {
                        "name": "pg_partman",
                        "version": "4.7.3",
                    },
                    {
                        "name": "pgmq",
                        "version": "0.10.0",
                    },
                    {
                        "name": "pg_stat_statements",
                        "version": "1.10.0",
                    },
                ],
                "extensions": [
                    {
                        "name": "pg_partman",
                        "description": "pg_partman extension",
                        "locations": [{
                            "enabled": false,
                            "version": "4.7.3",
                            "database": "postgres",
                            "schema": "public"}
                        ]
                    },
                    {
                        "name": "pgmq",
                        "description": "pgmq extension",
                        "locations": [{
                            "enabled": false,
                            "version": "0.10.0",
                            "database": "postgres",
                            "schema": "public"}
                        ]
                    },
                    {
                        "name": "pg_stat_statements",
                        "description": "pg_stat_statements extension",
                        "locations": [{
                            "enabled": false,
                            "version": "1.10.0",
                            "database": "postgres",
                            "schema": "public"}
                        ]
                    },
                ]
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for new replicas to be spun up before checking for extensions
        for i in 1..=replicas {
            let pod_name = format!("{}-{}", name, i);
            pod_ready_and_running(pods.clone(), pod_name).await;
        }

        // Wait until the extension is installed
        wait_until_psql_contains(
            context.clone(),
            coredb_resource.clone(),
            "select extname from pg_catalog.pg_extension;".to_string(),
            "pgmq".to_string(),
            true,
        )
        .await;

        // Assert that the extensions are installed on both replicas
        let retrieved_pods_result = coredb_resource.pods_by_cluster(client.clone()).await;

        let retrieved_pods = match retrieved_pods_result {
            Ok(pods_list) => pods_list,
            Err(e) => {
                panic!("Failed to retrieve pods: {:?}", e);
            }
        };
        for pod in &retrieved_pods {
            let cmd = vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "ls /var/lib/postgresql/data/tembo/extension/pgmq.control".to_owned(),
            ];
            let pod_name = pod.metadata.name.clone().expect("Pod should have a name");
            let result =
                run_command_in_container(pods.clone(), pod_name, cmd.clone(), Some("postgres".to_string()))
                    .await;
            assert!(result.contains("pgmq.control"));
        }

        // CLEANUP TEST
        // Cleanup CoreDB
        coredbs.delete(name, &Default::default()).await.unwrap();
        println!("Waiting for CoreDB to be deleted: {}", &name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });
        println!("CoreDB resource deleted {}", name);

        // Delete namespace
        let _ = delete_namespace(client.clone(), &namespace).await;
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_app_service() {
        // Initialize the Kubernetes client
        let client = kube_client().await;

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let cdb_name = &format!("test-coredb-{}", suffix);
        let namespace = match create_namespace(client.clone(), cdb_name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };

        let kind = "CoreDB";

        // Apply a basic configuration of CoreDB
        println!("Creating CoreDB resource {}", cdb_name);
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        // generate an instance w/ 2 appServices
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": cdb_name
            },
            "spec": {
                "appServices": [
                    {
                        "name": "test-app-0",
                        "image": "crccheck/hello-world:latest",
                        "ports": [
                            "80:8000"
                        ],
                        "resources": {
                            "requests": {
                                "cpu": "100m",
                                "memory": "256Mi"
                            },
                            "limits": {
                                "cpu": "100m",
                                "memory": "256Mi"
                            }
                        }
                    },
                    {
                        "name": "test-app-1",
                        "image": "crccheck/hello-world:latest",
                        "resources": {
                            "requests": {
                                "cpu": "50m",
                                "memory": "128Mi"
                            },
                            "limits": {
                                "cpu": "50m",
                                "memory": "128Mi"
                            }
                        }
                    }
                ],
                "postgresExporterEnabled": false
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        coredbs.patch(cdb_name, &params, &patch).await.unwrap();

        // assert we created two Deployments, with the names we provided
        let deployment_items: Vec<Deployment> = list_resources(client.clone(), cdb_name, &namespace, 2)
            .await
            .unwrap();
        // two AppService deployments. the postgres exporter is disabled
        assert!(deployment_items.len() == 2);

        let service_items: Vec<Service> = list_resources(client.clone(), cdb_name, &namespace, 1)
            .await
            .unwrap();
        // one AppService Service, since only ports exposed on one
        assert!(service_items.len() == 1);

        let app_0 = deployment_items[0].clone();
        let app_1 = deployment_items[1].clone();
        assert_eq!(app_0.metadata.name.unwrap(), format!("{cdb_name}-test-app-0"));
        assert_eq!(app_1.metadata.name.unwrap(), format!("{cdb_name}-test-app-1"));

        // Assert resources in first appService
        // select the pod
        let selector_map = app_0
            .spec
            .as_ref()
            .and_then(|s| s.selector.match_labels.as_ref())
            .expect("Deployment should have a selector");
        let selector = selector_map
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join(",");
        let lp = ListParams::default().labels(&selector);
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);
        // Fetch and print all the pods matching the label selector
        let pod_list = pods.list(&lp).await.unwrap();
        assert_eq!(pod_list.items.len(), 1);
        let app_0_pod = pod_list.items[0].clone();
        let app_0_container = app_0_pod.spec.unwrap().containers[0].clone();

        let expected: ResourceRequirements = serde_json::from_value(serde_json::json!({
            "requests": {
                "cpu": "100m",
                "memory": "256Mi"
            },
            "limits": {
                "cpu": "100m",
                "memory": "256Mi"
            }
        }))
        .unwrap();
        let app_0_resources = app_0_container.resources.unwrap();
        assert_eq!(app_0_resources, expected);


        // Assert resources in second AppService
        let selector_map = app_1
            .spec
            .as_ref()
            .and_then(|s| s.selector.match_labels.as_ref())
            .expect("Deployment should have a selector");
        let selector = selector_map
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join(",");
        let lp = ListParams::default().labels(&selector);
        let pod_list = pods.list(&lp).await.unwrap();
        assert_eq!(pod_list.items.len(), 1);
        let app_1_pod = pod_list.items[0].clone();
        let app_1_container = app_1_pod.spec.unwrap().containers[0].clone();

        let expected: ResourceRequirements = serde_json::from_value(serde_json::json!({
            "requests": {
                "cpu": "50m",
                "memory": "128Mi"
            },
            "limits": {
                "cpu": "50m",
                "memory": "128Mi"
            }
        }))
        .unwrap();
        let app_1_resources = app_1_container.resources.unwrap();
        assert_eq!(app_1_resources, expected);

        // Delete the one without a service
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": cdb_name
            },
            "spec": {
                "appServices": [
                    {
                        "name": "test-app-0",
                        "image": "crccheck/hello-world:latest",
                        "ports": [
                            "80:8000"
                        ],
                        "resources": {
                            "requests": {
                                "cpu": "100m",
                                "memory": "256Mi"
                            },
                            "limits": {
                                "cpu": "100m",
                                "memory": "256Mi"
                            }
                        }
                    },
                ],
                "postgresExporterEnabled": false
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        coredbs.patch(cdb_name, &params, &patch).await.unwrap();

        let deployment_items: Vec<Deployment> = list_resources(client.clone(), cdb_name, &namespace, 1)
            .await
            .unwrap();
        assert!(deployment_items.len() == 1);
        let app_0 = deployment_items[0].clone();
        assert_eq!(app_0.metadata.name.unwrap(), format!("{cdb_name}-test-app-0"));

        // should still be just one Service
        let service_items: Vec<Service> = list_resources(client.clone(), cdb_name, &namespace, 1)
            .await
            .unwrap();
        // One appService Services
        assert!(service_items.len() == 1);

        // Delete all of them
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": cdb_name
            },
            "spec": {
                "postgresExporterEnabled": false
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        coredbs.patch(cdb_name, &params, &patch).await.unwrap();
        let deployment_items: Vec<Deployment> = list_resources(client.clone(), cdb_name, &namespace, 0)
            .await
            .unwrap();
        assert!(deployment_items.is_empty());

        let service_items: Vec<Service> = list_resources(client.clone(), cdb_name, &namespace, 0)
            .await
            .unwrap();
        assert!(service_items.is_empty());
        // should be no Services

        // CLEANUP TEST
        // Cleanup CoreDB
        coredbs.delete(cdb_name, &Default::default()).await.unwrap();
        println!("Waiting for CoreDB to be deleted: {}", &cdb_name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), cdb_name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                cdb_name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });
        println!("CoreDB resource deleted {}", cdb_name);

        // Delete namespace
        let _ = delete_namespace(client.clone(), &namespace).await;
    }

    #[tokio::test]
    #[ignore]
    async fn restarts_postgres_correctly() {
        async fn wait_til_status_is_filled(coredbs: &Api<CoreDB>, name: &str) {
            let started_waiting = Utc::now();
            let max_wait_time = chrono::Duration::seconds(45);

            while Utc::now().signed_duration_since(started_waiting) <= max_wait_time {
                let coredb = coredbs.get(name).await.expect("spec not found");
                if coredb.status.is_some() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }

            panic!("Status was not populated fast enough");
        }

        async fn get_pg_start_time(coredbs: &Api<CoreDB>, name: &str, ctx: Arc<Context>) -> DateTime<Utc> {
            const PG_TIMESTAMP_DECL: &str = "%Y-%m-%d %H:%M:%S.%f%#z";

            let coredb = coredbs.get(name).await.expect("spec not found");

            let psql_output = coredb
                .psql(
                    "SELECT pg_postmaster_start_time()".into(),
                    "postgres".to_string(),
                    ctx,
                )
                .await
                .map_err(|err| eprintln!("Error: {:?}", err))
                .unwrap();
            let stdout = psql_output
                .stdout
                .as_ref()
                .and_then(|stdout| stdout.lines().nth(2).map(str::trim))
                .expect("expected stdout");

            DateTime::parse_from_str(stdout, PG_TIMESTAMP_DECL)
                .unwrap()
                .into()
        }

        async fn status_running(coredbs: &Api<CoreDB>, name: &str) -> bool {
            let spec = coredbs.get(name).await.expect("spec not found");

            spec.status.expect("Expected status to be present").running
        }

        // Initialize tracing
        tracing_subscriber::fmt().init();

        // Initialize the Kubernetes client
        let client = kube_client().await;
        let state = State::default();
        let context = state.create_context(client.clone());

        let name = {
            let mut rng = rand::thread_rng();
            let suffix = rng.gen_range(0..100000);

            format!("test-coredb-{}", suffix)
        };

        let namespace = create_namespace(client.clone(), &name).await.unwrap();

        // Apply a basic configuration of CoreDB
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);

        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": "CoreDB",
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": 1,
                "extensions": [],
                "trunk_installs": []
            }
        });

        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        coredbs.patch(&name, &params, &patch).await.unwrap();

        // Wait for CNPG Pod to be created
        {
            let pod_name = format!("{}-1", name);

            pod_ready_and_running(pods.clone(), pod_name.clone()).await;
            wait_til_status_is_filled(&coredbs, &name).await;
        }

        // Ensure status.running is true
        assert!(status_running(&coredbs, &name).await);
        let initial_start_time = get_pg_start_time(&coredbs, &name, context.clone()).await;

        // Apply the annotation to restart Postgres
        {
            let restart = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true).to_string();

            let patch_json = serde_json::json!({
                "metadata": {
                    "annotations": {
                        "kubectl.kubernetes.io/restartedAt": restart
                    }
                }
            });

            let patch = Patch::Merge(&patch_json);
            coredbs
                .patch(&name, &PatchParams::default(), &patch)
                .await
                .unwrap();
        }

        // Ensure that eventually `status.running` becomes false to reflect
        // that Postgres is down
        {
            let started = Utc::now();
            let max_wait_time = chrono::Duration::seconds(30);
            let mut running_became_false = false;
            while Utc::now().signed_duration_since(started) < max_wait_time {
                if status_running(&coredbs, &name).await {
                    println!("status.running is still true. Retrying in 1 sec.");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                } else {
                    println!("status.running is now false!");

                    running_became_false = true;
                    break;
                }
            }

            assert!(
                running_became_false,
                "status.running should've become false after restart"
            );
        }

        // Wait for Postgres to restart
        {
            let started = Utc::now();
            let max_wait_time = chrono::Duration::seconds(TIMEOUT_SECONDS_POD_READY as _);
            let mut running_became_true = false;
            while Utc::now().signed_duration_since(started) < max_wait_time {
                if status_running(&coredbs, &name).await.not() {
                    println!("status.running is still false. Retrying in 3 secs.");
                    tokio::time::sleep(Duration::from_secs(3)).await;
                } else {
                    println!("status.running is now true once again!");

                    running_became_true = true;
                    break;
                }
            }

            assert!(
                running_became_true,
                "status.running should've become true once restarted"
            );

            let reboot_start_time = get_pg_start_time(&coredbs, &name, context).await;


            assert!(
                reboot_start_time > initial_start_time,
                "start time should've changed"
            );
        }

        // Perform cleanup
        {
            coredbs.delete(&name, &Default::default()).await.unwrap();
            println!("Waiting for CoreDB to be deleted: {name}");
            let _assert_coredb_deleted = tokio::time::timeout(
                Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
                await_condition(coredbs.clone(), &name, conditions::is_deleted("")),
            )
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "CoreDB {} was not deleted after waiting {} seconds",
                    name, TIMEOUT_SECONDS_COREDB_DELETED
                )
            });
            println!("CoreDB resource deleted {name}");

            // Delete namespace
            let _ = delete_namespace(client.clone(), &namespace).await;
        }
    }

    #[tokio::test]
    #[ignore]
    async fn functional_test_status_configs() {
        async fn runtime_cfg(coredbs: &Api<CoreDB>, name: &str) -> Option<Vec<PgConfig>> {
            let spec = coredbs.get(name).await.expect("spec not found");
            spec.status.expect("Expected status to be present").runtime_config
        }

        // Initialize the Kubernetes client
        let client = kube_client().await;
        let state = State::default();
        let _context = state.create_context(client.clone());

        // Configurations
        let mut rng = rand::thread_rng();
        let suffix = rng.gen_range(0..100000);
        let name = &format!("test-coredb-{}", suffix);
        let namespace = match create_namespace(client.clone(), name).await {
            Ok(namespace) => namespace,
            Err(e) => {
                eprintln!("Error creating namespace: {}", e);
                std::process::exit(1);
            }
        };

        let kind = "CoreDB";
        let replicas = 1;

        // Create a pod we can use to run commands in the cluster
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);

        // Apply a basic configuration of CoreDB
        println!("Creating CoreDB resource {}", name);
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &namespace);
        // Generate basic CoreDB resource to start with
        let coredb_json = serde_json::json!({
            "apiVersion": API_VERSION,
            "kind": kind,
            "metadata": {
                "name": name
            },
            "spec": {
                "replicas": replicas,
                "trunk_installs": [
                    {
                        "name": "pg_partman",
                        "version": "4.7.3",
                    },
                    {
                        "name": "pgmq",
                        "version": "0.10.0",
                    },
                    {
                        "name": "pg_stat_statements",
                        "version": "1.10.0",
                    },
                ],
                "extensions": [
                    {
                        "name": "pg_partman",
                        "locations": [
                        {
                          "enabled": true,
                          "version": "4.7.3",
                          "database": "postgres",
                          "schema": "public"
                        }]
                    },
                    {
                        "name": "pgmq",
                        "locations": [
                        {
                          "enabled": true,
                          "version": "0.10.0",
                          "database": "postgres",
                          "schema": "public"
                        }]
                    },
                    {
                        "name": "pg_stat_statements",
                        "locations": [
                        {
                          "enabled": true,
                          "version": "1.10.0",
                          "database": "postgres",
                          "schema": "public"
                        }]
                    }
                ],
                "runtime_config": [
                    {
                        "name": "shared_preload_libraries",
                        "value": "pg_stat_statements,pg_partman_bgw"
                    }
                ]
            }
        });
        let params = PatchParams::apply("tembo-integration-test");
        let patch = Patch::Apply(&coredb_json);
        let _coredb_resource = coredbs.patch(name, &params, &patch).await.unwrap();

        // Wait for CNPG Pod to be created
        let pod_name = format!("{}-1", name);

        pod_ready_and_running(pods.clone(), pod_name.clone()).await;

        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);
        let lp =
            ListParams::default().labels(format!("app=postgres-exporter,coredb.io/name={}", name).as_str());
        let exporter_pods = pods.list(&lp).await.expect("could not get pods");
        let exporter_pod_name = exporter_pods.items[0].metadata.name.as_ref().unwrap();
        println!("Exporter pod name: {}", &exporter_pod_name);

        pod_ready_and_running(pods.clone(), exporter_pod_name.clone()).await;

        // Assert status contains configs
        let mut found_configs = false;
        let expected_config = ConfigValue::Multiple(BTreeSet::from_iter(vec![
            "pg_stat_statements".to_string(),
            "pg_partman_bgw".to_string(),
        ]));

        // Wait for status.runtime_config to contain expected_config
        while !found_configs {
            let runtime_config = runtime_cfg(&coredbs, &name).await;
            if runtime_config.is_some() {
                let runtime_config = runtime_config.unwrap();
                for config in runtime_config {
                    if config.name == "shared_preload_libraries" {
                        if config.value == expected_config {
                            found_configs = true;
                        }
                    }
                }
            }
            println!("Waiting for runtime_config to be populated with expected values");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }


        // Assert status.runtime_config length is greater than 350. It should be around 362, but
        // that will fluctuate between postgres versions. This is a sanity check to ensure that
        // the runtime_config is being populated with all config values.
        let runtime_cfg = runtime_cfg(&coredbs, &name).await.unwrap();
        assert!(runtime_cfg.len() > 350);
        println!(
            "Found {} runtime_config values",
            runtime_cfg.len()
        );

        // Assert status.runtime_config
        assert!(found_configs);

        // CLEANUP TEST
        // Cleanup CoreDB
        coredbs.delete(name, &Default::default()).await.unwrap();
        println!("Waiting for CoreDB to be deleted: {}", &name);
        let _assert_coredb_deleted = tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECONDS_COREDB_DELETED),
            await_condition(coredbs.clone(), name, conditions::is_deleted("")),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "CoreDB {} was not deleted after waiting {} seconds",
                name, TIMEOUT_SECONDS_COREDB_DELETED
            )
        });
        println!("CoreDB resource deleted {}", name);

        // Delete namespace
        let _ = delete_namespace(client.clone(), &namespace).await;
    }
}
