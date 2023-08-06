use crate::{
    apis::coredb_types::CoreDB,
    extensions::{
        database_queries, kubernetes_queries, types,
        types::{Extension, ExtensionInstallLocationStatus, ExtensionStatus},
    },
    get_current_coredb_resource, Context,
};
use kube::runtime::controller::Action;

use std::sync::Arc;
use tracing::error;

pub async fn reconcile_extension_toggle_state(
    cdb: &CoreDB,
    ctx: Arc<Context>,
) -> Result<Vec<ExtensionStatus>, Action> {
    let all_actually_installed_extensions = database_queries::get_all_extensions(cdb, ctx.clone()).await?;
    let ext_status_updates = determine_updated_extensions_status(cdb, all_actually_installed_extensions);
    kubernetes_queries::update_extensions_status(cdb, ext_status_updates.clone(), &ctx).await?;
    let cdb = get_current_coredb_resource(cdb, ctx.clone()).await?;
    let toggle_these_extensions = determine_extension_locations_to_toggle(&cdb);
    let ext_status_updates =
        toggle_extensions(ctx, ext_status_updates, &cdb, toggle_these_extensions).await?;
    Ok(ext_status_updates)
}

async fn toggle_extensions(
    ctx: Arc<Context>,
    ext_status_updates: Vec<ExtensionStatus>,
    cdb: &CoreDB,
    toggle_these_extensions: Vec<Extension>,
) -> Result<Vec<ExtensionStatus>, Action> {
    let mut ext_status_updates = ext_status_updates.clone();
    for extension_to_toggle in toggle_these_extensions {
        for location_to_toggle in extension_to_toggle.locations {
            match database_queries::toggle_extension(
                cdb,
                &extension_to_toggle.name,
                location_to_toggle.clone(),
                ctx.clone(),
            )
            .await
            {
                Ok(_) => {}
                Err(error_message) => {
                    let mut location_status = match types::get_location_status(
                        cdb,
                        &extension_to_toggle.name,
                        &location_to_toggle.database,
                        &location_to_toggle.schema,
                    ) {
                        None => {
                            error!("There should always be an extension status for a location before attempting to toggle an extension for that location");
                            ExtensionInstallLocationStatus {
                                database: location_to_toggle.database.clone(),
                                schema: location_to_toggle.schema.clone(),
                                version: None,
                                enabled: None,
                                error: true,
                                error_message: None,
                            }
                        }
                        Some(location_status) => location_status,
                    };
                    location_status.error = true;
                    location_status.error_message = Some(error_message);
                    ext_status_updates = kubernetes_queries::update_extension_location_in_status(
                        cdb,
                        ctx.clone(),
                        &extension_to_toggle.name,
                        &location_status,
                    )
                    .await?;
                }
            }
        }
    }
    Ok(ext_status_updates)
}

fn determine_updated_extensions_status(
    cdb: &CoreDB,
    all_actually_installed_extensions: Vec<Extension>,
) -> Vec<ExtensionStatus> {
    // Our results - what we will update the status to
    let mut ext_status_updates: Vec<ExtensionStatus> = vec![];
    // For every actually installed extension
    for actual_extension in all_actually_installed_extensions {
        let mut extension_status = ExtensionStatus {
            name: actual_extension.name.clone(),
            description: actual_extension.description.clone(),
            locations: vec![],
        };
        // For every location of an actually installed extension
        for actual_location in actual_extension.locations {
            // Create a location status
            let mut location_status = ExtensionInstallLocationStatus {
                enabled: Some(actual_location.enabled),
                database: actual_location.database.clone(),
                schema: actual_location.schema.clone(),
                version: actual_location.version.clone(),
                error: false,
                error_message: None,
            };
            // If there is a current status, retain the error and error message
            match types::get_location_status(
                cdb,
                &actual_extension.name.clone(),
                &actual_location.database.clone(),
                &actual_location.schema.clone(),
            ) {
                None => {}
                Some(current_status) => {
                    location_status.error = current_status.error;
                    location_status.error_message = current_status.error_message;
                }
            }
            // If the desired state matches the actual state, unset the error and error message
            match types::get_location_spec(
                cdb,
                &actual_extension.name,
                &actual_location.database,
                &actual_location.schema,
            ) {
                None => {}
                Some(desired_location) => {
                    if actual_location.enabled == desired_location.enabled {
                        location_status.error = false;
                        location_status.error_message = None;
                    }
                }
            }
            extension_status.locations.push(location_status);
        }
        // sort locations by database and schema so the order is deterministic
        extension_status
            .locations
            .sort_by(|a, b| a.database.cmp(&b.database).then(a.schema.cmp(&b.schema)));
        ext_status_updates.push(extension_status);
    }
    // We also want to include unavailable extensions if they are being requested
    for desired_extension in &cdb.spec.extensions {
        // If this extension is not in ext_status_updates
        if !ext_status_updates
            .iter()
            .any(|e| e.name == desired_extension.name)
        {
            // Create a new extension status
            let mut extension_status = ExtensionStatus {
                name: desired_extension.name.clone(),
                description: desired_extension.description.clone(),
                locations: vec![],
            };
            // For every location of the desired extension
            for desired_location in &desired_extension.locations {
                if desired_location.enabled {
                    let location_status = ExtensionInstallLocationStatus {
                        enabled: None,
                        database: desired_location.database.clone(),
                        schema: desired_location.schema.clone(),
                        version: desired_location.version.clone(),
                        error: true,
                        error_message: Some("Extension is not installed".to_string()),
                    };
                    extension_status.locations.push(location_status);
                }
            }
            // sort locations by database and schema so the order is deterministic
            extension_status
                .locations
                .sort_by(|a, b| a.database.cmp(&b.database).then(a.schema.cmp(&b.schema)));
            // If there are more than zero locations, add the extension status
            if !extension_status.locations.is_empty() {
                ext_status_updates.push(extension_status);
            }
        }
    }
    // sort by extension name so the order is deterministic
    ext_status_updates.sort_by(|a, b| a.name.cmp(&b.name));
    ext_status_updates
}

fn determine_extension_locations_to_toggle(cdb: &CoreDB) -> Vec<Extension> {
    let mut extensions_to_toggle: Vec<Extension> = vec![];
    for desired_extension in &cdb.spec.extensions {
        let mut needs_toggle = false;
        let mut extension_to_toggle = desired_extension.clone();
        for desired_location in &desired_extension.locations {
            match types::get_location_status(
                cdb,
                &desired_extension.name,
                &desired_location.database,
                &desired_location.schema,
            ) {
                None => {
                    error!("When determining extensions to toggle, there should always be a location status for the desired location, because that should be included by determine_updated_extensions_status.");
                }
                Some(actual_status) => {
                    // If we don't have an error already, the extension exists, and the desired does not match the actual
                    if !actual_status.error
                        && actual_status.enabled.is_some()
                        && actual_status.enabled.unwrap() != desired_location.enabled
                    {
                        needs_toggle = true;
                        extension_to_toggle.locations.push(desired_location.clone());
                    }
                }
            }
        }
        if needs_toggle {
            extensions_to_toggle.push(extension_to_toggle);
        }
    }
    extensions_to_toggle
}
