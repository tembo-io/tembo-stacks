use crate::{
    apis::coredb_types::CoreDB,
    cloudnativepg::cnpg::{get_fenced_pods, unfence_pod},
    extensions::{
        kubernetes_queries::{add_trunk_install_to_status, remove_trunk_installs_from_status},
        types::{TrunkInstall, TrunkInstallStatus},
    },
    Context,
};
use k8s_openapi::{api::core::v1::Pod, apimachinery::pkg::apis::meta::v1::ObjectMeta};
use kube::{runtime::controller::Action, Api};
use std::{sync::Arc, time::Duration};
use tracing::{debug, error, info, instrument, span, warn, Level};

use crate::apis::coredb_types::CoreDBStatus;

// Collect any fenced pods and add them to the list of pods to install extensions into
async fn fenced_instances(cdb: &CoreDB, ctx: Arc<Context>, pods: &mut Vec<Pod>) -> Result<Vec<Pod>, Action> {
    let name = cdb.metadata.name.clone().expect("CoreDB should have a name");

    // Get fenced pods
    let fenced_instances = get_fenced_pods(cdb, ctx.clone(), &name).await?;

    // Check if there are any fenced pods
    if let Some(fenced_names) = fenced_instances {
        debug!("Fenced pod names found: {:?}", fenced_names);

        // Create Pod objects from fenced_names and append to pods
        for fenced_name in fenced_names {
            let new_pod = Pod {
                metadata: ObjectMeta {
                    name: Some(fenced_name),
                    ..Default::default()
                },
                ..Default::default()
            };
            pods.push(new_pod);
        }
    }

    debug!("After appending fenced instances, pod count: {}", pods.len());

    Ok(pods.to_vec())
}

