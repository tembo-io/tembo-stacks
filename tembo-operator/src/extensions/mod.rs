pub mod database_queries;
pub mod trunk;
pub mod types;

use crate::{
    apis::coredb_types::CoreDB, controller::patch_cdb_status_merge, get_current_coredb_resource, Context,
};

use kube::{api::Api, runtime::controller::Action};
use lazy_static::lazy_static;
use regex::Regex;


use serde_json::json;
use std::sync::Arc;

use tokio::time::Duration;
use tracing::error;

use types::{
    Extension, ExtensionInstallLocation, ExtensionInstallLocationStatus, ExtensionStatus, TrunkInstallStatus,
};


lazy_static! {
    static ref VALID_INPUT: Regex = Regex::new(r"^[a-zA-Z]([a-zA-Z0-9]*[-_]?)*[a-zA-Z0-9]+$").unwrap();
}

pub fn check_input(input: &str) -> bool {
    VALID_INPUT.is_match(input)
}

fn get_location_status(
    cdb: &CoreDB,
    extension_name: &str,
    location_database: &str,
    location_schema: &str,
) -> Option<ExtensionInstallLocationStatus> {
    match &cdb.status {
        None => None,
        Some(status) => match &status.extensions {
            None => None,
            Some(extensions) => {
                for extension in extensions {
                    if extension.name == extension_name {
                        for location in &extension.locations {
                            if location.database == location_database && location.schema == location_schema {
                                return Some(location.clone());
                            }
                        }
                        return None;
                    }
                }
                None
            }
        },
    }
}

fn get_location_spec(
    cdb: &CoreDB,
    extension_name: &str,
    location_database: &str,
    location_schema: &str,
) -> Option<ExtensionInstallLocation> {
    for extension in &cdb.spec.extensions {
        if extension.name == extension_name {
            for location in &extension.locations {
                if location.database == location_database && location.schema == location_schema {
                    return Some(location.clone());
                }
            }
            return None;
        }
    }
    None
}

fn determine_extension_locations_to_toggle(cdb: &CoreDB) -> Vec<Extension> {
    let mut extensions_to_toggle: Vec<Extension> = vec![];
    for desired_extension in &cdb.spec.extensions {
        let mut needs_toggle = false;
        let mut extension_to_toggle = desired_extension.clone();
        for desired_location in &desired_extension.locations {
            match get_location_status(
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
            match get_location_status(
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
            match get_location_spec(
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

async fn update_extension_location_in_coredb_status(
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

async fn create_or_drop_extensions(cdb: &CoreDB, ctx: Arc<Context>) -> Result<Vec<ExtensionStatus>, Action> {
    let all_actually_installed_extensions = database_queries::get_all_extensions(cdb, ctx.clone()).await?;
    let mut ext_status_updates = determine_updated_extensions_status(cdb, all_actually_installed_extensions);
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
    let cdb = get_current_coredb_resource(cdb, ctx.clone()).await?;
    let toggle_these_extensions = determine_extension_locations_to_toggle(&cdb);
    // TODO move this into separate function toggle_extensions
    for extension_to_toggle in toggle_these_extensions {
        for location_to_toggle in extension_to_toggle.locations {
            match database_queries::toggle_extension(
                &cdb,
                &extension_to_toggle.name,
                location_to_toggle.clone(),
                ctx.clone(),
            )
            .await
            {
                Ok(_) => {}
                Err(error_message) => {
                    let mut location_status = match get_location_status(
                        &cdb,
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
                    ext_status_updates = update_extension_location_in_coredb_status(
                        &cdb,
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

/// reconcile extensions between the spec and the database
pub async fn reconcile_extensions(
    coredb: &CoreDB,
    ctx: Arc<Context>,
    _cdb_api: &Api<CoreDB>,
    _name: &str,
) -> Result<(Vec<TrunkInstallStatus>, Vec<ExtensionStatus>), Action> {
    let trunk_installs = trunk::reconcile_trunk_installs(coredb, ctx.clone()).await?;
    let extension_statuses = create_or_drop_extensions(coredb, ctx.clone()).await?;
    Ok((trunk_installs, extension_statuses))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::database_queries::{parse_databases, parse_extensions};

    #[test]
    fn test_parse_databases() {
        let three_db = " datname  
        ----------
         postgres
         cat
         dog
        (3 rows)
        
         ";

        let rows = parse_databases(three_db);
        println!("{:?}", rows);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], "postgres");
        assert_eq!(rows[1], "cat");
        assert_eq!(rows[2], "dog");

        let one_db = " datname  
        ----------
         postgres
        (1 row)
        
         ";

        let rows = parse_databases(one_db);
        println!("{:?}", rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], "postgres");
    }

    #[test]
    fn test_parse_extensions() {
        let ext_psql = "        name        | version | enabled |   schema   |                              description                               
        --------------------+---------+---------+------------+------------------------------------------------------------------------
         adminpack          | 2.1     | f       | public     | administrative functions for PostgreSQL
         amcheck            | 1.3     | f       | public     | functions for verifying relation integrity
         autoinc            | 1.0     | f       | public     | functions for autoincrementing fields
         bloom              | 1.0     | f       | public     | bloom access method - signature file based index
         btree_gin          | 1.3     | f       | public     | support for indexing common datatypes in GIN
         btree_gist         | 1.7     | f       | public     | support for indexing common datatypes in GiST
         citext             | 1.6     | f       | public     | data type for case-insensitive character strings
         cube               | 1.5     | f       | public     | data type for multidimensional cubes
         dblink             | 1.2     | f       | public     | connect to other PostgreSQL databases from within a database
         (9 rows)";

        let ext = parse_extensions(ext_psql);
        assert_eq!(ext.len(), 9);
        assert_eq!(ext[0].name, "adminpack");
        assert!(!ext[0].enabled);
        assert_eq!(ext[0].version, "2.1".to_owned());
        assert_eq!(ext[0].schema, "public".to_owned());
        assert_eq!(
            ext[0].description,
            "administrative functions for PostgreSQL".to_owned()
        );

        assert_eq!(ext[8].name, "dblink");
        assert!(!ext[8].enabled);
        assert_eq!(ext[8].version, "1.2".to_owned());
        assert_eq!(ext[8].schema, "public".to_owned());
        assert_eq!(
            ext[8].description,
            "connect to other PostgreSQL databases from within a database".to_owned()
        );
    }

    #[test]
    fn test_check_input() {
        let invalids = ["extension--", "data;", "invalid^#$$characters", ";invalid", ""];
        for i in invalids.iter() {
            assert!(!check_input(i), "input {} should be invalid", i);
        }

        let valids = [
            "extension_a",
            "schema_abc",
            "extension",
            "NewExtension",
            "NewExtension123",
            "postgis_tiger_geocoder-3",
            "address_standardizer-3",
            "xml2",
        ];
        for i in valids.iter() {
            assert!(check_input(i), "input {} should be valid", i);
        }
    }
}
