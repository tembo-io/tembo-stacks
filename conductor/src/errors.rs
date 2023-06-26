use aws_sdk_cloudformation::Error as CFError;
use kube;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConductorError {
    /// a json parsing error
    #[error("json parsing error {0}")]
    JsonParsingError(#[from] serde_json::error::Error),

    /// a kube error
    #[error("kube error {0}")]
    KubeError(#[from] kube::Error),

    // No status reported
    #[error("no status reported")]
    NoStatusReported,

    /// a aws error
    #[error("aws sdk error {0}")]
    AwsError(#[from] CFError),

    // No outputs found for the stack
    #[error("no outputs found for the stack")]
    NoOutputsFound,

    #[error("Didn't find Postgres connection information")]
    PostgresConnectionInfoNotFound,

    #[error("Didn't find Postgres connection information")]
    ParsingPostgresConnectionError,
}