#[instrument(skip(ctx, cdb))]
pub async fn reconcile_trunk_installs(
    cdb: &CoreDB,
    ctx: Arc<Context>,
) -> Result<Vec<TrunkInstallStatus>, Action> {
    let span = span!(Level::INFO, "reconcile_trunk_installs");
    let _enter = span.enter();

    info!("Starting to reconcile trunk installs");

    // Fetch all pods
    let mut all_pods = cdb.pods_by_cluster(ctx.client.clone()).await?;

    let coredb_api: Api<CoreDB> = Api::namespaced(
        ctx.client.clone(),
        &cdb.metadata
            .namespace
            .clone()
            .expect("CoreDB should have a namespace"),
    );

    all_pods = fenced_instances(cdb, ctx.clone(), &mut all_pods).await?;

    // Get extensions in status.trunk_install that are not in spec
    // Deleting them from status allows for retrying installation
    // by first removing the extension from the spec, then adding it back
    let span = span!(Level::DEBUG, "remove_trunk_installs_from_status");
    let _enter = span.enter();
    info!("Checking for trunk installs to remove from status");

    let trunk_installs_to_remove_from_status = match &cdb.status {
        None => {
            vec![]
        }
        Some(status) => match &status.trunk_installs {
            None => {
                vec![]
            }
            Some(trunk_installs) => trunk_installs
                .iter()
                .filter(|&ext_status| {
                    !cdb.spec
                        .trunk_installs
                        .iter()
                        .any(|ext| ext.name == ext_status.name)
                })
                .collect::<Vec<_>>(),
        },
    };

    // Get list of names
    let trunk_install_names_to_remove_from_status = trunk_installs_to_remove_from_status
        .iter()
        .map(|ext_status| ext_status.name.clone())
        .collect::<Vec<_>>();

    // Remove extensions from status
    remove_trunk_installs_from_status(
        &coredb_api,
        &cdb.metadata.name.clone().expect("CoreDB should have a name"),
        trunk_install_names_to_remove_from_status,
    )
    .await?;

    // Get extensions in spec.trunk_install that are not in status.trunk_install
    let span = span!(Level::DEBUG, "install_extensions");
    let _enter = span.enter();
    info!("Checking for trunk installs to add");

    let mut all_results = Vec::new();
    for pod in all_pods {
        let pod_name = pod.metadata.name.expect("Pod should always have a name");

        // Filter trunk installs that are not yet installed on this instance
        let trunk_installs_to_install = cdb
            .spec
            .trunk_installs
            .iter()
            .filter(|&ext| {
                debug!("Checking if extension {} needs to be installed on pod {}", ext.name, pod_name);

                let should_install = !cdb.status
                    .clone()
                    .unwrap_or_default()
                    .trunk_installs
                    .unwrap_or_default()
                    .iter()
                    .any(|ext_status| {
                        let should_skip = ext.name == ext_status.name
                            && ext_status
                                .installed_to_pods
                                .as_ref()
                                .expect("installed_to_pods should always be Some(Vec)")
                                .contains(&pod_name);

                        if should_skip {
                            debug!("Skipping installation of extension {} for pod {} because it's already installed", ext.name, pod_name);
                        }

                        should_skip
                    });

                if should_install {
                    debug!("Extension {} will be installed on pod {}", ext.name, pod_name);
                }

                should_install
            })
            .collect::<Vec<_>>();

        debug!(
            "Trunk installs to install on {}: {:?}",
            pod_name, trunk_installs_to_install
        );

        if trunk_installs_to_install.is_empty() {
            info!("Make sure pod {} is unfenced", pod_name);
            // Check if pod is fenced
            let fenced_pods =
                get_fenced_pods(cdb, ctx.clone(), cdb.metadata.name.as_deref().unwrap_or_default()).await?;
            if let Some(fenced_pods) = fenced_pods {
                // Check if pod_name is in fenced_pods
                if fenced_pods.contains(&pod_name) {
                    debug!("Unfencing pod {:?}", pod_name);
                    // Unfence pod_name
                    unfence_pod(
                        cdb,
                        ctx.clone(),
                        cdb.metadata.name.as_deref().unwrap_or_default(),
                        &pod_name.clone(),
                    )
                    .await?;
                }
            }
            continue;
        }

        // Install missing trunk installs
        match install_extensions(cdb, trunk_installs_to_install, &ctx, pod_name.clone()).await {
            Ok(result) => {
                all_results = result;
            }
            Err(err) => return Err(err),
        };
    }

    info!(
        "Completed reconciliation for trunk installs for {:?}",
        &cdb.metadata.name.clone()
    );

    Ok(all_results)
}

