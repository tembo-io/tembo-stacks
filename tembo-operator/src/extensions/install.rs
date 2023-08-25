use crate::{
    apis::coredb_types::{CoreDB, CoreDBStatus},
    exec::ExecOutput,
    extensions::{
        kubernetes_queries::{add_trunk_install_to_status, remove_trunk_installs_from_status},
        types::{TrunkInstall, TrunkInstallStatus},
    },
    Context,
};
use kube::{runtime::controller::Action, Api};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tracing::{debug, error, info, instrument, span, warn, Level};

pub async fn reconcile_trunk_installs(
    cdb: &CoreDB,
    ctx: Arc<Context>,
) -> Result<Vec<TrunkInstallStatus>, Action> {
    // Create a new span for this function.
    let _span = span!(Level::INFO, "reconcile_trunk_installs");
    let _enter = _span.enter();

    info!("Starting reconciliation for trunk installs");

    // Fetch all pods
    let all_pods = cdb.pods_by_cluster(ctx.client.clone()).await?;

    let coredb_api: Api<CoreDB> = Api::namespaced(
        ctx.client.clone(),
        &cdb.metadata
            .namespace
            .clone()
            .expect("CoreDB should have a namespace"),
    );

    // Get extensions in status.trunk_install that are not in spec
    // Deleting them from status allows for retrying installation
    // by first removing the extension from the spec, then adding it back
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

    // Create a span for removing trunk installs from status
    let remove_span = span!(Level::INFO, "remove_trunk_installs_from_status");
    let _remove_enter = remove_span.enter();

    info!("Removing trunk installs from status");
    // Remove extensions from status
    remove_trunk_installs_from_status(
        &coredb_api,
        &cdb.metadata.name.clone().expect("CoreDB should have a name"),
        trunk_install_names_to_remove_from_status,
    )
    .await?;

    // Create a span for installing extensions
    let install_span = span!(Level::INFO, "install_extensions");
    let _install_enter = install_span.enter();

    info!("Installing extensions");

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
                                .installed_to_instances
                                .as_ref()
                                .expect("installed_to_instances should always be Some(Vec)")
                                .contains(&pod_name);

                        if should_skip {
                            debug!("Skipping installation of extension {} for pod {} because it's already installed", ext.name, pod_name);
                        }

                        should_skip
                    });

                if should_install {
                    info!("Extension {} will be installed on pod {}", ext.name, pod_name);
                }

                should_install
            })
            .collect::<Vec<_>>();

        info!(
            "Trunk installs to install on {}: {:?}",
            pod_name, trunk_installs_to_install
        );

        // Install missing trunk installs
        match install_extensions(cdb, trunk_installs_to_install, &ctx, pod_name.clone()).await {
            Ok(result) => all_results.extend(result),
            Err(err) => return Err(err),
        };
    }

    info!("Completed reconciliation for trunk installs");
    Ok(all_results)
}

// handle_missing_version handles the case where the extension is missing
#[instrument]
async fn handle_missing_version(
    ext: &TrunkInstall,
    coredb_name: &str,
    pod_name: &str,
    coredb_api: &Api<CoreDB>,
) -> Result<TrunkInstallStatus, Action> {
    error!(
        "Installing extension {} into {}: missing version",
        ext.name, coredb_name
    );
    let trunk_install_status = TrunkInstallStatus {
        name: ext.name.clone(),
        version: None,
        error: true,
        error_message: Some("Missing version".to_string()),
        installed_to_instances: Some(vec![pod_name.to_string()]),
    };
    add_trunk_install_to_status(coredb_api, coredb_name, &trunk_install_status).await?;
    Ok(trunk_install_status)
}

// handle_install_result handles the result of installing an extension
#[instrument]
async fn handle_install_result(
    coredb_api: &Api<CoreDB>,
    coredb_name: &str,
    result: &ExecOutput,
    ext: &TrunkInstall,
    pod_name: &str,
    current_trunk_install_statuses: &mut Vec<TrunkInstallStatus>,
) -> Result<TrunkInstallStatus, Action> {
    let output_stdout = result
        .stdout
        .clone()
        .unwrap_or_else(|| "Nothing in stdout".to_string());
    let output_stderr = result
        .stderr
        .clone()
        .unwrap_or_else(|| "Nothing in stderr".to_string());
    let output = format!("{}\n{}", output_stdout, output_stderr);

    let mut trunk_install_status = if let Some(existing_status) = current_trunk_install_statuses
        .iter_mut()
        .find(|s| s.name == ext.name)
    {
        // The extension is already in the list of statuses
        if !existing_status
            .installed_to_instances
            .as_mut()
            .unwrap()
            .contains(&pod_name.to_string())
        {
            existing_status
                .installed_to_instances
                .as_mut()
                .unwrap()
                .push(pod_name.to_string());
        }
        existing_status.clone()
    } else {
        // The extension is not yet in the list of statuses
        let new_status = TrunkInstallStatus {
            name: ext.name.clone(),
            version: ext.version.clone(),
            error: false,
            error_message: None,
            installed_to_instances: Some(vec![pod_name.to_string()]),
        };
        current_trunk_install_statuses.push(new_status.clone());
        new_status
    };

    if result.success {
        info!("Installed extension {} into {}", &ext.name, coredb_name);
        debug!("{}", output);
        trunk_install_status.error = false;
        trunk_install_status.error_message = None;
    } else {
        error!(
            "Failed to install extension {} into {}:\n{}",
            &ext.name, coredb_name, output
        );
        trunk_install_status.error = true;
        trunk_install_status.error_message = Some(output);
    }

    add_trunk_install_to_status(coredb_api, coredb_name, &trunk_install_status).await?;
    Ok(trunk_install_status)
}

