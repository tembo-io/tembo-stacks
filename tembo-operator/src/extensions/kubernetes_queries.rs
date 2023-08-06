use crate::{
    apis::coredb_types::{CoreDB, CoreDBStatus},
    extensions::types::{ExtensionInstallLocationStatus, ExtensionStatus, TrunkInstallStatus},
    get_current_coredb_resource, patch_cdb_status_merge, Context,
};
use kube::{runtime::controller::Action, Api};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tracing::{error, warn};

pub async fn update_extension_location_in_status(
    cdb: &CoreDB,
    ctx: Arc<Context>,
    extension_name: &str,
    new_location_status: &ExtensionInstallLocationStatus,
) -> Result<Vec<ExtensionStatus>, Action> {
    let cdb = get_current_coredb_resource(cdb, ctx.clone()).await?;
    let mut current_extensions_status = match &cdb.status {
        None => {
            error!("status should always already be present when merging one extension location into existing status");
            return Err(Action::requeue(Duration::from_secs(300)));
        }
        Some(status) => match &status.extensions {
            None => {
                error!("status.extensions should always already be present when merging one extension location into existing status");
                return Err(Action::requeue(Duration::from_secs(300)));
            }
            Some(extensions) => extensions.clone(),
        },
    };
    for extension in &mut current_extensions_status {
        if extension.name == extension_name {
            for location in &mut extension.locations {
                if location.database == new_location_status.database
                    && location.schema == new_location_status.schema
                {
                    *location = new_location_status.clone();
                    break;
                }
            }
            break;
        }
    }
    update_extensions_status(&cdb, current_extensions_status.clone(), &ctx).await?;
    Ok(current_extensions_status.clone())
}

pub async fn update_extensions_status(
    cdb: &CoreDB,
    ext_status_updates: Vec<ExtensionStatus>,
    ctx: &Arc<Context>,
) -> Result<(), Action> {
    let patch_status = json!({
        "apiVersion": "coredb.io/v1alpha1",
        "kind": "CoreDB",
        "status": {
            "extensions": ext_status_updates
        }
    });
    let coredb_api: Api<CoreDB> = Api::namespaced(
        ctx.client.clone(),
        &cdb.metadata
            .namespace
            .clone()
            .expect("CoreDB should have a namespace"),
    );
    patch_cdb_status_merge(
        &coredb_api,
        &cdb.metadata
            .name
            .clone()
            .expect("CoreDB should always have a name"),
        patch_status,
    )
    .await?;
    Ok(())
}

pub async fn remove_trunk_installs_from_status(
    cdb: &Api<CoreDB>,
    name: &str,
    trunk_install_names: Vec<String>,
) -> crate::Result<(), Action> {
    if trunk_install_names.is_empty() {
        return Ok(());
    }
    let current_coredb = cdb.get(name).await.map_err(|e| {
        error!("Error getting CoreDB: {:?}", e);
        Action::requeue(Duration::from_secs(10))
    })?;
    let current_status = match current_coredb.status {
        None => CoreDBStatus::default(),
        Some(status) => status,
    };
    let current_trunk_installs = match current_status.trunk_installs {
        None => {
            warn!(
                "No trunk installs in status on {}, but we are trying remove from status {:?}",
                name, trunk_install_names
            );
            return Ok(());
        }
        Some(trunk_installs) => trunk_installs,
    };
    let mut new_trunk_installs_status = current_trunk_installs.clone();

    // Remove the trunk installs from the status
    for trunk_install_name in trunk_install_names {
        new_trunk_installs_status.retain(|t| t.name != trunk_install_name);
    }

    // sort alphabetically by name
    new_trunk_installs_status.sort_by(|a, b| a.name.cmp(&b.name));
    // remove duplicates
    new_trunk_installs_status.dedup_by(|a, b| a.name == b.name);

    let new_status = CoreDBStatus {
        trunk_installs: Some(new_trunk_installs_status),
        ..current_status
    };
    let patch_status = json!({
        "apiVersion": "coredb.io/v1alpha1",
        "kind": "CoreDB",
        "status": new_status
    });
    patch_cdb_status_merge(cdb, name, patch_status).await?;
    Ok(())
}

pub async fn add_trunk_install_to_status(
    cdb: &Api<CoreDB>,
    name: &str,
    trunk_install: &TrunkInstallStatus,
) -> crate::Result<Vec<TrunkInstallStatus>, Action> {
    let current_coredb = cdb.get(name).await.map_err(|e| {
        error!("Error getting CoreDB: {:?}", e);
        Action::requeue(Duration::from_secs(10))
    })?;
    let current_status = match current_coredb.status {
        None => CoreDBStatus::default(),
        Some(status) => status,
    };
    let current_trunk_installs = match current_status.trunk_installs {
        None => {
            vec![]
        }
        Some(trunk_installs) => trunk_installs,
    };
    let mut new_trunk_installs_status = current_trunk_installs.clone();
    let mut trunk_install_found = false;
    // Check if the trunk install is already in the list
    for (_i, ti) in current_trunk_installs.iter().enumerate() {
        if ti.name == trunk_install.clone().name {
            warn!(
                "Trunk install {} already in status on {}, replacing.",
                &trunk_install.name, name
            );
            new_trunk_installs_status.push(trunk_install.clone());
            trunk_install_found = true;
        } else {
            new_trunk_installs_status.push(ti.clone());
        }
    }
    if !trunk_install_found {
        new_trunk_installs_status.push(trunk_install.clone());
    }
    // sort alphabetically by name
    new_trunk_installs_status.sort_by(|a, b| a.name.cmp(&b.name));
    // remove duplicates
    new_trunk_installs_status.dedup_by(|a, b| a.name == b.name);
    let new_status = CoreDBStatus {
        trunk_installs: Some(new_trunk_installs_status.clone()),
        ..current_status
    };
    let patch_status = json!({
        "apiVersion": "coredb.io/v1alpha1",
        "kind": "CoreDB",
        "status": new_status
    });
    patch_cdb_status_merge(cdb, name, patch_status).await?;
    Ok(new_trunk_installs_status.clone())
}
