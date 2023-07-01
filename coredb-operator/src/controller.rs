use crate::{telemetry, Error, Metrics, Result};
use chrono::{DateTime, Utc};
use futures::{
    future::{BoxFuture, FutureExt},
    stream::StreamExt,
};

use crate::{
    apis::{
        coredb_types::{CoreDB, CoreDBStatus},
        postgres_parameters::reconcile_pg_parameters_configmap,
    },
    cloudnativepg::cnpg::{cnpg_cluster_from_cdb, reconcile_cnpg},
    config::Config,
    cronjob::reconcile_cronjob,
    deployment_postgres_exporter::reconcile_prometheus_exporter,
    exec::{ExecCommand, ExecOutput},
    extensions::{reconcile_extensions, Extension},
    ingress::reconcile_postgres_ing_route_tcp,
    postgres_exporter::{create_postgres_exporter_role, reconcile_prom_configmap},
    psql::{PsqlCommand, PsqlOutput},
    rbac::reconcile_rbac,
    secret::{reconcile_postgres_exporter_secret, reconcile_secret, PrometheusExporterSecretData},
    service::reconcile_svc,
    statefulset::{reconcile_sts, stateful_set_from_cdb},
};
use k8s_openapi::{
    api::{
        core::v1::{Namespace, Pod},
        rbac::v1::PolicyRule,
    },
    apimachinery::pkg::util::intstr::IntOrString,
};
use kube::{
    api::{Api, ListParams, Patch, PatchParams, ResourceExt},
    client::Client,
    runtime::{
        controller::{Action, Controller},
        events::{Event, EventType, Recorder, Reporter},
        finalizer::{finalizer, Event as Finalizer},
        wait::Condition,
    },
    Resource,
};
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use tokio::{sync::RwLock, time::Duration};
use tracing::*;

pub static COREDB_FINALIZER: &str = "coredbs.coredb.io";
pub static COREDB_ANNOTATION: &str = "coredbs.coredb.io/watch";

// Context for our reconciler
#[derive(Clone)]
pub struct Context {
    /// Kubernetes client
    pub client: Client,
    /// Diagnostics read by the web server
    pub diagnostics: Arc<RwLock<Diagnostics>>,
    /// Prometheus metrics
    pub metrics: Metrics,
}

