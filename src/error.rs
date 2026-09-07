use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, RemnantError>;

#[derive(Debug, Error)]
pub enum RemnantError {
    #[error("configuration file {path} does not exist")]
    MissingConfig { path: PathBuf },

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("configuration could not be read: {0}")]
    ConfigRead(#[from] std::io::Error),

    #[error("configuration could not be parsed: {0}")]
    ConfigParse(String),

    #[error("operation is unsafe: {0}")]
    UnsafeOperation(String),

    #[error("unsupported operation: {0}")]
    Unsupported(String),

    #[error("oracle process could not be started: {0}")]
    OracleStart(#[source] std::io::Error),

    #[error("oracle process failed while collecting output: {0}")]
    OracleProcess(#[source] std::io::Error),
}
