use crate::coredb_crd::{self as crd, CoreDBExtensionsLocations};
use crate::errors::ReconcilerError;
use log::LevelFilter;
use sqlx;
use sqlx::postgres::PgConnectOptions;
use sqlx::{ConnectOptions, FromRow};
use sqlx::{Pool, Postgres, Row};
use url::{ParseError, Url};

#[derive(Debug, FromRow)]
pub struct ExtRow {
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub schema: Option<String>,
}

// contains all the extensions in a database
pub struct DbExt {
    pub dbname: String,
    pub extensions: Vec<ExtRow>,
}

pub async fn get_databases(pool: &Pool<Postgres>) -> Result<Vec<String>, sqlx::Error> {
    let rows = sqlx::query("SELECT datname FROM pg_database WHERE datistemplate = false;")
        .map(|row: sqlx::postgres::PgRow| row.try_get(0))
        .fetch_all(pool)
        .await?;
    let databases: Vec<String> = rows.into_iter().map(|row| row.unwrap()).collect();
    Ok(databases)
}

/// lists extensions in a single database
pub async fn list_extensions(connection: &Pool<Postgres>) -> Result<Vec<ExtRow>, ReconcilerError> {
    let q = "
    select distinct
	t0.name as name,
	t0.installed as enabled,
	t1.version as version,
	t0.schema as schema
from (
	select 
		name,
		version,
		installed,
		coalesce(schema, 'public') as schema
	from pg_catalog.pg_available_extension_versions
) t0,
(
	select 
		name,
		COALESCE(installed_version, default_version) AS version, 
		comment 
	from pg_catalog.pg_available_extensions
) t1
where t0.name = t1.name
    ";
    let rows: Vec<ExtRow> = sqlx::query_as::<_, ExtRow>(q).fetch_all(connection).await?;
    Ok(rows)
}

// Configure connection options
pub fn conn_options(url: &str) -> Result<PgConnectOptions, ParseError> {
    // Parse url
    let parsed = Url::parse(url)?;
    let mut options = PgConnectOptions::new()
        .host(parsed.host_str().ok_or(ParseError::EmptyHost)?)
        .port(parsed.port().ok_or(ParseError::InvalidPort)?)
        .username(parsed.username())
        .password(parsed.password().ok_or(ParseError::IdnaError)?);
    options.log_statements(LevelFilter::Debug);
    Ok(options)
}

use std::collections::HashMap;

// wrangle the extensions in installed
// return as the crd / spec
pub async fn get_all_extensions(
    conn: &Pool<Postgres>,
) -> Result<Vec<crd::CoreDBExtensions>, ReconcilerError> {
    let databases = get_databases(conn).await?;

    let mut ext_hashmap: HashMap<String, Vec<CoreDBExtensionsLocations>> = HashMap::new();
    for db in databases {
        let extensions = list_extensions(conn).await?;
        for ext in extensions {
            let extlocation = CoreDBExtensionsLocations {
                database: Some(db.clone()),
                version: Some(ext.version),
                enabled: ext.enabled,
                schema: ext.schema,
            };
            ext_hashmap
                .entry(ext.name)
                .or_insert_with(Vec::new)
                .push(extlocation);
        }
    }

    let mut ext_spec: Vec<crd::CoreDBExtensions> = Vec::new();
    for (ext_name, ext_locations) in &ext_hashmap {
        ext_spec.push(crd::CoreDBExtensions {
            name: ext_name.clone(),
            locations: ext_locations.clone(),
        });
    }
    Ok(ext_spec)
}
