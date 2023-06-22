use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeSet;


// these values are multi-valued, and need to be merged across configuration layers
pub const MULTI_VAL_CONFIGS: [&str; 5] = [
    "shared_preload_libraries",
    "local_preload_libraries",
    "session_preload_libraries",
    "log_destination",
    "search_path",
];

// defines the postgresql configuration
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct PgConfig {
    pub name: String,
    #[serde(deserialize_with = "custom_deserialize_config_value")]
    pub value: ConfigValue,
}

impl PgConfig {
    // converts the configuration to the postgres format
    pub fn to_postgres(&self) -> String {
        format!("{} = '{}'", self.name, self.value)
    }
}

use thiserror::Error;

#[derive(Error, Debug)]
pub enum MergeError {
    #[error("SingleValError")]
    SingleValueNotAllowed,
}

use tracing::*;

impl ConfigValue {
    fn combine(self, other: Self) -> Result<Self, MergeError> {
        match (self, other) {
            (ConfigValue::Single(_), _) | (_, ConfigValue::Single(_)) => {
                Err(MergeError::SingleValueNotAllowed)
            }
            (ConfigValue::Multiple(set1), ConfigValue::Multiple(set2)) => {
                let set = set1.union(&set2).cloned().collect();
                Ok(ConfigValue::Multiple(set))
            }
        }
    }
}


pub fn merge_pg_configs(
    vec1: &Vec<PgConfig>,
    vec2: &Vec<PgConfig>,
    name: &str,
) -> Result<Option<PgConfig>, MergeError> {
    let config1 = vec1.clone().into_iter().find(|config| config.name == name);
    let config2 = vec2.clone().into_iter().find(|config| config.name == name);
    match (config1, config2) {
        (Some(mut c1), Some(c2)) => match c1.value.combine(c2.value) {
            Ok(combined_value) => {
                c1.value = combined_value;
                Ok(Some(c1))
            }
            Err(e) => Err(e),
        },
        (Some(c), None) | (None, Some(c)) => {
            info!("No configs to merge");
            Ok(Some(c))
        }
        (None, None) => Ok(None),
    }
}


#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigValue {
    Single(String),
    Multiple(BTreeSet<String>),
}

use schemars::schema::{Schema, SchemaObject};


impl JsonSchema for ConfigValue {
    fn schema_name() -> String {
        "ConfigValue".to_string()
    }

    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        let mut schema_object = SchemaObject::default();
        schema_object.metadata().description = Some("A postgresql.conf configuration value".to_owned());
        schema_object.metadata().read_only = false;
        schema_object.instance_type = Some(schemars::schema::InstanceType::String.into());
        Schema::Object(schema_object)
    }
}

impl std::fmt::Display for ConfigValue {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ConfigValue::Single(value) => write!(f, "{}", value),
            ConfigValue::Multiple(values) => {
                let joined_values = values.iter().cloned().collect::<Vec<String>>().join(",");
                write!(f, "{}", joined_values)
            }
        }
    }
}

use std::str::FromStr;


impl FromStr for ConfigValue {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.contains(",") {
            Ok(ConfigValue::Multiple(
                s.split(",").map(|s| s.to_string()).collect(),
            ))
        } else {
            Ok(ConfigValue::Single(s.to_string()))
        }
    }
}

// used for BTreeSet.join
use itertools::Itertools;

impl Serialize for ConfigValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            ConfigValue::Single(val) => serializer.serialize_str(val),
            ConfigValue::Multiple(set) => {
                let joined = set.into_iter().join(",");
                serializer.serialize_str(&joined)
            }
        }
    }
}


use serde::de::{self, MapAccess, Visitor};

struct PgConfigVisitor;

impl<'de> Visitor<'de> for PgConfigVisitor {
    type Value = PgConfig;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("struct PgConfig")
    }

    fn visit_map<A>(self, mut map: A) -> Result<PgConfig, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut name: Option<String> = None;
        let mut value: Option<String> = None;
        while let Some(key) = map.next_key()? {
            match key {
                "name" => {
                    if name.is_some() {
                        return Err(de::Error::duplicate_field("name"));
                    }
                    name = Some(map.next_value()?);
                }
                "value" => {
                    if value.is_some() {
                        return Err(de::Error::duplicate_field("value"));
                    }
                    value = Some(map.next_value()?);
                }
                _ => return Err(de::Error::unknown_field(key, &["name", "value"])),
            }
        }
        let name = name.ok_or_else(|| de::Error::missing_field("name"))?;
        let value_string = value.ok_or_else(|| de::Error::missing_field("value"))?;

        let value = if MULTI_VAL_CONFIGS.contains(&name.as_ref()) {
            let mut set = BTreeSet::new();
            set.insert(value_string);
            ConfigValue::Multiple(set)
        } else {
            ConfigValue::Single(value_string)
        };

        Ok(PgConfig { name, value })
    }
}

