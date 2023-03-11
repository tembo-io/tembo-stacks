use controller::controller::{Extension, ExtensionInstallLocation};

use crate::errors::ReconcilerError;
use controller::CoreDB;
use kube::Client;
use log::{debug, info, LevelFilter};
use sqlx;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, FromRow};
use sqlx::{Pool, Postgres, Row};
use std::collections::HashMap;
use url::{ParseError, Url};

#[derive(Debug, FromRow)]
pub struct ExtRow {
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub schema: String,
}

// contains all the extensions in a database
pub struct DbExt {
    pub dbname: String,
    pub extensions: Vec<ExtRow>,
}

const LIST_DATABASES_QUERY: &str =
    r#"SELECT datname FROM pg_database WHERE datistemplate = false;"#;
const LIST_EXTENSIONS_QUERY: &str = r#"select
distinct on
(name) *
from
(
select
    *
from
    (
    select
        t0.extname as name,
        t0.extversion as version,
        true as enabled,
        t1.nspname as schema
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
    ) t1
    where
        t1.oid = t0.extnamespace
) installed
union
select 
    name,
    default_version as version,
    false as enabled,
    'public' as schema
from
    pg_catalog.pg_available_extensions
order by
    enabled asc 
) combined
order by
name asc,
enabled desc
"#;

pub async fn get_databases(pool: &Pool<Postgres>) -> Result<Vec<String>, sqlx::Error> {
    let rows = sqlx::query(LIST_DATABASES_QUERY)
        .map(|row: sqlx::postgres::PgRow| row.try_get(0))
        .fetch_all(pool)
        .await?;
    let databases: Vec<String> = rows.into_iter().map(|row| row.unwrap()).collect();
    Ok(databases)
}

/// lists extensions in a single database
pub async fn list_extensions(connection: &Pool<Postgres>) -> Result<Vec<ExtRow>, ReconcilerError> {
    let rows: Vec<ExtRow> = sqlx::query_as::<_, ExtRow>(LIST_EXTENSIONS_QUERY)
        .fetch_all(connection)
        .await?;
    Ok(rows)
}

// wrangle the extensions in installed
// return as the crd / spec
pub async fn get_all_extensions(conn: &Pool<Postgres>) -> Result<Vec<Extension>, ReconcilerError> {
    let databases = get_databases(conn).await?;

    let mut ext_hashmap: HashMap<String, Vec<ExtensionInstallLocation>> = HashMap::new();
    for db in databases {
        let extensions = list_extensions(conn).await?;
        for ext in extensions {
            let extlocation = ExtensionInstallLocation {
                database: db.clone(),
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

    let mut ext_spec: Vec<Extension> = Vec::new();
    for (ext_name, ext_locations) in &ext_hashmap {
        ext_spec.push(Extension {
            name: ext_name.clone(),
            locations: ext_locations.clone(),
        });
    }
    Ok(ext_spec)
}

pub async fn exec_list_databases(
    cdb: &CoreDB,
    client: Client,
    database: &str,
) -> Result<Vec<String>, ReconcilerError> {
    let psql_out = cdb
        .psql(
            LIST_DATABASES_QUERY.to_owned(),
            database.to_owned(),
            client.clone(),
        )
        .await
        .unwrap();
    let result_string = psql_out.stdout.unwrap();
    let mut databases = vec![];
    for line in result_string.lines().skip(2) {
        let fields: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
        if fields.len() < 4 {
            debug!("Done:{:?}", fields);
            continue;
        }
        databases.push(fields[0].to_string());
    }
    let num_databases = databases.len();
    info!("Found {} databases", num_databases);
    Ok(databases)
}

pub async fn exec_list_extensions(
    cdb: &CoreDB,
    client: Client,
    database: &str,
) -> Result<Vec<ExtRow>, ReconcilerError> {
    let psql_out = cdb
        .psql(
            LIST_EXTENSIONS_QUERY.to_owned(),
            database.to_owned(),
            client.clone(),
        )
        .await
        .unwrap();
    let result_string = psql_out.stdout.unwrap();
    let mut extensions = vec![];
    for line in result_string.lines().skip(2) {
        let fields: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
        if fields.len() < 4 {
            debug!("Done:{:?}", fields);
            continue;
        }
        let package = ExtRow {
            name: fields[0].to_owned(),
            version: fields[1].to_owned(),
            enabled: fields[2] == "t",
            schema: fields[3].to_owned(),
        };
        extensions.push(package);
    }
    let num_extensions = extensions.len();
    info!("Found {} extensions", num_extensions);
    Ok(extensions)
}

// wrangle the extensions in installed
// return as the crd / spec
pub async fn exec_get_all_extensions(
    cdb: &CoreDB,
    client: Client,
    database: &str,
) -> Result<Vec<Extension>, ReconcilerError> {
    let databases = exec_list_databases(cdb, client.clone(), database).await?;

    let mut ext_hashmap: HashMap<String, Vec<ExtensionInstallLocation>> = HashMap::new();
    for db in databases {
        let extensions = exec_list_extensions(cdb, client.clone(), &db).await?;
        for ext in extensions {
            let extlocation = ExtensionInstallLocation {
                database: db.clone(),
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

    let mut ext_spec: Vec<Extension> = Vec::new();
    for (ext_name, ext_locations) in &ext_hashmap {
        ext_spec.push(Extension {
            name: ext_name.clone(),
            locations: ext_locations.clone(),
        });
    }
    Ok(ext_spec)
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

pub async fn connect(url: &str) -> Result<Pool<Postgres>, ReconcilerError> {
    let options = conn_options(url)?;
    let pgp = PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(10))
        .max_connections(5)
        .connect_with(options)
        .await?;
    Ok(pgp)
}
