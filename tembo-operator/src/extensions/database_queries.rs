use crate::{
    apis::coredb_types::CoreDB,
    extensions,
    extensions::types::{Extension, ExtensionInstallLocation},
    Context,
};
use kube::runtime::controller::Action;
use std::{collections::HashMap, sync::Arc};
use tracing::{debug, error, info, warn};

pub const LIST_DATABASES_QUERY: &str = r#"SELECT datname FROM pg_database WHERE datistemplate = false;"#;

pub const LIST_EXTENSIONS_QUERY: &str = r#"select
distinct on
(name) *
from
(
select
    name,
    version,
    enabled,
    schema,
    description
from
    (
    select
        t0.extname as name,
        t0.extversion as version,
        true as enabled,
        t1.nspname as schema,
        comment as description
    from
        (
        select
            extnamespace,
            extname,
            extversion
        from
            pg_extension
) t0,
        (
        select
            oid,
            nspname
        from
            pg_namespace
) t1,
        (
        select
            name,
            comment
        from
            pg_catalog.pg_available_extensions
) t2
    where
        t1.oid = t0.extnamespace
        and t2.name = t0.extname
) installed
union
select
    name,
    default_version as version,
    false as enabled,
    'public' as schema,
    comment as description
from
    pg_catalog.pg_available_extensions
order by
    enabled asc
) combined
order by
name asc,
enabled desc
"#;

#[derive(Debug)]
pub struct ExtRow {
    pub name: String,
    pub description: String,
    pub version: String,
    pub enabled: bool,
    pub schema: String,
}

/// lists all extensions in a single database
pub async fn list_extensions(cdb: &CoreDB, ctx: Arc<Context>, database: &str) -> Result<Vec<ExtRow>, Action> {
    let psql_out = cdb
        .psql(LIST_EXTENSIONS_QUERY.to_owned(), database.to_owned(), ctx)
        .await?;
    let result_string = psql_out.stdout.unwrap();
    Ok(parse_extensions(&result_string))
}

pub fn parse_extensions(psql_str: &str) -> Vec<ExtRow> {
    let mut extensions = vec![];
    for line in psql_str.lines().skip(2) {
        let fields: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
        if fields.len() < 5 {
            debug!("Done:{:?}", fields);
            continue;
        }
        let package = ExtRow {
            name: fields[0].to_owned(),
            version: fields[1].to_owned(),
            enabled: fields[2] == "t",
            schema: fields[3].to_owned(),
            description: fields[4].to_owned(),
        };
        extensions.push(package);
    }
    let num_extensions = extensions.len();
    debug!("Found {} extensions", num_extensions);
    extensions
}

/// returns all the databases in an instance
pub async fn list_databases(cdb: &CoreDB, ctx: Arc<Context>) -> Result<Vec<String>, Action> {
    let _client = ctx.client.clone();
    let psql_out = cdb
        .psql(LIST_DATABASES_QUERY.to_owned(), "postgres".to_owned(), ctx)
        .await?;
    let result_string = psql_out.stdout.unwrap();
    Ok(parse_databases(&result_string))
}

pub fn parse_databases(psql_str: &str) -> Vec<String> {
    let mut databases = vec![];
    for line in psql_str.lines().skip(2) {
        let fields: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
        if fields.is_empty()
            || fields[0].is_empty()
            || fields[0].contains("rows)")
            || fields[0].contains("row)")
        {
            debug!("Done:{:?}", fields);
            continue;
        }
        databases.push(fields[0].to_string());
    }
    let num_databases = databases.len();
    info!("Found {} databases", num_databases);
    databases
}

/// list databases then get all extensions from each database
pub async fn get_all_extensions(cdb: &CoreDB, ctx: Arc<Context>) -> Result<Vec<Extension>, Action> {
    let databases = list_databases(cdb, ctx.clone()).await?;
    debug!("databases: {:?}", databases);

    let mut ext_hashmap: HashMap<(String, String), Vec<ExtensionInstallLocation>> = HashMap::new();
    // query every database for extensions
    // transform results by extension name, rather than by database
    for db in databases {
        let extensions = list_extensions(cdb, ctx.clone(), &db).await?;
        for ext in extensions {
            let extlocation = ExtensionInstallLocation {
                database: db.clone(),
                version: Some(ext.version),
                enabled: ext.enabled,
                schema: ext.schema,
            };
            ext_hashmap
                .entry((ext.name, ext.description))
                .or_insert_with(Vec::new)
                .push(extlocation);
        }
    }

    let mut ext_spec: Vec<Extension> = Vec::new();
    for ((extname, extdescr), ext_locations) in &ext_hashmap {
        ext_spec.push(Extension {
            name: extname.clone(),
            description: Some(extdescr.clone()),
            locations: ext_locations.clone(),
        });
    }
    // put them in order
    ext_spec.sort_by_key(|e| e.name.clone());
    Ok(ext_spec)
}

/// Handles create/drop an extension location
/// On failure, returns an error message
pub async fn toggle_extension(
    cdb: &CoreDB,
    ext_name: &str,
    ext_loc: ExtensionInstallLocation,
    ctx: Arc<Context>,
) -> Result<(), String> {
    let coredb_name = cdb.metadata.name.clone().expect("CoreDB should have a name");
    if !extensions::check_input(ext_name) {
        warn!(
            "Extension is not formatted properly. Skipping operation. {}",
            &coredb_name
        );
        return Err("Extension name is not formatted properly".to_string());
    }
    let database_name = ext_loc.database.to_owned();
    if !extensions::check_input(&database_name) {
        warn!(
            "Database name is not formatted properly. Skipping operation. {}",
            &coredb_name
        );
        return Err("Database name is not formatted properly".to_string());
    }
    let schema_name = ext_loc.schema.to_owned();
    if !extensions::check_input(&schema_name) {
        warn!(
            "Extension.Database.Schema {}.{}.{} is not formatted properly. Skipping operation. {}",
            ext_name, database_name, schema_name, &coredb_name
        );
        return Err("Schema name is not formatted properly".to_string());
    }
    let command = match ext_loc.enabled {
        true => {
            info!(
                "Creating extension: {}, database {}, instance {}",
                ext_name, database_name, &coredb_name
            );

            format!(
                "CREATE EXTENSION IF NOT EXISTS \"{}\" SCHEMA {} CASCADE;",
                ext_name, schema_name
            )
        }
        false => {
            info!(
                "Dropping extension: {}, database {}, instance {}",
                ext_name, database_name, &coredb_name
            );
            format!("DROP EXTENSION IF EXISTS \"{}\" CASCADE;", ext_name)
        }
    };

    let result = cdb
        .psql(command.clone(), database_name.clone(), ctx.clone())
        .await;

    match result {
        Ok(psql_output) => match psql_output.success {
            true => {
                info!(
                    "Successfully toggled extension {} in database {}, instance {}",
                    ext_name, database_name, &coredb_name
                );
            }
            false => {
                warn!(
                    "Failed to toggle extension {} in database {}, instance {}",
                    ext_name, database_name, &coredb_name
                );
                match psql_output.stdout {
                    Some(stdout) => {
                        return Err(stdout);
                    }
                    None => {
                        return Err("Failed to enable extension, and found no output. Please try again. If this issue persists, contact support.".to_string());
                    }
                }
            }
        },
        Err(e) => {
            error!(
                "Failed to reconcile extension because of kube exec error: {:?}",
                e
            );
            return Err(
                "Could not connect to database, try again. If problem persists, please contact support."
                    .to_string(),
            );
        }
    }
    Ok(())
}