// custom deserialize to handle converting specific config values to a multi value
// configs in global MULTI_VAL_CONFIGS all become ConfigValue::Multiple
fn custom_deserialize_config_value<'de, D>(deserializer: D) -> Result<ConfigValue, D::Error>
where
    D: Deserializer<'de>,
{
    let value_string = String::deserialize(deserializer)?;
    let value = if MULTI_VAL_CONFIGS.contains(&value_string.as_ref()) {
        let mut set = BTreeSet::new();
        set.insert(value_string);
        ConfigValue::Multiple(set)
    } else {
        ConfigValue::Single(value_string)
    };

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apis::coredb_types::{CoreDBSpec, Stack};


    #[test]
    fn test_pg_config() {
        let pg_config = PgConfig {
            name: "max_parallel_workers".to_string(),
            value: "32".parse().unwrap(),
        };
        assert_eq!(pg_config.to_postgres(), "max_parallel_workers = '32'");
        let pg_config_multi = PgConfig {
            name: "shared_preload_libraries".to_string(),
            value: "pg_cron,pg_stat_statements".parse().unwrap(),
        };
        assert_eq!(
            pg_config_multi.to_postgres(),
            "shared_preload_libraries = 'pg_cron,pg_stat_statements'"
        );
    }

    #[test]
    fn test_get_configs() {
        let spec = CoreDBSpec {
            runtime_config: Some(vec![PgConfig {
                name: "shared_buffers".to_string(),
                value: "0.5GB".parse().unwrap(),
            }]),
            stack: Some(Stack {
                name: "tembo".to_string(),
                postgres_config: Some(vec![
                    PgConfig {
                        name: "pg_stat_statements.track".to_string(),
                        value: "all".parse().unwrap(),
                    },
                    PgConfig {
                        name: "shared_preload_libraries".to_string(),
                        value: "pg_cron,pg_stat_statements".parse().unwrap(),
                    },
                ]),
            }),
            ..Default::default()
        };
        let pg_configs = spec
            .get_pg_configs()
            .expect("failed to get pg configs")
            .expect("expected configs");
        assert_eq!(pg_configs.len(), 3);
        assert_eq!(pg_configs[0].name, "pg_stat_statements.track");
        assert_eq!(pg_configs[0].value.to_string(), "all");
        assert_eq!(pg_configs[1].name, "shared_preload_libraries");
        assert_eq!(pg_configs[1].value.to_string(), "pg_cron,pg_stat_statements");
        assert_eq!(pg_configs[2].name, "shared_buffers");
        assert_eq!(pg_configs[2].value.to_string(), "0.5GB");
    }

    #[test]
    fn test_alpha_order_multiple() {
        // assert ordering of multi values is always alpha
        let pgc = PgConfig {
            name: "test_configuration".to_string(),
            value: "a,b,c".parse().unwrap(),
        };
        assert_eq!(pgc.to_postgres(), "test_configuration = 'a,b,c'");
        let pgc = PgConfig {
            name: "test_configuration".to_string(),
            value: "a,z,c".parse().unwrap(),
        };
        assert_eq!(pgc.to_postgres(), "test_configuration = 'a,c,z'");
        let pgc = PgConfig {
            name: "test_configuration".to_string(),
            value: "z,y,x".parse().unwrap(),
        };
        assert_eq!(pgc.to_postgres(), "test_configuration = 'x,y,z'");
    }

    #[test]
    fn test_merge_pg_configs() {
        let pgc_0 = PgConfig {
            name: "test_configuration".to_string(),
            value: "a,b,c".parse().unwrap(),
        };
        let pgc_1 = PgConfig {
            name: "test_configuration".to_string(),
            value: "x,y,z".parse().unwrap(),
        };

        let merged = merge_pg_configs(&vec![pgc_0.clone()], &vec![pgc_1], "test_configuration")
            .expect("failed to merge pg configs")
            .expect("expected configs");
        assert_eq!(merged.value.to_string(), "a,b,c,x,y,z");

        // Single value should not be allowed to be merged
        let pgc_0 = PgConfig {
            name: "test_configuration".to_string(),
            value: "a".parse().unwrap(),
        };
        let pgc_1 = PgConfig {
            name: "test_configuration".to_string(),
            value: "b".parse().unwrap(),
        };
        let merged = merge_pg_configs(&vec![pgc_0], &vec![pgc_1], "test_configuration");
        assert!(merged.is_err())
    }
}
