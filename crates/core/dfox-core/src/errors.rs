use thiserror::Error;

/// Custom error type for database operations.
#[derive(Error, Debug)]
pub enum DbError {
    #[error("Import error: {0}")]
    Import(String),
    #[error("Export error: {0}")]
    Export(String),
    /// Configuration error (e.g., invalid database URL or missing parameters).
    #[error("Configuration error: {0}")]
    Config(String),
    /// Connection error (e.g., issues with network or database connection).
    #[error("Connection error: {0}")]
    Connection(String),
    /// The query was cancelled before it finished.
    #[error("Query cancelled")]
    Cancelled,
    /// General error with a custom message.
    #[error("Error: {0}")]
    General(String),
}