#[instrument(skip(ctx, cdb), fields(trace_id))]
async fn reconcile(cdb: Arc<CoreDB>, ctx: Arc<Context>) -> Result<Action> {
    let cfg = Config::default();
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", &field::display(&trace_id));
    let _timer = ctx.metrics.count_and_measure();
    ctx.diagnostics.write().await.last_event = Utc::now();
    let ns = cdb.namespace().unwrap(); // cdb is namespace scoped
    let coredbs: Api<CoreDB> = Api::namespaced(ctx.client.clone(), &ns);
    // Get metadata for the CoreDB object
    let metadata = cdb.meta().clone();
    // Get annotations from the metadata
    let annotations = metadata.annotations.clone().unwrap_or_default();

    // Check the annotations to see if it exists and check it's value
    if let Some(value) = annotations.get(COREDB_ANNOTATION) {
        // If the value is false, then we should skip reconciling
        if value == "false" {
            info!(
                "Skipping reconciliation for CoreDB \"{}\" in {}",
                cdb.name_any(),
                ns
            );
            return Ok(Action::await_change());
        }
    }

    info!("Reconciling CoreDB \"{}\" in {}", cdb.name_any(), ns);
    finalizer(&coredbs, COREDB_FINALIZER, cdb, |event| async {
        match event {
            Finalizer::Apply(cdb) => match cdb.reconcile(ctx.clone(), &cfg).await {
                Ok(action) => Ok(action),
                Err(requeue_action) => Ok(requeue_action),
            },
            Finalizer::Cleanup(cdb) => cdb.cleanup(ctx.clone()).await,
        }
    })
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

fn error_policy(cdb: Arc<CoreDB>, error: &Error, ctx: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    ctx.metrics.reconcile_failure(&cdb, error);
    Action::requeue(Duration::from_secs(5 * 60))
}

// Create role policy rulesets
async fn create_policy_rules(cdb: &CoreDB) -> Vec<PolicyRule> {
    vec![
        // This policy allows get, list, watch access to the coredb resource
        PolicyRule {
            api_groups: Some(vec!["coredb.io".to_owned()]),
            resource_names: Some(vec![cdb.name_any()]),
            resources: Some(vec!["coredbs".to_owned()]),
            verbs: vec!["get".to_string(), "list".to_string(), "watch".to_string()],
            ..PolicyRule::default()
        },
        // This policy allows get, patch, update, watch access to the coredb/status resource
        PolicyRule {
            api_groups: Some(vec!["coredb.io".to_owned()]),
            resource_names: Some(vec![cdb.name_any()]),
            resources: Some(vec!["coredbs/status".to_owned()]),
            verbs: vec![
                "get".to_string(),
                "patch".to_string(),
                "update".to_string(),
                "watch".to_string(),
            ],
            ..PolicyRule::default()
        },
        // This policy allows get, watch access to a secret in the namespace
        PolicyRule {
            api_groups: Some(vec!["".to_owned()]),
            resource_names: Some(vec![format!("{}-connection", cdb.name_any())]),
            resources: Some(vec!["secrets".to_owned()]),
            verbs: vec!["get".to_string(), "watch".to_string()],
            ..PolicyRule::default()
        },
        // This policy for now is specifically open for all configmaps in the namespace
        // We currently do not have any configmaps
        PolicyRule {
            api_groups: Some(vec!["".to_owned()]),
            resources: Some(vec!["configmaps".to_owned()]),
            verbs: vec!["get".to_string(), "watch".to_string()],
            ..PolicyRule::default()
        },
    ]
}

impl CoreDB {
    async fn cnpg_enabled(&self, ctx: Arc<Context>) -> bool {
        // We will migrate databases by applying this label manually to the namespace
        let cnpg_enabled_label = "tembo-pod-init.tembo.io/watch";

        let client = ctx.client.clone();
        // Get labels of the current namespace
        let ns_api: Api<Namespace> = Api::all(client.clone());
        let ns = self.namespace().unwrap();
        let ns_labels = ns_api
            .get(&ns)
            .await
            .unwrap_or_default()
            .metadata
            .labels
            .unwrap_or_default();
        dbg!(ns_labels.clone());

        let enabled_value = ns_labels.get(&String::from(cnpg_enabled_label));
        if enabled_value.is_some() {
            let enabled = enabled_value.expect("We already checked this is_some") == "true";
            return enabled;
        }
        false
    }

    // Reconcile (for non-finalizer related changes)
    async fn reconcile(&self, ctx: Arc<Context>, cfg: &Config) -> Result<Action, Action> {
        let client = ctx.client.clone();
        let _recorder = ctx.diagnostics.read().await.recorder(client.clone(), self);
        let ns = self.namespace().unwrap();
        let name = self.name_any();
        let coredbs: Api<CoreDB> = Api::namespaced(client.clone(), &ns);

        let cnpg_enabled = self.cnpg_enabled(ctx.clone()).await;
        match std::env::var("DATA_PLANE_BASEDOMAIN") {
            Ok(basedomain) => {
                debug!(
                    "DATA_PLANE_BASEDOMAIN is set to {}, reconciling ingress route tcp",
                    basedomain
                );
                let service_name_read_write = match cnpg_enabled {
                    // When CNPG is enabled, we use the CNPG service name
                    true => format!("{}-rw", self.name_any().as_str()),
                    false => self.name_any().as_str().to_string(),
                };
                reconcile_postgres_ing_route_tcp(
                    self,
                    ctx.clone(),
                    self.name_any().as_str(),
                    basedomain.as_str(),
                    ns.as_str(),
                    service_name_read_write.as_str(),
                    IntOrString::Int(5432),
                )
                .await
                .map_err(|e| {
                    error!("Error reconciling postgres ingress route: {:?}", e);
                    // For unexpected errors, we should requeue for several minutes at least,
                    // for expected, "waiting" type of requeuing, those should be shorter, just a few seconds.
                    // IngressRouteTCP does not have expected errors during reconciliation.
                    Action::requeue(Duration::from_secs(300))
                })?;
            }
            Err(_e) => {
                warn!("DATA_PLANE_BASEDOMAIN is not set, skipping reconciliation of IngressRouteTCP");
            }
        };

        // create/update configmap when postgres exporter enabled
        if self.spec.postgresExporterEnabled {
            debug!("Reconciling prometheus configmap");
            reconcile_prom_configmap(self, client.clone(), &ns)
                .await
                .map_err(|e| {
                    error!("Error reconciling prometheus configmap: {:?}", e);
                    Action::requeue(Duration::from_secs(300))
                })?;
        }

        // reconcile service account, role, and role binding
        reconcile_rbac(self, ctx.clone(), None, create_policy_rules(self).await)
            .await
            .map_err(|e| {
                error!("Error reconciling service account: {:?}", e);
                Action::requeue(Duration::from_secs(300))
            })?;

        // reconcile secret
        debug!("Reconciling secret");
        reconcile_secret(self, ctx.clone()).await.map_err(|e| {
            error!("Error reconciling secret: {:?}", e);
            Action::requeue(Duration::from_secs(300))
        })?;

        // reconcile postgres exporter secret
        let secret_data: Option<PrometheusExporterSecretData> = if self.spec.postgresExporterEnabled {
            let result = reconcile_postgres_exporter_secret(self, ctx.clone())
                .await
                .map_err(|e| {
                    error!("Error reconciling postgres exporter secret: {:?}", e);
                    Action::requeue(Duration::from_secs(300))
                })?;

            match result {
                Some(data) => Some(data),
                None => {
                    warn!("Secret already exists, no new password is generated");
                    None
                }
            }
        } else {
            None
        };

        // reconcile cronjob for backups
        debug!("Reconciling cronjob");
        reconcile_cronjob(self, ctx.clone()).await.map_err(|e| {
            error!("Error reconciling cronjob: {:?}", e);
            Action::requeue(Duration::from_secs(300))
        })?;

        // handle postgres configs
        debug!("Reconciling postgres configmap");
        reconcile_pg_parameters_configmap(self, client.clone(), &ns)
            .await
            .map_err(|e| {
                error!("Error reconciling postgres configmap: {:?}", e);
                Action::requeue(Duration::from_secs(300))
            })?;

        // reconcile statefulset
        debug!("Reconciling statefulset");
        reconcile_sts(self, ctx.clone()).await.map_err(|e| {
            error!("Error reconciling statefulset: {:?}", e);
            Action::requeue(Duration::from_secs(300))
        })?;

        // reconcile prometheus exporter deployment if enabled
        if self.spec.postgresExporterEnabled {
            debug!("Reconciling prometheus exporter deployment");
            reconcile_prometheus_exporter(self, ctx.clone(), cnpg_enabled)
                .await
                .map_err(|e| {
                    error!("Error reconciling prometheus exporter deployment: {:?}", e);
                    Action::requeue(Duration::from_secs(300))
                })?;
        };

        // reconcile service
        debug!("Reconciling service");
        reconcile_svc(self, ctx.clone()).await.map_err(|e| {
            error!("Error reconciling service: {:?}", e);
            Action::requeue(Duration::from_secs(300))
        })?;

        let new_status = match self.spec.stop {
            false => {
                let primary_pod_coredb = self.primary_pod_coredb(ctx.client.clone()).await;
                if primary_pod_coredb.is_err() {
                    info!(
                        "Did not find primary pod of {}, waiting a short period",
                        self.name_any()
                    );
                    return Ok(Action::requeue(Duration::from_secs(1)));
                }
                let primary_pod_coredb = primary_pod_coredb.unwrap();

                if !is_postgres_ready().matches_object(Some(&primary_pod_coredb)) {
                    info!(
                        "Did not find postgres ready {}, waiting a short period",
                        self.name_any()
                    );
                    return Ok(Action::requeue(Duration::from_secs(1)));
                }

                if cnpg_enabled {
                    reconcile_cnpg(self, ctx.clone()).await.map_err(|e| {
                        error!("Error reconciling CNPG: {:?}", e);
                        Action::requeue(Duration::from_secs(300))
                    })?;

                    let primary_pod_cnpg = self.primary_pod_cnpg(ctx.client.clone()).await;
                    if primary_pod_cnpg.is_err() {
                        info!(
                            "Did not find primary pod of CNPG for {}, waiting a short period",
                            self.name_any()
                        );
                        return Ok(Action::requeue(Duration::from_secs(1)));
                    }
                    let primary_pod_cnpg = primary_pod_cnpg.unwrap();

                    if !is_postgres_ready().matches_object(Some(&primary_pod_cnpg)) {
                        info!(
                            "Did not find CNPG postgres pod ready for {}, waiting a short period",
                            self.name_any()
                        );
                        return Ok(Action::requeue(Duration::from_secs(1)));
                    }
                }

                // TODO: before merge (bug) on fresh DB, this cannot run because postgres not ready on CNPG
                // and primary of coredb also not ready
                // creating exporter role is pre-requisite to the postgres pod becoming "ready"
                if self.spec.postgresExporterEnabled {
                    create_postgres_exporter_role(self, ctx.clone(), secret_data)
                        .await
                        .map_err(|e| {
                            error!(
                                "Error creating postgres_exporter on CoreDB {}, {}",
                                self.metadata.name.clone().unwrap(),
                                e
                            );
                            Action::requeue(Duration::from_secs(300))
                        })?;
                }

                // This step is applicable to coredb but not cnpg
                if !is_pod_ready().matches_object(Some(&primary_pod_coredb)) {
                    info!(
                        "Did not find pod ready {}, waiting a short period",
                        self.name_any()
                    );
                    return Ok(Action::requeue(Duration::from_secs(1)));
                }

                let extensions: Vec<Extension> = reconcile_extensions(self, ctx.clone(), &coredbs, &name)
                    .await
                    .map_err(|e| {
                        error!("Error reconciling extensions: {:?}", e);
                        Action::requeue(Duration::from_secs(300))
                    })?;

                // Check cfg.enable_initial_backup to make sure we should run the initial backup
                // if it's true, run the backup
                if cfg.enable_initial_backup {
                    let backup_command = vec![
                        "/bin/sh".to_string(),
                        "-c".to_string(),
                        "/usr/bin/wal-g backup-push /var/lib/postgresql/data --full --verify".to_string(),
                    ];

                    let _backup_result = self
                        .exec(primary_pod_coredb.name_any(), client, &backup_command)
                        .await
                        .map_err(|e| {
                            error!("Error running backup: {:?}", e);
                            Action::requeue(Duration::from_secs(300))
                        })?;
                }

                CoreDBStatus {
                    running: true,
                    extensionsUpdating: false,
                    storage: self.spec.storage.clone(),
                    sharedirStorage: self.spec.sharedirStorage.clone(),
                    pkglibdirStorage: self.spec.pkglibdirStorage.clone(),
                    extensions: Some(extensions),
                }
            }
            true => CoreDBStatus {
                running: false,
                extensionsUpdating: false,
                storage: self.spec.storage.clone(),
                sharedirStorage: self.spec.sharedirStorage.clone(),
                pkglibdirStorage: self.spec.pkglibdirStorage.clone(),
                extensions: self.status.clone().and_then(|f| f.extensions),
            },
        };

        let patch_status = json!({
            "apiVersion": "coredb.io/v1alpha1",
            "kind": "CoreDB",
            "status": new_status
        });

        patch_cdb_status_force(&coredbs, &name, patch_status).await?;

        // Check back every 5 minutes
        Ok(Action::requeue(Duration::from_secs(300)))
    }

    // Finalizer cleanup (the object was deleted, ensure nothing is orphaned)
    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
        // If namespace is terminating, do not publish delete event. Attempting to publish an event
        // in a terminating namespace will leave us in a bad state in which the namespace will hang
        // in terminating state.
        let ns_api: Api<Namespace> = Api::all(ctx.client.clone());
        let ns_status = ns_api
            .get_status(self.metadata.namespace.as_ref().unwrap())
            .await
            .map_err(Error::KubeError);
        let phase = ns_status.unwrap().status.unwrap().phase;
        if phase == Some("Terminating".to_string()) {
            return Ok(Action::await_change());
        }
        let recorder = ctx.diagnostics.read().await.recorder(ctx.client.clone(), self);
        // CoreDB doesn't have dependencies in this example case, so we just publish an event
        recorder
            .publish(Event {
                type_: EventType::Normal,
                reason: "DeleteCoreDB".into(),
                note: Some(format!("Delete `{}`", self.name_any())),
                action: "Reconciling".into(),
                secondary: None,
            })
            .await
            .map_err(Error::KubeError)?;
        Ok(Action::await_change())
    }

    pub async fn primary_pod_coredb(&self, client: Client) -> Result<Pod, Error> {
        let sts = stateful_set_from_cdb(self);
        let sts_name = sts.metadata.name.unwrap();
        let sts_namespace = sts.metadata.namespace.unwrap();
        let label_selector = format!("statefulset={sts_name}");
        let list_params = ListParams::default().labels(&label_selector);
        let pods: Api<Pod> = Api::namespaced(client, &sts_namespace);
        let pods = pods.list(&list_params);
        // Return an error if the query fails
        let pod_list = pods.await.map_err(Error::KubeError)?;
        // Return an error if the list is empty
        if pod_list.items.is_empty() {
            return Err(Error::KubeError(kube::Error::Api(kube::error::ErrorResponse {
                status: "404".to_string(),
                message: "No pods found".to_string(),
                reason: "Not Found".to_string(),
                code: 404,
            })));
        }
        let primary = pod_list.items[0].clone();
        Ok(primary)
    }

    pub async fn primary_pod_cnpg(&self, client: Client) -> Result<Pod, Error> {
        let cluster = cnpg_cluster_from_cdb(self);
        let cluster_name = cluster
            .metadata
            .name
            .expect("CNPG Cluster should always have a name");
        let namespace = self
            .metadata
            .namespace
            .clone()
            .expect("Operator should always be namespaced");
        let cluster_selector = format!("cnpg.io/cluster={cluster_name}");
        let role_selector = "role=primary".to_string();
        let list_params = ListParams::default()
            .labels(&cluster_selector)
            .labels(&role_selector);
        let pods: Api<Pod> = Api::namespaced(client, &namespace);
        let pods = pods.list(&list_params);
        // Return an error if the query fails
        let pod_list = pods.await.map_err(Error::KubeError)?;
        // Return an error if the list is empty
        if pod_list.items.is_empty() {
            return Err(Error::KubeError(kube::Error::Api(kube::error::ErrorResponse {
                status: "404".to_string(),
                message: "Did not find CNPG primary".to_string(),
                reason: "Selecting using the labels 'cnpg.io/cluster' and 'role' did not find any pods. This can happen when there is a pod moving, switching over, or a new cluster.".to_string(),
                code: 404,
            })));
        }
        let primary = pod_list.items[0].clone();
        Ok(primary)
    }

    pub async fn psql(
        &self,
        command: String,
        database: String,
        context: Arc<Context>,
    ) -> Result<PsqlOutput, kube::Error> {
        let client = context.client.clone();

        let pod_name_coredb = self
            .primary_pod_coredb(client.clone())
            .await
            .unwrap()
            .metadata
            .name
            .expect("All pods should have a name");

        let coredb_psql_command = PsqlCommand::new(
            pod_name_coredb,
            self.metadata.namespace.clone().unwrap(),
            command.clone(),
            database.clone(),
            context.clone(),
        );
        let coredb_exec = coredb_psql_command.execute();

        if self.cnpg_enabled(context.clone()).await {
            let pod_name_cnpg = self
                .primary_pod_cnpg(client.clone())
                .await
                .unwrap()
                .metadata
                .name
                .expect("All pods should have a name");

            let cnpg_psql_command = PsqlCommand::new(
                pod_name_cnpg,
                self.metadata.namespace.clone().unwrap(),
                command,
                database,
                context,
            );
            let cnpg_exec = cnpg_psql_command.execute();

            let _coredb_output = coredb_exec.await?;
            return cnpg_exec.await;
        }

        coredb_exec.await
    }

    pub async fn exec(
        &self,
        pod_name: String,
        client: Client,
        command: &[String],
    ) -> Result<ExecOutput, Error> {
        ExecCommand::new(pod_name, self.metadata.namespace.clone().unwrap(), client)
            .execute(command)
            .await
    }
}

