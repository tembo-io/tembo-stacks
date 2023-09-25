use crate::{apis::coredb_types::CoreDB, Context, Error, Result};
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec},
        core::v1::{
            Container, ContainerPort, EnvVar, HTTPGetAction, PodSpec, PodTemplateSpec, Probe,
            SecurityContext, Service, ServicePort, ServiceSpec,
        },
    },
    apimachinery::pkg::{
        apis::meta::v1::{LabelSelector, OwnerReference},
        util::intstr::IntOrString,
    },
};
use kube::{
    api::{Api, ListParams, ObjectMeta, Patch, PatchParams, ResourceExt},
    runtime::controller::Action,
    Client, Resource,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

use super::types::AppService;
use tracing::*;

// private wrapper to hold the AppService Resources
struct AppServiceResources {
    deployment: Deployment,
    name: String,
    service: Option<Service>,
}


const COMPONENT_NAME: &str = "appService";

// generates Kubernetes Deployment and Service templates for a AppService
fn generate_resource(
    appsvc: &AppService,
    cdb_name: String,
    namespace: &str,
    oref: OwnerReference,
) -> AppServiceResources {
    let name = format!("{cdb_name}-{}", appsvc.name.clone());
    let service = appsvc
        .ports
        .as_ref()
        .map(|_| generate_service(appsvc, name.clone(), namespace, oref.clone()));
    let deployment = generate_deployment(appsvc, name.clone(), namespace, oref);
    AppServiceResources {
        deployment,
        name,
        service,
    }
}

// templates the Kubernetes Service for an AppService
fn generate_service(
    appsvc: &AppService,
    resource_name: String,
    namespace: &str,
    oref: OwnerReference,
) -> Service {
    let mut selector_labels: BTreeMap<String, String> = BTreeMap::new();
    selector_labels.insert("app".to_owned(), resource_name.clone());
    selector_labels.insert("component".to_owned(), COMPONENT_NAME.to_string());
    selector_labels.insert("coredb.io/name".to_owned(), namespace.to_owned());

    let mut labels = selector_labels.clone();
    labels.insert("component".to_owned(), COMPONENT_NAME.to_owned());

    let ports = match appsvc.ports.as_ref() {
        Some(ports) => {
            let ports: Vec<ServicePort> = ports
                .iter()
                .map(|pm| ServicePort {
                    port: pm.container as i32,
                    name: Some(appsvc.name.clone()),
                    target_port: Some(IntOrString::String(appsvc.name.clone())),
                    ..ServicePort::default()
                })
                .collect();
            Some(ports)
        }
        None => None,
    };
    Service {
        metadata: ObjectMeta {
            name: Some(resource_name.to_owned()),
            namespace: Some(namespace.to_owned()),
            labels: Some(labels.clone()),
            owner_references: Some(vec![oref]),
            ..ObjectMeta::default()
        },
        spec: Some(ServiceSpec {
            ports,
            selector: Some(selector_labels.clone()),
            ..ServiceSpec::default()
        }),
        ..Service::default()
    }
}


// templates a single Kubernetes Deployment for an AppService
fn generate_deployment(
    appsvc: &AppService,
    resource_name: String,
    namespace: &str,
    oref: OwnerReference,
) -> Deployment {
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    labels.insert("app".to_owned(), resource_name.clone());
    labels.insert("component".to_owned(), COMPONENT_NAME.to_string());
    labels.insert("coredb.io/name".to_owned(), namespace.to_owned());

    let deployment_metadata = ObjectMeta {
        name: Some(resource_name.clone()),
        namespace: Some(namespace.to_owned()),
        labels: Some(labels.clone()),
        owner_references: Some(vec![oref]),
        ..ObjectMeta::default()
    };

    let (readiness_probe, liveness_probe) = match appsvc.probes.clone() {
        Some(probes) => {
            let readiness_probe = Probe {
                http_get: Some(HTTPGetAction {
                    path: Some(probes.readiness.path),
                    port: IntOrString::String(probes.readiness.port),
                    ..HTTPGetAction::default()
                }),
                initial_delay_seconds: Some(probes.readiness.initial_delay_seconds as i32),
                ..Probe::default()
            };
            let liveness_probe = Probe {
                http_get: Some(HTTPGetAction {
                    path: Some(probes.liveness.path),
                    port: IntOrString::String(probes.liveness.port),
                    ..HTTPGetAction::default()
                }),
                initial_delay_seconds: Some(probes.liveness.initial_delay_seconds as i32),
                ..Probe::default()
            };
            (Some(readiness_probe), Some(liveness_probe))
        }
        None => {
            // are there default probes we could configure when none are provided?
            (None, None)
        }
    };

    // container port mapping
    let container_ports: Option<Vec<ContainerPort>> = match appsvc.ports.as_ref() {
        Some(ports) => {
            let container_ports: Vec<ContainerPort> = ports
                .iter()
                .map(|pm| ContainerPort {
                    container_port: pm.container as i32,
                    host_port: Some(pm.host as i32),
                    name: Some(appsvc.name.clone()),
                    protocol: Some("TCP".to_string()),
                    ..ContainerPort::default()
                })
                .collect();
            Some(container_ports)
        }
        None => None,
    };

    let security_context = SecurityContext {
        run_as_user: Some(65534),
        allow_privilege_escalation: Some(false),
        ..SecurityContext::default()
    };

    let env_vars: Option<Vec<EnvVar>> = appsvc.env.clone().map(|env| {
        env.into_iter()
            .map(|(k, v)| EnvVar {
                name: k,
                value: Some(v),
                ..EnvVar::default()
            })
            .collect()
    });
    // TODO: Container VolumeMounts, currently not in scope
    // TODO: PodSpec volumes, currently not in scope

    let pod_spec = PodSpec {
        containers: vec![Container {
            args: appsvc.args.clone(),
            env: env_vars,
            image: Some(appsvc.image.clone()),
            name: appsvc.name.clone(),
            ports: container_ports,
            resources: appsvc.resources.clone(),
            readiness_probe,
            liveness_probe,
            security_context: Some(security_context),
            ..Container::default()
        }],
        ..PodSpec::default()
    };

    let pod_template_spec = PodTemplateSpec {
        metadata: Some(deployment_metadata.clone()),
        spec: Some(pod_spec),
    };

    let deployment_spec = DeploymentSpec {
        selector: LabelSelector {
            match_labels: Some(labels.clone()),
            ..LabelSelector::default()
        },
        template: pod_template_spec,
        ..DeploymentSpec::default()
    };
    Deployment {
        metadata: deployment_metadata,
        spec: Some(deployment_spec),
        ..Deployment::default()
    }
}

// gets all names of AppService Deployments in the namespace that have the label "component=AppService"
async fn get_appservice_deployments(client: &Client, namespace: &str) -> Result<Vec<String>, Error> {
    let label_selector = format!("component={}", COMPONENT_NAME);
    let deployent_api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels(&label_selector).timeout(10);
    let deployments = deployent_api.list(&lp).await.map_err(Error::KubeError)?;
    Ok(deployments
        .items
        .iter()
        .map(|d| d.metadata.name.to_owned().expect("no name on resource"))
        .collect())
}

// gets all names of AppService Services in the namespace that have the label "component=AppService"
async fn get_appservice_services(client: &Client, namespace: &str) -> Result<Vec<String>, Error> {
    let label_selector = format!("component={}", COMPONENT_NAME);
    let deployent_api: Api<Service> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels(&label_selector).timeout(10);
    let services = deployent_api.list(&lp).await.map_err(Error::KubeError)?;
    Ok(services
        .items
        .iter()
        .map(|d| d.metadata.name.to_owned().expect("no name on resource"))
        .collect())
}

// determines AppService deployments
fn appservice_to_delete(desired: Vec<String>, actual: Vec<String>) -> Option<Vec<String>> {
    let mut to_delete: Vec<String> = Vec::new();
    for a in actual {
        // if actual not in desired, put it in the delete vev
        if !desired.contains(&a) {
            to_delete.push(a);
        }
    }
    if to_delete.is_empty() {
        None
    } else {
        Some(to_delete)
    }
}

async fn apply_resources(resources: Vec<AppServiceResources>, client: &Client, ns: &str) -> bool {
    let deployment_api: Api<Deployment> = Api::namespaced(client.clone(), ns);
    let service_api: Api<Service> = Api::namespaced(client.clone(), ns);
    let ps = PatchParams::apply("cntrlr").force();

    let mut has_errors: bool = false;

    // apply desired resources
    for res in resources {
        match deployment_api
            .patch(&res.name, &ps, &Patch::Apply(&res.deployment))
            .await
            .map_err(Error::KubeError)
        {
            Ok(_) => {
                debug!("ns: {}, applied AppService Deployment: {}", ns, res.name);
            }
            Err(e) => {
                // TODO: find a better way to handle single error without stopping all reconciliation of AppService
                has_errors = true;
                error!(
                    "ns: {}, failed to apply AppService Deployment: {}, error: {}",
                    ns, res.name, e
                );
            }
        }
        if res.service.is_none() {
            continue;
        }
        match service_api
            .patch(&res.name, &ps, &Patch::Apply(&res.service))
            .await
            .map_err(Error::KubeError)
        {
            Ok(_) => {
                debug!("ns: {}, applied AppService Service: {}", ns, res.name);
            }
            Err(e) => {
                // TODO: find a better way to handle single error without stopping all reconciliation of AppService
                has_errors = true;
                error!(
                    "ns: {}, failed to apply AppService Service: {}, error: {}",
                    ns, res.name, e
                );
            }
        }
    }
    has_errors
}


pub async fn reconcile_app_services(cdb: &CoreDB, ctx: Arc<Context>) -> Result<(), Action> {
    let client = ctx.client.clone();
    let ns = cdb.namespace().unwrap();
    let oref = cdb.controller_owner_ref(&()).unwrap();
    let deployment_api: Api<Deployment> = Api::namespaced(client.clone(), &ns);
    let service_api: Api<Service> = Api::namespaced(client.clone(), &ns);


    let desired_deployments = match cdb.spec.app_services.clone() {
        Some(appsvcs) => appsvcs.iter().map(|a| a.name.clone()).collect(),
        None => {
            debug!("No AppServices found in Instance: {}", ns);
            vec![]
        }
    };

    // only deploy the Kubernetes Service when there are port mappings
    let desired_services = match cdb.spec.app_services.clone() {
        Some(appsvcs) => {
            let mut desired_svc: Vec<String> = Vec::new();
            for appsvc in appsvcs.iter() {
                if appsvc.ports.as_ref().is_some() {
                    desired_svc.push(appsvc.name.clone());
                }
            }
            desired_svc
        }
        None => {
            vec![]
        }
    };
    // TODO: we can improve our overall error handling design
    // for app_service reconciliation, not stop all reconciliation if an operation on a single AppService fails
    // however, we do want to requeue if there are any error
    // currently there are no expected errors in this path
    // for simplicity, we will return a requeue Action if there are errors
    let mut has_errors: bool = false;

    let actual_deployments = match get_appservice_deployments(&client, &ns).await {
        Ok(deployments) => deployments,
        Err(e) => {
            has_errors = true;
            error!("ns: {}, failed to get AppService Deployments: {}", ns, e);
            vec![]
        }
    };
    let actual_services = match get_appservice_services(&client, &ns).await {
        Ok(services) => services,
        Err(e) => {
            has_errors = true;
            error!("ns: {}, failed to get AppService Services: {}", ns, e);
            vec![]
        }
    };

    // reap any AppService Deployments that are no longer desired
    if let Some(to_delete) = appservice_to_delete(desired_deployments, actual_deployments) {
        for d in to_delete {
            match deployment_api.delete(&d, &Default::default()).await {
                Ok(_) => {
                    debug!("ns: {}, successfully deleted AppService: {}", ns, d);
                }
                Err(e) => {
                    has_errors = true;
                    error!("ns: {}, Failed to delete AppService: {}, error: {}", ns, d, e);
                }
            }
        }
    }

    // reap any AppService  that are no longer desired
    if let Some(to_delete) = appservice_to_delete(desired_services, actual_services) {
        for d in to_delete {
            match service_api.delete(&d, &Default::default()).await {
                Ok(_) => {
                    debug!("ns: {}, successfully deleted AppService: {}", ns, d);
                }
                Err(e) => {
                    has_errors = true;
                    error!("ns: {}, Failed to delete AppService: {}, error: {}", ns, d, e);
                }
            }
        }
    }

    let appsvcs = match cdb.spec.app_services.clone() {
        Some(appsvcs) => appsvcs,
        None => {
            debug!("ns: {}, No AppServices found in spec", ns);
            return Ok(());
        }
    };

    let resources: Vec<AppServiceResources> = appsvcs
        .iter()
        .map(|appsvc| generate_resource(appsvc, cdb.name_any(), &ns, oref.clone()))
        .collect();


    let apply_errored = apply_resources(resources, &client, &ns).await;

    if has_errors || apply_errored {
        return Err(Action::requeue(Duration::from_secs(300)));
    }
    Ok(())
}
