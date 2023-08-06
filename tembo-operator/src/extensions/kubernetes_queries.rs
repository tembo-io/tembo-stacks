use crate::{
    apis::coredb_types::CoreDB,
    extensions::types::{ExtensionInstallLocationStatus, ExtensionStatus},
    get_current_coredb_resource, patch_cdb_status_merge, Context,
};
use kube::{runtime::controller::Action, Api};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tracing::error;

pub async fn update_extension_location_in_coredb_status(
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
    let patch_status = json!({
        "apiVersion": "coredb.io/v1alpha1",
        "kind": "CoreDB",
        "status": {
            "extensions": current_extensions_status
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
    Ok(current_extensions_status.clone())
}
