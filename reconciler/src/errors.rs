use thiserror::Error;

#[derive(Error, Debug)]
pub enum ReconcilerError {
    /// a json parsing error
    #[error("json parsing error {0}")]
    JsonParsingError(#[from] serde_json::error::Error),
}