/// handles installing extensions
#[instrument(skip(ctx, cdb))]
pub async fn install_extensions(
    cdb: &CoreDB,
    trunk_installs: Vec<&TrunkInstall>,
    ctx: &Arc<Context>,
    pod_name: String,
) -> Result<Vec<TrunkInstallStatus>, Action> {
    let span = span!(Level::INFO, "install_extensions");
    let _enter = span.enter();

    let coredb_name = cdb.metadata.name.clone().expect("CoreDB should have a name");
    let mut current_trunk_install_statuses: Vec<TrunkInstallStatus> = cdb
        .status
        .clone()
        .unwrap_or_else(|| {
            debug!("No current status on {}, initializing default", coredb_name);
            CoreDBStatus::default()
        })
        .trunk_installs
        .unwrap_or_else(|| {
            debug!(
                "No current trunk installs on {}, initializing empty list",
                coredb_name
            );
            vec![]
        });
    if trunk_installs.is_empty() {
        debug!("No extensions to install into {}", coredb_name);
        return Ok(current_trunk_install_statuses);
    }
    info!("Installing extensions into {}: {:?}", coredb_name, trunk_installs);
    let span = span!(Level::INFO, "install_each_extension");
    let _enter = span.enter();

    let client = ctx.client.clone();
    let coredb_api: Api<CoreDB> = Api::namespaced(
        ctx.client.clone(),
        &cdb.metadata
            .namespace
            .clone()
            .expect("CoreDB should have a namespace"),
    );

    let mut requeue = false;
    for ext in trunk_installs.iter() {
        // Nested span for individual extension installation
        let ext_span = span!(Level::INFO, "install_individual_extension", extension = ext.name);
        let _ext_enter = ext_span.enter();
        info!("Attempting to install extension: {}", ext.name);

        let version = match ext.version.clone() {
            None => {
                error!(
                    "Installing extension {} into {}: missing version",
                    ext.name, coredb_name
                );
                let trunk_install_status = TrunkInstallStatus {
                    name: ext.name.clone(),
                    version: None,
                    error: true,
                    error_message: Some("Missing version".to_string()),
                    installed_to_pods: Some(vec![pod_name.clone()]),
                };
                current_trunk_install_statuses =
                    add_trunk_install_to_status(&coredb_api, &coredb_name, &trunk_install_status).await?;
                continue;
            }
            Some(version) => version,
        };

        let cmd = vec![
            "trunk".to_owned(),
            "install".to_owned(),
            "-r https://registry.pgtrunk.io".to_owned(),
            ext.name.clone(),
            "--version".to_owned(),
            version,
        ];

        // Get pod status of pod_name
        if let Err(e) = cdb.log_pod_status(client.clone(), &pod_name).await {
            warn!("Could not fetch or log pod status: {:?}", e);
        }

        let result = cdb.exec(pod_name.clone(), client.clone(), &cmd).await;

        match result {
            Ok(result) => {
                let output_stdout = result
                    .stdout
                    .clone()
                    .unwrap_or_else(|| "Nothing in stdout".to_string());
                let output_stderr = result
                    .stderr
                    .clone()
                    .unwrap_or_else(|| "Nothing in stderr".to_string());
                let output = format!("{}\n{}", output_stdout, output_stderr);
                match result.success {
                    true => {
                        info!("Installed extension {} into {}", &ext.name, coredb_name);
                        debug!("{}", output);
                        let trunk_install_status = TrunkInstallStatus {
                            name: ext.name.clone(),
                            version: ext.version.clone(),
                            error: false,
                            error_message: None,
                            installed_to_pods: Some(vec![pod_name.clone()]),
                        };
                        current_trunk_install_statuses =
                            add_trunk_install_to_status(&coredb_api, &coredb_name, &trunk_install_status)
                                .await?;
                    }
                    false => {
                        error!(
                            "Failed to install extension {} into {}:\n{}",
                            &ext.name,
                            coredb_name,
                            output.clone()
                        );
                        let trunk_install_status = TrunkInstallStatus {
                            name: ext.name.clone(),
                            version: ext.version.clone(),
                            error: true,
                            error_message: Some(output),
                            installed_to_pods: Some(vec![pod_name.clone()]),
                        };
                        current_trunk_install_statuses =
                            add_trunk_install_to_status(&coredb_api, &coredb_name, &trunk_install_status)
                                .await?
                    }
                }
            }
            Err(err) => {
                // This kind of error means kube exec failed, which are errors other than the
                // trunk install command failing inside the pod. So, we should retry
                // when we find this kind of error.
                error!(
                    "Kube exec error installing extension {} into {}: {}",
                    &ext.name, coredb_name, err
                );
                requeue = true
            }
        }
    }
    if requeue {
        warn!("Requeueing due to errors");
        return Err(Action::requeue(Duration::from_secs(10)));
    }
    info!("Successfully installed all extensions");

    // Check for fenced pods and unfence it
    let fenced_pods =
        get_fenced_pods(cdb, ctx.clone(), cdb.metadata.name.as_deref().unwrap_or_default()).await?;
    if let Some(fenced_pods) = fenced_pods {
        // Check if pod_name is in fenced_pods
        if fenced_pods.contains(&pod_name) {
            debug!("Unfencing pod {:?}", pod_name);
            // Unfence pod_name
            unfence_pod(
                cdb,
                ctx.clone(),
                cdb.metadata.name.as_deref().unwrap_or_default(),
                &pod_name.clone(),
            )
            .await?;
        }
    }

    Ok(current_trunk_install_statuses)
}
