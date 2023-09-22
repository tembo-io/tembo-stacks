use crate::{apis::coredb_types::CoreDB, Context, Error, Result};
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec},
        core::v1::{
            Container, ContainerPort, EnvVar, HTTPGetAction, PodSpec, PodTemplateSpec, Probe, SecurityContext,
        },
    },
    apimachinery::pkg::{
        apis::meta::v1::{LabelSelector, OwnerReference},
        util::intstr::IntOrString,
    },
};
use kube::{
    api::{Api, ListParams, ObjectMeta, Patch, PatchParams, ResourceExt},
    Client, Resource,
};
use std::{collections::BTreeMap, sync::Arc};

use super::types::AppService;
use tracing::*;

// private wrapper to hold the AppService and the name of the Deployment
struct AppDeployment {
    deployment: Deployment,
    name: String,
}

// creates a single Deployment given an AppService
fn generate_deployment(appsvc: &AppService, namespace: &str, oref: OwnerReference) -> AppDeployment {
    // namespace and owner name are the same

    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    labels.insert("app".to_owned(), appsvc.name.clone());
    labels.insert("component".to_owned(), "AppService".to_string());
    labels.insert("coredb.io/name".to_owned(), namespace.to_owned());

    let deployment_metadata = ObjectMeta {
        name: Some(appsvc.name.to_owned()),
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
    AppDeployment {
        deployment: Deployment {
            metadata: deployment_metadata,
            spec: Some(deployment_spec),
            ..Deployment::default()
        },
        name: appsvc.name.clone(),
    }
}

// gets all names of AppService Deployments in the namespace that have the label "component=AppService"
async fn get_appservice_deployments(client: &Client, namespace: &str) -> Result<Vec<String>, Error> {
    let deployent_api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels("component=AppService").timeout(10);
    let deployments = deployent_api.list(&lp).await.map_err(Error::KubeError)?;
    Ok(deployments
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


pub async fn reconcile_app_services(cdb: &CoreDB, ctx: Arc<Context>) -> Result<(), Error> {
    let client = ctx.client.clone();
    let ns = cdb.namespace().unwrap();
    let oref = cdb.controller_owner_ref(&()).unwrap();
    let deployment_api: Api<Deployment> = Api::namespaced(client.clone(), &ns);

    let desired_deployments = match cdb.spec.app_services.clone() {
        Some(appsvcs) => appsvcs.iter().map(|a| a.name.clone()).collect(),
        None => {
            debug!("No AppServices found in Instance: {}", ns);
            vec![]
        }
    };
    let actual_deployments = get_appservice_deployments(&client, &ns).await?;

    // reap any deployments that are no longer desired
    if let Some(to_delete) = appservice_to_delete(desired_deployments, actual_deployments) {
        for d in to_delete {
            match deployment_api.delete(&d, &Default::default()).await {
                Ok(_) => {
                    debug!("Successfully deleted AppService: {}", d);
                }
                Err(e) => {
                    error!("Failed to delete AppService: {}, error: {}", d, e);
                }
            }
        }
    }

    let appsvcs = match cdb.spec.app_services.clone() {
        Some(appsvcs) => appsvcs,
        None => {
            debug!("No AppServices found in Instance: {}", ns);
            return Ok(());
        }
    };

    let deployments: Vec<AppDeployment> = appsvcs
        .iter()
        .map(|appsvc| generate_deployment(appsvc, &ns, oref.clone()))
        .collect();

    let ps = PatchParams::apply("cntrlr").force();
    // apply desired deployments
    for ad in deployments {
        match deployment_api
            .patch(&ad.name, &ps, &Patch::Apply(&ad.deployment))
            .await
            .map_err(Error::KubeError)
        {
            Ok(_) => {
                debug!("Successfully reconciled AppService: {}", ad.name);
            }
            Err(e) => {
                // is here a better way to surface failures without completely stopping all reconciliation of AppService?
                error!("Failed to reconcile AppService: {}, error: {}", ad.name, e);
            }
        }
    }

    Ok(())
}