// Returns a new vector containing unique `TrunkInstallStatus` instances with merged `installed_to_instances`.
fn deduplicate_trunk_install_statuses(statuses: Vec<TrunkInstallStatus>) -> Vec<TrunkInstallStatus> {
    let mut status_map: HashMap<String, TrunkInstallStatus> = HashMap::new();

    for status in statuses {
        let name_clone = status.name.clone();
        let entry = status_map.entry(name_clone);

        entry
            .and_modify(|existing_status| {
                if let Some(existing_instances) = &mut existing_status.installed_to_instances {
                    if let Some(new_instances) = &status.installed_to_instances {
                        for instance in new_instances.iter() {
                            if !existing_instances.contains(instance) {
                                existing_instances.push(instance.clone());
                            }
                        }
                    }
                } else {
                    existing_status.installed_to_instances = status.installed_to_instances.clone();
                }
            })
            .or_insert(status);
    }

    //status_map.into_iter().map(|(_, v)| v).collect()
    status_map.into_values().collect()
}

/// handles installing extensions
// #[instrument(skip(ctx, cdb))]
// pub async fn install_extensions(
//     cdb: &CoreDB,
//     trunk_installs: Vec<&TrunkInstall>,
//     ctx: &Arc<Context>,
//     pod_name: String,
// ) -> Result<Vec<TrunkInstallStatus>, Action> {
//     let coredb_name = cdb.metadata.name.clone().expect("CoreDB should have a name");
//     let install_span = span!(Level::INFO, "install_extensions", coredb_name = %coredb_name);
//     let _enter = install_span.enter();
//
//     info!("Starting extension installation for {}", coredb_name);
//
//     let mut current_trunk_install_statuses: Vec<TrunkInstallStatus> = cdb
//         .status
//         .clone()
//         .unwrap_or_else(|| {
//             debug!("No current status on {}, initializing default", coredb_name);
//             CoreDBStatus::default()
//         })
//         .trunk_installs
//         .unwrap_or_else(|| {
//             debug!(
//                 "No current trunk installs on {}, initializing empty list",
//                 coredb_name
//             );
//             vec![]
//         });
//     if trunk_installs.is_empty() {
//         info!("No extensions to install into {}", coredb_name);
//         return Ok(current_trunk_install_statuses);
//     }
//     info!("Installing extensions into {}: {:?}", coredb_name, trunk_installs);
//     let client = ctx.client.clone();
//     let coredb_api: Api<CoreDB> = Api::namespaced(
//         ctx.client.clone(),
//         &cdb.metadata
//             .namespace
//             .clone()
//             .expect("CoreDB should have a namespace"),
//     );
//     let mut requeue = false;
//
//     // For each extension execute trunk install
//     for ext in trunk_installs.iter() {
//         let ext_span = span!(Level::INFO, "install_single_extension", extension_name = %ext.name);
//         let _enter_ext = ext_span.enter();
//         info!("Processing extension: {}", ext.name);
//         let version = match ext.version.clone() {
//             None => {
//                 let version_status =
//                     handle_missing_version(ext, &coredb_name, &pod_name, &coredb_api).await?;
//                 current_trunk_install_statuses =
//                     add_trunk_install_to_status(&coredb_api, &coredb_name, &version_status).await?;
//                 continue;
//             }
//             Some(version) => version,
//         };
//
//         let cmd = vec![
//             "trunk".to_owned(),
//             "install".to_owned(),
//             "-r https://registry.pgtrunk.io".to_owned(),
//             ext.name.clone(),
//             "--version".to_owned(),
//             version,
//         ];
//         info!("Executing install command: {:?}", cmd);
//         let result = cdb.exec(pod_name.clone(), client.clone(), &cmd).await;
//
//         match result {
//             Ok(result) => {
//                 let _ = handle_install_result(
//                     &coredb_api,
//                     &coredb_name,
//                     &result,
//                     ext,
//                     &pod_name,
//                     &mut current_trunk_install_statuses,
//                 )
//                 .await?;
//             }
//             Err(err) => {
//                 // This kind of error means kube exec failed, which are errors other than the
//                 // trunk install command failing inside the pod. So, we should retry
//                 // when we find this kind of error.
//                 error!(
//                     "Kube exec error installing extension {} into {}: {}",
//                     &ext.name, coredb_name, err
//                 );
//                 requeue = true
//             }
//         }
//     }
//     // Deduplicate the TrunkInstallStatus instances.
//     let deduplicated_statuses = deduplicate_trunk_install_statuses(current_trunk_install_statuses);
//
//     if requeue {
//         warn!("Requeueing due to errors during installation");
//         return Err(Action::requeue(Duration::from_secs(10)));
//     }
//     Ok(deduplicated_statuses)
// }