pub fn is_pod_ready() -> impl Condition<Pod> + 'static {
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

pub fn is_postgres_ready() -> impl Condition<Pod> + 'static {
    move |obj: Option<&Pod>| {
        if let Some(pod) = &obj {
            if let Some(status) = &pod.status {
                if let Some(container_statuses) = &status.container_statuses {
                    for container in container_statuses {
                        if container.name == "postgres" {
                            return container.ready;
                        }
                    }
                }
            }
        }
        false
    }
}

pub async fn patch_cdb_status_force(
    cdb: &Api<CoreDB>,
    name: &str,
    patch: serde_json::Value,
) -> Result<(), Action> {
    let ps = PatchParams::apply("cntrlr").force();
    let patch_status = Patch::Apply(patch);
    let _o = cdb.patch_status(name, &ps, &patch_status).await.map_err(|e| {
        error!("Error updating CoreDB status: {:?}", e);
        Action::requeue(Duration::from_secs(10))
    })?;
    Ok(())
}

pub async fn patch_cdb_status_merge(
    cdb: &Api<CoreDB>,
    name: &str,
    patch: serde_json::Value,
) -> Result<(), Action> {
    let pp = PatchParams::default();
    let patch_status = Patch::Merge(patch);
    let _o = cdb.patch_status(name, &pp, &patch_status).await.map_err(|e| {
        error!("Error updating CoreDB status: {:?}", e);
        Action::requeue(Duration::from_secs(10))
    })?;
    Ok(())
}

