use crate::{
    apis::coredb_types::CoreDB,
    defaults::default_postgres_exporter_image,
    postgres_exporter::{EXPORTER_CONFIGMAP, EXPORTER_VOLUME, QUERIES_YAML},
    rbac::reconcile_rbac,
    Context, Error, Result,
};
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec},
        core::v1::{
            ConfigMapVolumeSource, Container, ContainerPort, EnvVar, HTTPGetAction, PodSpec, PodTemplateSpec,
            Probe, SecurityContext, Volume, VolumeMount,
        },
        rbac::v1::PolicyRule,
    },
    apimachinery::pkg::{apis::meta::v1::LabelSelector, util::intstr::IntOrString},
};
use kube::{
    api::{Api, ObjectMeta, Patch, PatchParams, ResourceExt},
    Resource,
};
use std::{collections::BTreeMap, sync::Arc};

const PROM_CFG_DIR: &str = "/prometheus";


pub async fn reconcile_prometheus_exporter(cdb: &CoreDB, ctx: Arc<Context>) -> Result<(), Error> {
    let client = ctx.client.clone();
    let ns = cdb.namespace().unwrap();
    let name = "postgres-exporter";
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    let deployment_api: Api<Deployment> = Api::namespaced(client, &ns);
    let oref = cdb.controller_owner_ref(&()).unwrap();
    labels.insert("app".to_owned(), name.to_string());
    labels.insert("coredb.io/name".to_owned(), cdb.name_any());

    // reconcile rbac(service account, role, role binding) for the postgres-exporter
    let rbac = reconcile_rbac(cdb, ctx.clone(), Some("metrics"), create_policy_rules(cdb).await).await?;

    // Generate the ObjectMeta for the Deployment
    let deployment_metadata = ObjectMeta {
        name: Some(name.to_owned()),
        namespace: Some(ns.to_owned()),
        labels: Some(labels.clone()),
        owner_references: Some(vec![oref]),
        ..ObjectMeta::default()
    };

    // 0 replicas on deployment when stopping
    // 1 replica in all other cases
    let replicas = match cdb.spec.stop {
        true => 0,
        false => 1,
    };

    // Generate the Probe for the Container
    let readiness_probe = Probe {
        http_get: Some(HTTPGetAction {
            path: Some("/metrics".to_string()),
            port: IntOrString::String("metrics".to_string()),
            ..HTTPGetAction::default()
        }),
        initial_delay_seconds: Some(3),
        ..Probe::default()
    };

    // Generate ContainerPort for the Container
    let container_port = vec![ContainerPort {
        container_port: 9187,
        name: Some("metrics".to_string()),
        protocol: Some("TCP".to_string()),
        ..ContainerPort::default()
    }];

    // Generate SecurityContext for the Container
    let security_context = SecurityContext {
        run_as_user: Some(65534),
        allow_privilege_escalation: Some(false),
        ..SecurityContext::default()
    };

    // Generate EnvVar for the Container
    let env_vars = vec![
        EnvVar {
            name: "DATA_SOURCE_NAME".to_string(),
            value: Some(format!(
                "postgresql://postgres_exporter@{}:5432/postgres",
                cdb.name_any()
            )),
            ..EnvVar::default()
        },
        EnvVar {
            name: "PG_EXPORTER_EXTEND_QUERY_PATH".to_string(),
            value: Some(format!("{PROM_CFG_DIR}/{QUERIES_YAML}")),
            ..EnvVar::default()
        },
    ];

    // Generate VolumeMounts for the Container
    let exporter_vol_mounts = vec![VolumeMount {
        name: EXPORTER_VOLUME.to_owned(),
        mount_path: PROM_CFG_DIR.to_string(),
        ..VolumeMount::default()
    }];

    // Generate Volumes for the PodSpec
    let exporter_volumes = vec![Volume {
        config_map: Some(ConfigMapVolumeSource {
            name: Some(EXPORTER_VOLUME.to_owned()),
            ..ConfigMapVolumeSource::default()
        }),
        name: EXPORTER_CONFIGMAP.to_owned(),
        ..Volume::default()
    }];

    // Generate the PodSpec for the PodTemplateSpec
    let pod_spec = PodSpec {
        containers: vec![Container {
            args: Some(vec!["--auto-discover-databases".to_string()]),
            env: Some(env_vars),
            image: Some(default_postgres_exporter_image()),
            name: name.to_string(),
            ports: Some(container_port),
            readiness_probe: Some(readiness_probe),
            security_context: Some(security_context),
            volume_mounts: Some(exporter_vol_mounts),
            ..Container::default()
        }],
        service_account: rbac.service_account.metadata.name.clone(),
        service_account_name: rbac.service_account.metadata.name.clone(),
        volumes: Some(exporter_volumes),
        ..PodSpec::default()
    };

    // Generate the PodTemplateSpec for the DeploymentSpec
    let pod_template_spec = PodTemplateSpec {
        metadata: Some(deployment_metadata.clone()),
        spec: Some(pod_spec),
    };

    // Generate the DeploymentSpec for the Deployment
    let deployment_spec = DeploymentSpec {
        replicas: Some(replicas),
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
        .patch(name, &ps, &Patch::Apply(&deployment))
        .await
        .map_err(Error::KubeError)?;

    Ok(())
}

// Generate the PolicyRules for the Role
async fn create_policy_rules(cdb: &CoreDB) -> Vec<PolicyRule> {
    vec![
        // This policy allows create, get, list for pods & pods/exec
        PolicyRule {
            api_groups: Some(vec!["".to_owned()]),
            resources: Some(vec!["pods".to_owned()]),
            verbs: vec!["create".to_string(), "get".to_string(), "watch".to_string()],
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