#[instrument(skip(ctx, cdb))]
pub async fn install_extensions(
    cdb: &CoreDB,
    trunk_installs: Vec<&TrunkInstall>,
    ctx: &Arc<Context>,
    pod_name: String,
) -> Result<Vec<TrunkInstallStatus>, Action> {
    let coredb_name = cdb.metadata.name.clone().expect("CoreDB should have a name");
    let install_span = span!(Level::INFO, "install_extensions", coredb_name = %coredb_name);
    let _enter = install_span.enter();

    info!("Starting extension installation for {}", coredb_name);

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

    let mut local_statuses: Vec<TrunkInstallStatus> = Vec::new(); // New local vector to hold statuses for this function call

    if trunk_installs.is_empty() {
        info!("No extensions to install into {}", coredb_name);
        return Ok(current_trunk_install_statuses);
    }

    info!("Installing extensions into {}: {:?}", coredb_name, trunk_installs);
    let client = ctx.client.clone();
    let coredb_api: Api<CoreDB> = Api::namespaced(
        ctx.client.clone(),
        &cdb.metadata
            .namespace
            .clone()
            .expect("CoreDB should have a namespace"),
    );
    let mut requeue = false;

    // For each extension execute trunk install
    for ext in trunk_installs.iter() {
        let ext_span = span!(Level::INFO, "install_single_extension", extension_name = %ext.name);
        let _enter_ext = ext_span.enter();
        info!("Processing extension: {}", ext.name);
        let version = match ext.version.clone() {
            None => {
                let version_status =
                    handle_missing_version(ext, &coredb_name, &pod_name, &coredb_api).await?;
                current_trunk_install_statuses =
                    add_trunk_install_to_status(&coredb_api, &coredb_name, &version_status).await?;
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
        info!("Executing install command: {:?}", cmd);
        let result = cdb.exec(pod_name.clone(), client.clone(), &cmd).await;

        match result {
            Ok(result) => {
                let status = handle_install_result(
                    &coredb_api,
                    &coredb_name,
                    &result,
                    ext,
                    &pod_name,
                    &mut current_trunk_install_statuses,
                )
                .await?;
                local_statuses.push(status); // Push to the local vector
            }
            Err(err) => {
                error!(
                    "Kube exec error installing extension {} into {}: {}",
                    &ext.name, coredb_name, err
                );
                requeue = true;
            }
        }
    }

    // Deduplicate the local vector
    let deduped_local_statuses = deduplicate_trunk_install_statuses(local_statuses);

    // Merge deduped_local_statuses back into current_trunk_install_statuses
    for local_status in deduped_local_statuses {
        // Search for an existing status in current_trunk_install_statuses
        if let Some(existing_status) = current_trunk_install_statuses
            .iter_mut()
            .find(|s| s.name == local_status.name)
        {
            // Update the existing status
            existing_status.version = local_status.version.clone();
            existing_status.error = local_status.error;
            existing_status.error_message = local_status.error_message.clone();

            if let Some(ref mut existing_instances) = existing_status.installed_to_instances {
                if let Some(new_instances) = &local_status.installed_to_instances {
                    for instance in new_instances.iter() {
                        if !existing_instances.contains(instance) {
                            existing_instances.push(instance.clone());
                        }
                    }
                }
            } else {
                existing_status.installed_to_instances = local_status.installed_to_instances.clone();
            }
        } else {
            // Insert the new local_status into current_trunk_install_statuses
            current_trunk_install_statuses.push(local_status.clone());
        }
    }

    if requeue {
        warn!("Requeueing due to errors during installation");
        return Err(Action::requeue(Duration::from_secs(10)));
    }

    Ok(current_trunk_install_statuses)
}
