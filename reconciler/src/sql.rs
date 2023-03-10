use sqlx;
use crate::errors::{ReconcilerError};
use log::LevelFilter;
use serde::{Deserialize, Serialize};
use sqlx::error::Error;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgRow};
use sqlx::types::chrono::Utc;
use sqlx::{ConnectOptions, FromRow};
use sqlx::{Pool, Postgres, Row};
use crate::coredb_crd::{self as crd, CoreDBExtensionsLocations};
use url::{ParseError, Url};

use sqlx::query;

use sqlx::{postgres::PgPool};

#[derive(Debug, FromRow)]
struct ExtRow {
    name: String,
    version: String,
    enabled: bool,
    schema: Option<String>,
}

pub async fn get_databases(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    let rows = sqlx::query("SELECT datname FROM pg_database WHERE datistemplate = false;")
        .map(|row: sqlx::postgres::PgRow| row.try_get(0))
        .fetch_all(pool)
        .await?;

    // unwrap the Option<String> results from try_get and collect them into a Vec<String>
    let strings: Vec<String> = rows.into_iter().map(|row| row.unwrap()).collect();
    println!("databases: {:?}", strings);
    Ok(strings)
}


pub async fn get_extensions(
    connection: &Pool<Postgres>,
) -> Result<Option<Vec<CoreDBExtensionsLocations>>, ReconcilerError> {
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
    let rows = sqlx::query_as::<_, ExtRow>(q).fetch_all(connection).await;
    println!("rows: {:?}", rows);
    let mut extensions: Vec<CoreDBExtensionsLocations> = Vec::new();
    if let Err(sqlx::error::Error::RowNotFound) = rows {
        return Ok(None);
    } else if let Err(e) = rows {
        return Err(e)?;
    } else if let Ok(rows) = rows {
        // happy path - successfully read extensions
        for row in rows {
            let ext = CoreDBExtensionsLocations {
                enabled: row.enabled,
                version: Some(row.version),
                schema: Some(row.schema.unwrap()),
                database: Some("postgres".to_owned()),
            };
            extensions.push(ext);
        }
    }
    Ok(Some(extensions))
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
