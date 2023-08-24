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
use std::{sync::Arc, time::Duration};
use tracing::{debug, error, info, instrument};

pub async fn reconcile_trunk_installs(
    cdb: &CoreDB,
    ctx: Arc<Context>,
) -> Result<Vec<TrunkInstallStatus>, Action> {
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

    // Remove extensions from status
    remove_trunk_installs_from_status(
        &coredb_api,
        &cdb.metadata.name.clone().expect("CoreDB should have a name"),
        trunk_install_names_to_remove_from_status,
    )
    .await?;

    // Get extensions in spec.trunk_install that are not in status.trunk_install
    let trunk_installs = cdb
        .spec
        .trunk_installs
        .iter()
        .filter(|&ext| {
            !cdb.status
                .clone()
                .unwrap_or_default()
                .trunk_installs
                .unwrap_or_default()
                .iter()
                .any(|ext_status| ext.name == ext_status.name)
        })
        .collect::<Vec<_>>();

    let mut all_results = Vec::new();
    for pod in all_pods {
        let pod_name = pod.metadata.name.expect("Pod should always have a name");

        // Install missing extensions
        match install_extensions(cdb, trunk_installs.clone(), &ctx, pod_name).await {
            Ok(result) => all_results.extend(result),
            Err(err) => return Err(err), // or handle error differently
        };
    }

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
        installed_to_instances: vec![pod_name.to_string()],
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

    if result.success {
        info!("Installed extension {} into {}", &ext.name, coredb_name);
        debug!("{}", output);
        let trunk_install_status = TrunkInstallStatus {
            name: ext.name.clone(),
            version: ext.version.clone(),
            error: false,
            error_message: None,
            installed_to_instances: vec![pod_name.to_string()],
        };
        add_trunk_install_to_status(coredb_api, coredb_name, &trunk_install_status).await?;
        Ok(trunk_install_status)
    } else {
        error!(
            "Failed to install extension {} into {}:\n{}",
            &ext.name, coredb_name, output
        );
        let trunk_install_status = TrunkInstallStatus {
            name: ext.name.clone(),
            version: ext.version.clone(),
            error: true,
            error_message: Some(output),
            installed_to_instances: vec![pod_name.to_string()],
        };
        add_trunk_install_to_status(coredb_api, coredb_name, &trunk_install_status).await?;
        Ok(trunk_install_status)
    }
}

/// handles installing extensions
#[instrument(skip(ctx, cdb))]
pub async fn install_extensions(
    cdb: &CoreDB,
    trunk_installs: Vec<&TrunkInstall>,
    ctx: &Arc<Context>,
    pod_name: String,
) -> Result<Vec<TrunkInstallStatus>, Action> {
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
        let result = cdb.exec(pod_name.clone(), client.clone(), &cmd).await;

        match result {
            Ok(result) => {
                let status =
                    handle_install_result(&coredb_api, &coredb_name, &result, ext, &pod_name).await?;
                current_trunk_install_statuses.push(status);
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
        return Err(Action::requeue(Duration::from_secs(10)));
    }
    Ok(current_trunk_install_statuses)
}
