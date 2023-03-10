use thiserror::Error;
use url::ParseError;

#[derive(Error, Debug)]
pub enum ReconcilerError {
    /// a json parsing error
    #[error("json parsing error {0}")]
    JsonParsingError(#[from] serde_json::error::Error),

    /// a database error
    #[error("database error {0}")]
    DatabaseError(#[from] sqlx::Error),

    /// a url parsing error
    #[error("url parsing error {0}")]
    UrlParsingError(#[from] ParseError),
}
