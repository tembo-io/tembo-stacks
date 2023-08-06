use crate::defaults;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};


#[derive(Clone, Debug, Deserialize, Eq, Hash, JsonSchema, Serialize, PartialEq)]
pub struct TrunkInstall {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, JsonSchema, Serialize, PartialEq)]
pub struct TrunkInstallStatus {
    pub name: String,
    pub version: Option<String>,
    pub status: InstallStatus,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, JsonSchema, Serialize, PartialEq)]
pub enum InstallStatus {
    Installed,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, JsonSchema, Serialize, PartialEq)]
pub struct Extension {
    pub name: String,
    #[serde(default = "defaults::default_description")]
    pub description: Option<String>,
    pub locations: Vec<ExtensionInstallLocation>,
}

impl Default for Extension {
    fn default() -> Self {
        Extension {
            name: "pg_stat_statements".to_owned(),
            description: Some(
                " track planning and execution statistics of all SQL statements executed".to_owned(),
            ),
            locations: vec![ExtensionInstallLocation::default()],
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, JsonSchema, Serialize, PartialEq)]
pub struct ExtensionInstallLocation {
    pub enabled: bool,
    // no database or schema when disabled
    #[serde(default = "defaults::default_database")]
    pub database: String,
    #[serde(default = "defaults::default_schema")]
    pub schema: String,
    pub version: Option<String>,
}

impl Default for ExtensionInstallLocation {
    fn default() -> Self {
        ExtensionInstallLocation {
            schema: "public".to_owned(),
            database: "postgres".to_owned(),
            enabled: true,
            version: Some("1.9".to_owned()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, JsonSchema, Serialize, PartialEq)]
pub struct ExtensionStatus {
    pub name: String,
    #[serde(default = "defaults::default_description")]
    pub description: Option<String>,
    pub locations: Vec<ExtensionInstallLocationStatus>,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, JsonSchema, Serialize, PartialEq)]
pub struct ExtensionInstallLocationStatus {
    #[serde(default = "defaults::default_database")]
    pub database: String,
    #[serde(default = "defaults::default_schema")]
    pub schema: String,
    pub version: Option<String>,
    // None means this is not actually installed
    pub enabled: Option<bool>,
    pub error: bool,
    pub error_message: Option<String>,
}
