use crate::{apis::coredb_types::CoreDB, Context, Error, Result};
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec},
        core::v1::{
            Container, ContainerPort, EnvVar, HTTPGetAction, PodSpec, PodTemplateSpec, Probe, SecurityContext,
        },
    },
    apimachinery::pkg::{apis::meta::v1::LabelSelector, util::intstr::IntOrString},
};
use kube::{
    api::{Api, ObjectMeta, Patch, PatchParams, ResourceExt},
    Resource,
};
use std::{collections::BTreeMap, sync::Arc};

// TODO: run this for every app_service
pub async fn reconcile_app_services(cdb: &CoreDB, ctx: Arc<Context>) -> Result<(), Error> {
    let appsvc = cdb.spec.app_services.clone().unwrap()[0].clone();

    let client = ctx.client.clone();
    let ns = cdb.namespace().unwrap();
    let name = format!("{}-{}", cdb.name_any(), appsvc.name);
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    let deployment_api: Api<Deployment> = Api::namespaced(client, &ns);
    let oref = cdb.controller_owner_ref(&()).unwrap();
    labels.insert("app".to_owned(), name.clone());
    labels.insert("component".to_owned(), "AppService".to_string());
    labels.insert("coredb.io/name".to_owned(), cdb.name_any());

    let deployment_metadata = ObjectMeta {
        name: Some(name.to_owned()),
        namespace: Some(ns.to_owned()),
        labels: Some(labels.clone()),
        owner_references: Some(vec![oref]),
        ..ObjectMeta::default()
    };

    let (readiness_probe, liveness_probe) = match appsvc.probes {
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
    let container_ports: Option<Vec<ContainerPort>> = match appsvc.ports {
        Some(ports) => {
            let container_ports: Vec<ContainerPort> = ports
                .into_iter()
                .map(|pm| ContainerPort {
                    container_port: pm.container as i32,
                    host_port: Some(pm.host as i32),
                    name: Some(name.clone()),
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

    let env_vars: Option<Vec<EnvVar>> = match appsvc.env {
        Some(env) => Some(
            env.into_iter()
                .map(|(k, v)| EnvVar {
                    name: k,
                    value: Some(v),
                    ..EnvVar::default()
                })
                .collect(),
        ),
        None => None,
    };

    // TODO: Container VolumeMounts, currently not in scope
    // TODO: PodSpec volumes, currently not in scope

    let pod_spec = PodSpec {
        containers: vec![Container {
            args: appsvc.args,
            env: env_vars,
            image: Some(appsvc.image),
            name: name.clone(),
            ports: container_ports,
            readiness_probe,
            liveness_probe,
            security_context: Some(security_context),
            ..Container::default()
        }],
        ..PodSpec::default()
    };

    // Generate the PodTemplateSpec for the DeploymentSpec
    let pod_template_spec = PodTemplateSpec {
        metadata: Some(deployment_metadata.clone()),
        spec: Some(pod_spec),
    };

    // Generate the DeploymentSpec for the Deployment
    let deployment_spec = DeploymentSpec {
        selector: LabelSelector {
            match_labels: Some(labels.clone()),
            ..LabelSelector::default()
        },
        template: pod_template_spec,
        ..DeploymentSpec::default()
    };

    // Generate the Deployment for Prometheus Exporter
    let deployment = Deployment {
        metadata: deployment_metadata,
        spec: Some(deployment_spec),
        ..Deployment::default()
    };

    let ps = PatchParams::apply("cntrlr").force();
    let _o = deployment_api
        .patch(&name, &ps, &Patch::Apply(&deployment))
        .await
        .map_err(Error::KubeError)?;

    Ok(())
}