/// Diagnostics to be exposed by the web server
#[derive(Clone, Serialize)]
pub struct Diagnostics {
    #[serde(deserialize_with = "from_ts")]
    pub last_event: DateTime<Utc>,
    #[serde(skip)]
    pub reporter: Reporter,
}
impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            last_event: Utc::now(),
            reporter: "coredb-controller".into(),
        }
    }
}
impl Diagnostics {
    fn recorder(&self, client: Client, cdb: &CoreDB) -> Recorder {
        Recorder::new(client, self.reporter.clone(), cdb.object_ref(&()))
    }
}

/// State shared between the controller and the web server
#[derive(Clone, Default)]
pub struct State {
    /// Diagnostics populated by the reconciler
    diagnostics: Arc<RwLock<Diagnostics>>,
    /// Metrics registry
    registry: prometheus::Registry,
}

/// State wrapper around the controller outputs for the web server
impl State {
    /// Metrics getter
    pub fn metrics(&self) -> Vec<prometheus::proto::MetricFamily> {
        self.registry.gather()
    }

    /// State getter
    pub async fn diagnostics(&self) -> Diagnostics {
        self.diagnostics.read().await.clone()
    }

    // Create a Controller Context that can update State
    pub fn create_context(&self, client: Client) -> Arc<Context> {
        Arc::new(Context {
            client,
            metrics: Metrics::default().register(&self.registry).unwrap(),
            diagnostics: self.diagnostics.clone(),
        })
    }
}

