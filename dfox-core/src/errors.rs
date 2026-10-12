use thiserror::Error;

/// Custom error type for database operations.
#[derive(Error, Debug)]
pub enum DbError {
    /// Error that occurs during database interactions (e.g., SQL query failure).
    #[error("Database error: {0}")]
    Sqlx(#[from] sqlx::Error), // Converts sqlx::Error to DbError::SqlxError.
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
    /// General error with a custom message.
    #[error("Error: {0}")]
    General(String),
}