/// Initialize the controller and shared state (given the crd is installed)
pub async fn init(client: Client) -> (BoxFuture<'static, ()>, State) {
    let state = State::default();
    let cdb = Api::<CoreDB>::all(client.clone());
    if let Err(e) = cdb.list(&ListParams::default().limit(1)).await {
        error!("CRD is not queryable; {e:?}. Is the CRD installed?");
        info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
        std::process::exit(1);
    }
    let controller = Controller::new(cdb, ListParams::default())
        .shutdown_on_signal()
        .run(reconcile, error_policy, state.create_context(client))
        .filter_map(|x| async move { Result::ok(x) })
        .for_each(|_| futures::future::ready(()))
        .boxed();
    (controller, state)
}

// Tests rely on fixtures.rs
#[cfg(test)]
mod test {
    use super::{reconcile, Context, CoreDB};
    use std::sync::Arc;

    #[tokio::test]
    async fn new_coredbs_without_finalizers_gets_a_finalizer() {
        let (testctx, fakeserver, _) = Context::test();
        let coredb = CoreDB::test();
        // verify that coredb gets a finalizer attached during reconcile
        fakeserver.handle_finalizer_creation(&coredb);
        let res = reconcile(Arc::new(coredb), testctx).await;
        assert!(res.is_ok(), "initial creation succeeds in adding finalizer");
    }

    #[tokio::test]
    async fn test_patches_coredb() {
        let (testctx, fakeserver, _) = Context::test();
        let coredb = CoreDB::test().finalized();
        fakeserver.handle_coredb_patch(&coredb);
        let res = reconcile(Arc::new(coredb), testctx).await;
        assert!(res.is_ok(), "finalized coredb succeeds in its reconciler");
    }
}
